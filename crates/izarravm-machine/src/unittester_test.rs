// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_core::{
    CanonicalSectionId, CanonicalSectionRequirement, CanonicalSectionVersion, CanonicalStateView,
    CanonicalStateWriter,
};

const UNIT_TESTER_PAYLOAD_LEN: usize = 33;

fn canonical_payload(device: &UnitTester) -> Vec<u8> {
    let projection = device.canonical_projection();
    let mut state = CanonicalStateWriter::new().unwrap();
    state
        .section(
            CanonicalSectionId::new(0x0002_0008).unwrap(),
            CanonicalSectionVersion::new(1).unwrap(),
            CanonicalSectionRequirement::Required,
            |out| projection.write_payload(out),
        )
        .unwrap();
    let bytes = state.finish().unwrap();
    let view = CanonicalStateView::parse(&bytes).unwrap();
    view.sections()[0].payload().to_vec()
}

fn assert_only_offset_changed(before: &[u8], after: &[u8], expected: usize) {
    let actual: Vec<_> = before
        .iter()
        .zip(after)
        .enumerate()
        .filter_map(|(offset, (left, right))| (left != right).then_some(offset))
        .collect();
    assert_eq!(actual, [expected]);
}

#[test]
fn canonical_payload_layout_is_exact() {
    let mut device = UnitTester::default();
    for offset in 0..REG_FILE_SIZE {
        device.set_reg_u8(offset, (offset as u8).wrapping_mul(7).wrapping_add(3));
    }
    device.write_port(PORT_INDEX, 0xff);

    let mut expected = vec![0xff];
    expected
        .extend((0..REG_FILE_SIZE).map(|offset| (offset as u8).wrapping_mul(7).wrapping_add(3)));

    let payload = canonical_payload(&device);
    assert_eq!(payload.len(), UNIT_TESTER_PAYLOAD_LEN);
    assert_eq!(payload, expected);
}

#[test]
fn canonical_payload_pins_every_offset() {
    let baseline = UnitTester::default();
    let expected = canonical_payload(&baseline);

    let mut changed = baseline.clone();
    changed.write_port(PORT_INDEX, 0xff);
    assert_only_offset_changed(&expected, &canonical_payload(&changed), 0);

    for offset in 0..REG_FILE_SIZE {
        let mut changed = baseline.clone();
        changed.set_reg_u8(offset, 0x80 | offset as u8);
        assert_only_offset_changed(&expected, &canonical_payload(&changed), offset + 1);
    }
}

#[test]
fn high_index_read_and_write_keep_their_exact_continuations() {
    let mut first_out_of_range = UnitTester::default();
    first_out_of_range.write_port(PORT_INDEX, REG_FILE_SIZE as u8);
    assert_eq!(first_out_of_range.read_port(PORT_DATA), Some(0));
    assert_eq!(first_out_of_range.read_port(PORT_INDEX), Some(1));

    let mut read = UnitTester::default();
    read.set_reg_u8(0, 0x5a);
    read.write_port(PORT_INDEX, 0xff);
    assert_eq!(canonical_payload(&read)[0], 0xff);
    assert_eq!(read.read_port(PORT_DATA), Some(0));
    assert_eq!(read.read_port(PORT_INDEX), Some(0));
    assert_eq!(read.reg_u8(0), 0x5a);

    let mut write = UnitTester::default();
    write.set_reg_u8(0, 0x5a);
    write.write_port(PORT_INDEX, 0xff);
    assert!(write.write_port(PORT_DATA, 0xa5));
    assert_eq!(write.read_port(PORT_INDEX), Some(0));
    assert_eq!(write.reg_u8(0), 0x5a);
}

#[test]
fn canonical_serialization_is_read_only_across_an_armed_data_read() {
    let mut device = UnitTester::default();
    device.set_reg_u8(REG_W, 0x40);
    device.write_port(PORT_INDEX, REG_W as u8);

    let first = canonical_payload(&device);
    let second = canonical_payload(&device);
    assert_eq!(first, second);
    assert_eq!(first[0], REG_W as u8);

    assert_eq!(device.read_port(PORT_DATA), Some(0x40));
    let advanced = canonical_payload(&device);
    assert_eq!(advanced[0], REG_W as u8 + 1);
    assert_eq!(&advanced[1..], &first[1..]);
}

#[test]
fn crc32_matches_the_zlib_check_value() {
    // The canonical CRC-32 check: "123456789" -> 0xCBF43926.
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    assert_eq!(crc32(b""), 0);
}

#[test]
fn data_register_round_trips_and_auto_increments() {
    let mut ut = UnitTester::default();
    ut.write_port(PORT_INDEX, REG_W as u8);
    ut.write_port(PORT_DATA, 0x40); // W low
    ut.write_port(PORT_DATA, 0x01); // W high -> 0x0140 = 320
    ut.write_port(PORT_INDEX, REG_W as u8);
    assert_eq!(ut.read_port(PORT_DATA), Some(0x40));
    assert_eq!(ut.read_port(PORT_DATA), Some(0x01));
    let (_, _, w, _) = ut.rect();
    assert_eq!(w, 320);
}

#[test]
fn command_write_is_latched_for_deferred_execution() {
    let mut ut = UnitTester::default();
    assert_eq!(ut.read_port(PORT_COMMAND), Some(0)); // ready
    ut.write_port(PORT_COMMAND, CMD_CRC);
    assert_eq!(ut.take_pending(), Some(CMD_CRC));
    assert_eq!(ut.take_pending(), None);
}

#[test]
fn benchmark_region_round_trips_through_the_host_helpers() {
    let mut ut = UnitTester::default();
    ut.set_reg_u8(REG_SELECTOR, 1);
    assert_eq!(ut.reg_u8(REG_SELECTOR), 1);

    // Guest writes iterations and aux as little-endian u32 via the data port.
    ut.write_port(PORT_INDEX, REG_RESULT_ITER as u8);
    for byte in 40u32.to_le_bytes() {
        ut.write_port(PORT_DATA, byte);
    }
    for byte in 1899u32.to_le_bytes() {
        ut.write_port(PORT_DATA, byte);
    }
    assert_eq!(ut.reg_u32(REG_RESULT_ITER), 40);
    assert_eq!(ut.reg_u32(REG_RESULT_AUX), 1899);
}

#[test]
fn mark_id_round_trips_through_the_data_port() {
    let mut ut = UnitTester::default();
    ut.write_port(PORT_INDEX, REG_MARK as u8);
    ut.write_port(PORT_DATA, 3);
    assert_eq!(ut.mark_id(), 3);
}

#[test]
fn mark_register_does_not_overlap_the_benchmark_block() {
    // REG_RESULT_STATUS is the last Neurketa byte; the mark id must sit above it
    // or a benchmark run would clobber the boot profiler's boundary id.
    const { assert!(REG_MARK > REG_RESULT_STATUS) };
    let mut ut = UnitTester::default();
    ut.set_reg_u8(REG_RESULT_STATUS, 0xFF);
    assert_eq!(ut.mark_id(), 0);
}

#[test]
fn mark_command_is_latched_like_every_other_command() {
    let mut ut = UnitTester::default();
    ut.write_port(PORT_COMMAND, CMD_MARK);
    assert_eq!(ut.take_pending(), Some(CMD_MARK));
    assert_eq!(ut.take_pending(), None);
}
