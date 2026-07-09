// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

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
