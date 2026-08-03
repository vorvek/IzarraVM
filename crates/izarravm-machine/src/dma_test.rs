// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_bus::Memory;
use izarravm_core::{
    CanonicalSectionId, CanonicalSectionRequirement, CanonicalSectionVersion, CanonicalStateView,
    CanonicalStateWriter,
};

const TEST_TOTALS_ENVELOPE_ID: u32 = 0x7ffe_0001;

/// Physical byte address a 16-bit (slave) DMA channel drives, per the PC/AT
/// wiring: the page register supplies A23-A17 from its bits 7-1 (bit 0 is
/// ignored, because A16 comes from the counter) and the channel's word counter
/// supplies A16-A1 with A0 tied low.
///
/// Drivers program it as Linux's `set_dma_addr` does -- page `addr >> 16`,
/// counter `(addr >> 1) & 0xFFFF` -- so shifting the whole page byte left by 17
/// would count the page twice and land a 128 KB window past the buffer.
fn slave_byte_addr(page: u8, word_addr: u16) -> u32 {
    ((u32::from(page) & 0xFE) << 16) | (u32::from(word_addr) << 1)
}

fn canonical_dma_payload(dma: &DmaController) -> Vec<u8> {
    let mut state = CanonicalStateWriter::new().unwrap();
    state
        .section(
            CanonicalSectionId::new(0x0002_0006).unwrap(),
            CanonicalSectionVersion::new(1).unwrap(),
            CanonicalSectionRequirement::Required,
            |out| dma.canonical_projection().write_payload(out),
        )
        .unwrap();
    let bytes = state.finish().unwrap();
    let view = CanonicalStateView::parse(&bytes).unwrap();
    view.sections()[0].payload().to_vec()
}

fn canonical_dma_event_totals(dma: &DmaController) -> Vec<u8> {
    let mut state = CanonicalStateWriter::new().unwrap();
    state
        .section(
            CanonicalSectionId::new(TEST_TOTALS_ENVELOPE_ID).unwrap(),
            CanonicalSectionVersion::new(1).unwrap(),
            CanonicalSectionRequirement::Required,
            |out| dma.canonical_event_totals_v1().write_payload(out),
        )
        .unwrap();
    let bytes = state.finish().unwrap();
    let view = CanonicalStateView::parse(&bytes).unwrap();
    view.sections()[0].payload().to_vec()
}

fn canonical_dma_event_total(dma: &DmaController, channel: usize) -> u64 {
    let totals = canonical_dma_event_totals(dma);
    let offset = channel * 8;
    u64::from_le_bytes(totals[offset..offset + 8].try_into().unwrap())
}

fn channel_mut(dma: &mut DmaController, channel: usize) -> &mut DmaChannel {
    if channel < 4 {
        &mut dma.master.channels[channel]
    } else {
        &mut dma.slave.channels[channel - 4]
    }
}

fn channel_payload_offset(channel: usize) -> usize {
    if channel < 4 {
        4 + channel * 17
    } else {
        76 + (channel - 4) * 17
    }
}

fn assert_channel_offsets(
    channel: usize,
    before: DmaController,
    after: DmaController,
    local_offsets: &[usize],
) {
    let before_totals = canonical_dma_event_totals(&before);
    let after_totals = canonical_dma_event_totals(&after);
    assert_eq!(before_totals, after_totals, "channel {channel} totals");
    let before = canonical_dma_payload(&before);
    let after = canonical_dma_payload(&after);
    let changed = before
        .iter()
        .zip(&after)
        .enumerate()
        .filter_map(|(index, (left, right))| (left != right).then_some(index))
        .collect::<Vec<_>>();
    let base = channel_payload_offset(channel);
    let expected = local_offsets
        .iter()
        .map(|offset| base + offset)
        .collect::<Vec<_>>();
    assert_eq!(changed, expected, "channel {channel}");
}

fn golden_channel(index: u8) -> DmaChannel {
    DmaChannel {
        base_addr: 0x1000 | u16::from(index),
        cur_addr: 0x2000 | u16::from(index),
        base_count: 0x3000 | u16::from(index),
        cur_count: 0x4000 | u16::from(index),
        page: 0x50 | index,
        addr_decrement: index & 1 != 0,
        auto_init: index & 2 != 0,
        transfer_kind: index & 3,
        transfer_mode: 3 - (index & 3),
        mask: index & 1 == 0,
        reached_tc: index & 2 == 0,
        dreq: index & 4 != 0,
        active: index & 1 != 0,
        transfer_cycles: 0x0102_0304_0506_0708 + u64::from(index),
    }
}

fn golden_dma() -> DmaController {
    let mut page_scratch = [0; 16];
    for (port, value) in [
        (0x84usize, 0xa4),
        (0x85, 0xa5),
        (0x86, 0xa6),
        (0x88, 0xa8),
        (0x8c, 0xac),
        (0x8d, 0xad),
        (0x8e, 0xae),
    ] {
        page_scratch[port & 0x0f] = value;
    }
    DmaController {
        master: DmaChip {
            channels: std::array::from_fn(|index| golden_channel(index as u8)),
            hi_lo: true,
            command: 0xfb,
            status: 0xa5,
            request_reg: 0xca,
        },
        slave: DmaChip {
            channels: std::array::from_fn(|index| golden_channel((index + 4) as u8)),
            hi_lo: false,
            command: 0xff,
            status: 0x5a,
            request_reg: 0x35,
        },
        page_scratch,
        refresh_page: 0xaf,
    }
}

#[test]
fn canonical_dma_payload_layout_is_exact() {
    assert_eq!(
        canonical_dma_payload(&golden_dma()),
        [
            0x01, 0x03, 0x05, 0x0a, 0x00, 0x10, 0x00, 0x20, 0x00, 0x30, 0x00, 0x40, 0x50, 0x00,
            0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x00, 0x01, 0x10, 0x01, 0x20, 0x01, 0x30, 0x01,
            0x40, 0x51, 0x01, 0x00, 0x01, 0x02, 0x00, 0x01, 0x00, 0x01, 0x02, 0x10, 0x02, 0x20,
            0x02, 0x30, 0x02, 0x40, 0x52, 0x00, 0x01, 0x02, 0x01, 0x01, 0x00, 0x00, 0x00, 0x03,
            0x10, 0x03, 0x20, 0x03, 0x30, 0x03, 0x40, 0x53, 0x01, 0x01, 0x03, 0x00, 0x00, 0x00,
            0x00, 0x01, 0x00, 0x04, 0x0a, 0x05, 0x04, 0x10, 0x04, 0x20, 0x04, 0x30, 0x04, 0x40,
            0x54, 0x00, 0x00, 0x00, 0x03, 0x01, 0x01, 0x01, 0x00, 0x05, 0x10, 0x05, 0x20, 0x05,
            0x30, 0x05, 0x40, 0x55, 0x01, 0x00, 0x01, 0x02, 0x00, 0x01, 0x01, 0x01, 0x06, 0x10,
            0x06, 0x20, 0x06, 0x30, 0x06, 0x40, 0x56, 0x00, 0x01, 0x02, 0x01, 0x01, 0x00, 0x01,
            0x00, 0x07, 0x10, 0x07, 0x20, 0x07, 0x30, 0x07, 0x40, 0x57, 0x01, 0x01, 0x03, 0x00,
            0x00, 0x00, 0x01, 0x01, 0xa4, 0xa5, 0xa6, 0xa8, 0xac, 0xad, 0xae, 0xaf,
        ]
    );
}

#[test]
fn canonical_dma_event_totals_layout_is_exact() {
    assert_eq!(
        canonical_dma_event_totals(&golden_dma()),
        [
            0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, 0x09, 0x07, 0x06, 0x05, 0x04, 0x03,
            0x02, 0x01, 0x0a, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, 0x0b, 0x07, 0x06, 0x05,
            0x04, 0x03, 0x02, 0x01, 0x0c, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, 0x0d, 0x07,
            0x06, 0x05, 0x04, 0x03, 0x02, 0x01, 0x0e, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01,
            0x0f, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01,
        ]
    );
}

#[test]
fn canonical_dma_channel_fields_and_event_lanes_are_exact() {
    for channel in 0..8 {
        let base = DmaController::default();
        for (local_offsets, mutate) in [
            (&[0, 1][..], 0usize),
            (&[2, 3], 1),
            (&[4, 5], 2),
            (&[6, 7], 3),
        ] {
            let mut changed = base.clone();
            let target = channel_mut(&mut changed, channel);
            match mutate {
                0 => target.base_addr = 0x3412,
                1 => target.cur_addr = 0x3412,
                2 => target.base_count = 0x3412,
                _ => target.cur_count = 0x3412,
            }
            assert_channel_offsets(channel, base.clone(), changed, local_offsets);
        }
        for local_offset in 8usize..=16 {
            let mut changed = base.clone();
            let target = channel_mut(&mut changed, channel);
            let mutate = local_offset - 8;
            match mutate {
                0 => target.page = 0x5a,
                1 => target.addr_decrement = true,
                2 => target.auto_init = true,
                3 => target.transfer_kind = 3,
                4 => target.transfer_mode = 3,
                5 => target.mask = false,
                6 => target.reached_tc = true,
                7 => target.dreq = true,
                _ => target.active = true,
            }
            assert_channel_offsets(channel, base.clone(), changed, &[local_offset]);
        }

        let mut changed = base.clone();
        channel_mut(&mut changed, channel).transfer_cycles = 0x8877_6655_4433_2211;
        assert_eq!(
            canonical_dma_payload(&base),
            canonical_dma_payload(&changed)
        );
        let before = canonical_dma_event_totals(&base);
        let after = canonical_dma_event_totals(&changed);
        let changed_offsets = before
            .iter()
            .zip(&after)
            .enumerate()
            .filter_map(|(index, (left, right))| (left != right).then_some(index))
            .collect::<Vec<_>>();
        assert_eq!(
            changed_offsets,
            (channel * 8..channel * 8 + 8).collect::<Vec<_>>()
        );
    }
}

#[test]
fn canonical_dma_shared_and_page_offsets_are_exact() {
    let base = DmaController::default();
    for (offset, mutate) in [(0usize, 0usize), (1, 1), (2, 2), (3, 3)] {
        let mut changed = base.clone();
        match mutate {
            0 => changed.master.hi_lo = true,
            1 => changed.master.command = 1,
            2 => changed.master.status = 1,
            _ => changed.master.request_reg = 1,
        }
        let before = canonical_dma_payload(&base);
        let after = canonical_dma_payload(&changed);
        assert_eq!(
            before
                .iter()
                .zip(&after)
                .enumerate()
                .filter_map(|(index, (left, right))| (left != right).then_some(index))
                .collect::<Vec<_>>(),
            vec![offset]
        );
        assert_eq!(
            canonical_dma_event_totals(&base),
            canonical_dma_event_totals(&changed)
        );
    }
    for (offset, mutate) in [(72usize, 0usize), (73, 1), (74, 2), (75, 3)] {
        let mut changed = base.clone();
        match mutate {
            0 => changed.slave.hi_lo = true,
            1 => changed.slave.command = 4,
            2 => changed.slave.status = 1,
            _ => changed.slave.request_reg = 1,
        }
        let before = canonical_dma_payload(&base);
        let after = canonical_dma_payload(&changed);
        assert_eq!(
            before
                .iter()
                .zip(&after)
                .enumerate()
                .filter_map(|(index, (left, right))| (left != right).then_some(index))
                .collect::<Vec<_>>(),
            vec![offset]
        );
    }
    for (payload_offset, port) in [
        (144usize, 0x84usize),
        (145, 0x85),
        (146, 0x86),
        (147, 0x88),
        (148, 0x8c),
        (149, 0x8d),
        (150, 0x8e),
    ] {
        let mut changed = base.clone();
        changed.page_scratch[port & 0x0f] = 0x5a;
        let before = canonical_dma_payload(&base);
        let after = canonical_dma_payload(&changed);
        assert_eq!(
            before
                .iter()
                .zip(&after)
                .enumerate()
                .filter_map(|(index, (left, right))| (left != right).then_some(index))
                .collect::<Vec<_>>(),
            vec![payload_offset]
        );
    }
    let mut changed = base.clone();
    changed.refresh_page = 0x5a;
    assert_ne!(
        canonical_dma_payload(&base)[151],
        canonical_dma_payload(&changed)[151]
    );
}

#[test]
fn canonical_dma_normalizes_only_unconsumed_storage() {
    let mut left = DmaController::default();
    let mut right = left.clone();
    right.master.command = 0xfa;
    right.slave.command = 0xfb;
    right.master.status = 0xf0;
    right.slave.status = 0xf0;
    right.master.request_reg = 0xf0;
    right.slave.request_reg = 0xf0;
    for index in [0usize, 1, 2, 3, 7, 9, 10, 11, 15] {
        right.page_scratch[index] = 0x80 | index as u8;
    }
    assert_eq!(canonical_dma_payload(&left), canonical_dma_payload(&right));

    for dma in [&mut left, &mut right] {
        dma.write_port(0x0b, 0x49);
        dma.write_port(0x02, 0x10);
        dma.write_port(0x02, 0x00);
        dma.write_port(0x03, 0x00);
        dma.write_port(0x03, 0x00);
        dma.write_port(0x0a, 0x01);
    }
    let mut left_memory = mem_with(0x10, &[0x5a]);
    let mut right_memory = mem_with(0x10, &[0x5a]);
    assert_eq!(
        left.read_byte(1, &mut left_memory),
        right.read_byte(1, &mut right_memory)
    );
    assert_eq!(left.read_port(0x08), right.read_port(0x08));
    assert_eq!(canonical_dma_payload(&left), canonical_dma_payload(&right));
    assert_eq!(
        canonical_dma_event_totals(&left),
        canonical_dma_event_totals(&right)
    );

    let mut no_mem_to_mem = DmaController::default();
    no_mem_to_mem.master.command = 2;
    assert_eq!(
        canonical_dma_payload(&DmaController::default()),
        canonical_dma_payload(&no_mem_to_mem)
    );
    no_mem_to_mem.master.command = 3;
    assert_ne!(
        canonical_dma_payload(&DmaController::default()),
        canonical_dma_payload(&no_mem_to_mem)
    );
}

#[test]
fn canonical_dma_capture_preserves_destructive_shared_state() {
    let mut dma = DmaController::default();
    dma.master.hi_lo = true;
    dma.master.status = 0x01;
    dma.master.request_reg = 0x02;
    dma.master.channels[0].reached_tc = true;
    dma.slave.hi_lo = true;
    dma.slave.status = 0x04;
    dma.slave.request_reg = 0x08;
    dma.slave.channels[2].reached_tc = true;

    let first = canonical_dma_payload(&dma);
    let second = canonical_dma_payload(&dma);
    assert_eq!(first, second);
    assert_eq!(&first[0..4], &[1, 0, 1, 2]);
    assert_eq!(&first[72..76], &[1, 0, 4, 8]);

    assert_eq!(dma.read_port(0x08), Some(0x21));
    assert_eq!(dma.read_port(0xd0), Some(0x84));
    let consumed = canonical_dma_payload(&dma);
    assert_eq!(consumed[2], 0, "master status is read-clear");
    assert_eq!(consumed[74], 0, "slave status is read-clear");
    assert_eq!(consumed[3], 2, "master request persists");
    assert_eq!(consumed[75], 8, "slave request persists");
    assert_eq!(consumed[18], 1, "master channel TC state persists");
    assert_eq!(consumed[124], 1, "slave channel TC state persists");
}

#[test]
fn canonical_dma_preserves_half_read_and_write_continuations_on_both_chips() {
    let mut dma = DmaController::default();

    dma.write_port(0x0c, 0);
    dma.write_port(0x00, 0x34);
    assert_eq!(canonical_dma_payload(&dma)[0], 1);
    assert_eq!(canonical_dma_payload(&dma)[0], 1);
    dma.write_port(0x00, 0x12);
    assert_eq!(dma.master.channels[0].base_addr, 0x1234);
    assert_eq!(canonical_dma_payload(&dma)[0], 0);

    dma.master.channels[0].cur_addr = 0xabcd;
    dma.write_port(0x0c, 0);
    assert_eq!(dma.read_port(0x00), Some(0xcd));
    assert_eq!(canonical_dma_payload(&dma)[0], 1);
    assert_eq!(dma.read_port(0x00), Some(0xab));

    dma.write_port(0xd8, 0);
    dma.write_port(0xc4, 0x78);
    assert_eq!(canonical_dma_payload(&dma)[72], 1);
    assert_eq!(canonical_dma_payload(&dma)[72], 1);
    dma.write_port(0xc4, 0x56);
    assert_eq!(dma.slave.channels[1].base_addr, 0x5678);
    assert_eq!(canonical_dma_payload(&dma)[72], 0);

    dma.slave.channels[1].cur_addr = 0xef90;
    dma.write_port(0xd8, 0);
    assert_eq!(dma.read_port(0xc4), Some(0x90));
    assert_eq!(canonical_dma_payload(&dma)[72], 1);
    assert_eq!(dma.read_port(0xc4), Some(0xef));
}

#[test]
fn canonical_dma_event_totals_count_completed_cycles_in_transfer_units() {
    let mut dma = DmaController::default();
    let mut byte_memory = mem_with(0x10, &[0x5a]);
    dma.master.channels[0] = DmaChannel {
        cur_addr: 0x10,
        cur_count: 0,
        transfer_kind: 2,
        mask: false,
        ..Default::default()
    };
    assert_eq!(dma.read_byte(0, &mut byte_memory), Some(0x5a));

    let mut word_memory = mem_with(0x20, &[0x34, 0x12]);
    dma.slave.channels[1] = DmaChannel {
        cur_addr: 0x10,
        cur_count: 0,
        transfer_kind: 2,
        mask: false,
        ..Default::default()
    };
    assert_eq!(dma.read_word(5, &mut word_memory), Some(0x1234));

    dma.master.channels[1] = DmaChannel {
        cur_count: 0,
        mask: false,
        ..Default::default()
    };
    dma.master.set_hardware_request(1, true);
    assert_eq!(dma.master.verify(1), Some(()));
    dma.master.set_hardware_request(1, false);

    let mut write_memory = Memory::new(0x40).unwrap();
    dma.slave.channels[2] = DmaChannel {
        cur_addr: 0x10,
        cur_count: 0,
        transfer_kind: 1,
        mask: false,
        ..Default::default()
    };
    dma.slave.set_hardware_request(2, true);
    assert_eq!(dma.slave.write_word(2, &mut write_memory, 0xbeef), Some(()));
    dma.slave.set_hardware_request(2, false);

    assert_eq!(canonical_dma_event_total(&dma, 0), 1);
    assert_eq!(canonical_dma_event_total(&dma, 1), 1);
    assert_eq!(canonical_dma_event_total(&dma, 5), 1, "a word is one cycle");
    assert_eq!(canonical_dma_event_total(&dma, 6), 1, "a word is one cycle");

    dma.master.channels[3] = DmaChannel {
        cur_count: 1,
        mask: false,
        transfer_cycles: u64::MAX,
        ..Default::default()
    };
    dma.master.set_hardware_request(3, true);
    assert_eq!(dma.master.verify(3), Some(()));
    assert_eq!(canonical_dma_event_total(&dma, 3), u64::MAX);
}

#[test]
fn canonical_dma_event_totals_reject_incomplete_cycles() {
    let mut memory = Memory::new(1).unwrap();

    let mut masked = DmaController::default();
    masked.master.channels[0].transfer_kind = 2;
    assert_eq!(masked.read_byte(0, &mut memory), None);

    let mut wrong_kind = DmaController::default();
    wrong_kind.master.channels[0].mask = false;
    wrong_kind.master.channels[0].transfer_kind = 1;
    assert_eq!(wrong_kind.read_byte(0, &mut memory), None);

    let mut cascade = DmaController::default();
    cascade.master.channels[0].mask = false;
    cascade.master.channels[0].transfer_kind = 2;
    cascade.master.channels[0].transfer_mode = 3;
    assert_eq!(cascade.read_byte(0, &mut memory), None);

    let mut disabled = DmaController::default();
    disabled.master.channels[0].mask = false;
    disabled.master.channels[0].transfer_kind = 2;
    disabled.master.command = 4;
    assert_eq!(disabled.read_byte(0, &mut memory), None);

    let mut failed_memory = DmaController::default();
    failed_memory.master.channels[0].mask = false;
    failed_memory.master.channels[0].transfer_kind = 2;
    failed_memory.master.channels[0].cur_addr = 1;
    assert_eq!(failed_memory.read_byte(0, &mut memory), None);

    for dma in [&masked, &wrong_kind, &cascade, &disabled, &failed_memory] {
        assert_eq!(canonical_dma_event_totals(dma), vec![0; 64]);
    }
}

fn mem_with(addr: u32, bytes: &[u8]) -> Memory {
    let mut m = Memory::new((addr as usize) + bytes.len()).unwrap();
    for (i, &b) in bytes.iter().enumerate() {
        m.write_u8(addr as usize + i, b).unwrap();
    }
    m
}

#[test]
fn programming_channel_1_round_trips_through_ports() {
    let mut dma = DmaController::default();
    dma.write_port(0x0B, 0x49); // mode register, channel 1: single, read
    dma.write_port(0x02, 0x34); // base/current address LSB
    dma.write_port(0x02, 0x12); // ...MSB -> 0x1234
    dma.write_port(0x03, 0x0F); // base/current count LSB
    dma.write_port(0x03, 0x00); // ...MSB -> 0x000F
    dma.write_port(0x83, 0x05); // page register for channel 1 = 0x05
    dma.write_port(0x0A, 0x01); // clear mask for channel 1

    let ch = &dma.master.channels[1];
    assert_eq!(ch.base_addr, 0x1234);
    assert_eq!(ch.base_count, 0x000F);
    assert_eq!(ch.page, 0x05);
    assert!(!ch.mask);
    // Read-back of current address reuses the same flip-flop (LSB then MSB).
    assert_eq!(dma.read_port(0x02), Some(0x34));
    assert_eq!(dma.read_port(0x02), Some(0x12));
}

#[test]
fn page_registers_use_the_ibm_at_address_order() {
    let mut dma = DmaController::default();
    dma.write_port(0x83, 0x11);
    dma.write_port(0x81, 0x22);
    dma.write_port(0x82, 0x33);
    dma.write_port(0x87, 0x44);
    assert_eq!(dma.master.channels[1].page, 0x11); // 0x83 -> ch1
    assert_eq!(dma.master.channels[2].page, 0x22); // 0x81 -> ch2
    assert_eq!(dma.master.channels[3].page, 0x33); // 0x82 -> ch3
    assert_eq!(dma.master.channels[0].page, 0x44); // 0x87 -> ch0
}

#[test]
fn status_reports_terminal_count_after_a_transfer() {
    let mut dma = DmaController::default();
    // channel 1: address 0x10, page 0, count 0 -> 1 transfer
    dma.write_port(0x0B, 0x49);
    dma.write_port(0x02, 0x10);
    dma.write_port(0x02, 0x00);
    dma.write_port(0x03, 0x00);
    dma.write_port(0x03, 0x00);
    dma.write_port(0x0A, 0x01); // unmask ch1
    let mut mem = mem_with(0x0010, &[0x77]);
    assert_eq!(dma.read_byte(1, &mut mem), Some(0x77));
    // Status bit 1 latched; reading the status register returns and clears it.
    assert_eq!(dma.read_port(0x08), Some(0x02));
    assert_eq!(dma.read_port(0x08), Some(0x00), "TC bits cleared on read");
}

#[test]
fn single_transfer_reads_advances_and_signals_tc() {
    // Channel 1: page 0x00, base address 0x0010, count 2 (3 transfers: n+1).
    let mut ch = DmaChannel {
        base_addr: 0x0010,
        cur_addr: 0x0010,
        base_count: 2,
        cur_count: 2,
        page: 0x00,
        mask: false,
        ..Default::default()
    };
    ch.set_mode(0x49); // single transfer, read, auto-init off, ch1

    let mut mem = mem_with(0x0010, &[0x11, 0x22, 0x33]);
    let b0 = ch.read_byte(&mut mem).unwrap();
    let b1 = ch.read_byte(&mut mem).unwrap();
    let b2 = ch.read_byte(&mut mem).unwrap();
    assert_eq!([b0, b1, b2], [0x11, 0x22, 0x33]);
    assert!(ch.reached_tc);
    assert!(ch.mask, "single mode masks the channel at TC");
    assert_eq!(ch.read_byte(&mut mem), None, "no more data after TC");
}

#[test]
fn auto_init_reloads_from_base_at_tc() {
    let mut ch = DmaChannel {
        base_addr: 0x0008,
        cur_addr: 0x0008,
        base_count: 1, // 2 transfers per cycle
        cur_count: 1,
        mask: false,
        ..Default::default()
    };
    ch.set_mode(0x59); // auto-init on

    let mut mem = mem_with(0x0008, &[0xAA, 0xBB]);
    let _ = ch.read_byte(&mut mem);
    let second = ch.read_byte(&mut mem).unwrap(); // TC -> reload
    assert!(ch.reached_tc);
    assert!(!ch.mask, "auto-init keeps the channel unmasked");
    assert_eq!(second, 0xBB);
    assert_eq!(ch.cur_addr, ch.base_addr, "address reloaded from base");
    assert_eq!(ch.cur_count, ch.base_count, "count reloaded from base");
    assert_eq!(ch.read_byte(&mut mem).unwrap(), 0xAA, "restarts the buffer");
}

#[test]
fn slave_channel_5_reads_word_little_endian_and_steps_in_words() {
    // Channel 5 = slave local channel 1, page 0x8B.
    // Slave ports: 0xC4/0xC6 (stride-2 local 1), mode 0xD6, mask 0xD4.
    let mut dma = DmaController::default();
    dma.write_port(0xD6, 0x49); // mode, slave ch1: single, read, auto-init off
    dma.write_port(0xC4, 0x10); // slave ch1 address LSB
    dma.write_port(0xC4, 0x00); // ...MSB -> word addr 0x0010
    dma.write_port(0xC6, 0x00); // slave ch1 count LSB
    dma.write_port(0xC6, 0x00); // ...MSB -> 0 (1 word transfer)
    dma.write_port(0x8B, 0x02); // page 0x02 -> A17 set, so the page really contributes
    dma.write_port(0xD4, 0x01); // unmask slave ch1 (channel 5)

    // Seed two bytes at the word-aligned byte address.
    let byte_addr = slave_byte_addr(0x02, 0x0010);
    let mut mem = Memory::new(byte_addr as usize + 4).unwrap();
    mem.write_u8(byte_addr as usize, 0x34).unwrap();
    mem.write_u8(byte_addr as usize + 1, 0x12).unwrap();

    let word = dma.read_word(5, &mut mem).expect("a word from channel 5");
    assert_eq!(word, 0x1234, "little-endian word read");
    assert!(dma.slave.channels[1].reached_tc);
    assert!(dma.slave.channels[1].mask, "single mode masks at TC");
    assert_eq!(dma.read_word(5, &mut mem), None);
}

#[test]
fn slave_channel_5_auto_init_reloads_and_keeps_feeding() {
    let mut dma = DmaController::default();
    dma.write_port(0xD6, 0x59); // mode, slave ch1: auto-init, read
    dma.write_port(0xC4, 0x02); // word addr 0x0002
    dma.write_port(0xC4, 0x00);
    dma.write_port(0xC6, 0x01); // count 1 -> 2 word transfers per cycle
    dma.write_port(0xC6, 0x00);
    dma.write_port(0x8B, 0x02); // page 0x02 -> byte base 0x2_0000
    dma.write_port(0xD4, 0x01); // unmask slave ch1

    let byte_addr = slave_byte_addr(0x02, 0x0002);
    let mut mem = Memory::new(byte_addr as usize + 4).unwrap();
    mem.write_u8(byte_addr as usize, 0x78).unwrap();
    mem.write_u8(byte_addr as usize + 1, 0x56).unwrap();

    let w0 = dma.read_word(5, &mut mem).unwrap();
    let _tc = dma.read_word(5, &mut mem).unwrap(); // TC -> auto-init reload
    assert!(dma.slave.channels[1].reached_tc);
    assert!(
        !dma.slave.channels[1].mask,
        "auto-init keeps the channel live"
    );
    // After reload the address is back at the base, so the next word repeats.
    assert_eq!(w0, 0x5678);
    assert_eq!(dma.read_word(5, &mut mem), Some(0x5678), "buffer restarts");
}

// Page register read-back.

#[test]
fn channel_page_ports_read_back_what_was_written() {
    let mut dma = DmaController::default();
    // Master channel pages, in the AT's non-channel-order wiring.
    for (port, want) in [(0x83, 0xA1), (0x81, 0xA2), (0x82, 0xA3), (0x87, 0xA0)] {
        dma.write_port(port, want);
        assert_eq!(dma.read_port(port), Some(want), "master page {port:#x}");
    }
    // Slave channel pages (channels 5-7; 0x8F is no longer slave ch0).
    for (port, want) in [(0x8B, 0xB1), (0x89, 0xB2), (0x8A, 0xB3)] {
        dma.write_port(port, want);
        assert_eq!(dma.read_port(port), Some(want), "slave page {port:#x}");
    }
}

#[test]
fn scratch_page_ports_are_plain_read_write_latches() {
    let mut dma = DmaController::default();
    for (i, port) in [0x84u16, 0x85, 0x86, 0x88, 0x8C, 0x8D, 0x8E]
        .into_iter()
        .enumerate()
    {
        let val = 0x10 + i as u8;
        assert_eq!(
            dma.read_port(port),
            Some(0),
            "scratch {port:#x} starts zero"
        );
        assert!(
            dma.write_port(port, val),
            "scratch {port:#x} accepts a write"
        );
        assert_eq!(
            dma.read_port(port),
            Some(val),
            "scratch {port:#x} round trip"
        );
    }
}

#[test]
fn dma_does_not_claim_the_post_diagnostic_port_0x80() {
    // 0x80 stays with the machine's passive POST latch, so the DMA controller
    // must decline both reads and writes for it.
    let mut dma = DmaController::default();
    assert!(
        !dma.write_port(0x80, 0x42),
        "0x80 is not a DMA scratch latch"
    );
    assert_eq!(dma.read_port(0x80), None, "0x80 reads fall through DMA");
}

#[test]
fn refresh_page_0x8f_is_its_own_latch_not_slave_channel_zero() {
    let mut dma = DmaController::default();
    dma.write_port(0x8F, 0x77);
    assert_eq!(dma.read_port(0x8F), Some(0x77), "0x8F reads back");
    assert_eq!(dma.refresh_page, 0x77);
    // Writing 0x8F must not bleed into slave channel 0 (the cascade channel).
    assert_eq!(dma.slave.channels[0].page, 0x00);
}

// Status request-active bits.

#[test]
fn status_reflects_software_request_bits_without_clearing_them() {
    let mut dma = DmaController::default();
    // Request register write: bit2 sets, bits0-1 select the channel (ch2).
    dma.write_port(0x09, 0x06); // set DREQ for channel 2
    let s = dma.read_port(0x08).unwrap();
    assert_eq!(s & (1 << (4 + 2)), 1 << 6, "request bit appears at 4+ci");
    // Request bits are level, not read-cleared: a second read still shows it.
    let s2 = dma.read_port(0x08).unwrap();
    assert_eq!(
        s2 & (1 << 6),
        1 << 6,
        "request bit is level, survives a read"
    );
}

#[test]
fn status_tc_bits_clear_but_request_bits_persist() {
    let mut dma = DmaController::default();
    // Channel 0: one transfer to latch a TC bit, plus a software request.
    dma.write_port(0x0B, 0x48); // mode ch0: single, read
    dma.write_port(0x00, 0x00); // address LSB
    dma.write_port(0x00, 0x00); // address MSB -> 0
    dma.write_port(0x01, 0x00); // count -> 0 (one transfer)
    dma.write_port(0x01, 0x00);
    dma.write_port(0x0A, 0x00); // unmask ch0 (low 2 bits select the channel)
    dma.write_port(0x09, 0x05); // software DREQ for channel 1
    let mut mem = mem_with(0x0000, &[0x99]);
    assert_eq!(dma.read_byte(0, &mut mem), Some(0x99));
    let s = dma.read_port(0x08).unwrap();
    assert_eq!(s & 0x01, 0x01, "ch0 TC latched");
    assert_eq!(s & (1 << 5), 1 << 5, "ch1 request active");
    let s2 = dma.read_port(0x08).unwrap();
    assert_eq!(s2 & 0x01, 0x00, "TC bit cleared on read");
    assert_eq!(s2 & (1 << 5), 1 << 5, "request bit remains");
}

// Mode register transfer-mode field.

#[test]
fn set_mode_decodes_the_transfer_mode_field() {
    for (bits76, want) in [(0u8, 0u8), (1, 1), (2, 2), (3, 3)] {
        let mut ch = DmaChannel::default();
        // bits 6-7 carry the mode; keep the rest at a benign read encoding.
        ch.set_mode((bits76 << 6) | 0x08);
        assert_eq!(ch.transfer_mode, want, "mode bits {bits76:02b}");
    }
}

// Device-to-memory write and verify datapaths.

#[test]
fn write_transfer_stores_to_memory_steps_and_signals_tc() {
    // Channel programmed for a write (device->memory): kind 1, single mode.
    let mut ch = DmaChannel {
        base_addr: 0x0020,
        cur_addr: 0x0020,
        base_count: 2, // 3 transfers (n+1)
        cur_count: 2,
        mask: false,
        ..Default::default()
    };
    ch.set_mode(0x45); // single, write (kind 1), auto-init off, ch1

    let mut mem = Memory::new(0x0020 + 4).unwrap();
    ch.write_byte(&mut mem, 0xDE).unwrap();
    ch.write_byte(&mut mem, 0xAD).unwrap();
    ch.write_byte(&mut mem, 0xBE).unwrap();
    assert_eq!(mem.read_u8(0x0020).unwrap(), 0xDE);
    assert_eq!(mem.read_u8(0x0021).unwrap(), 0xAD);
    assert_eq!(mem.read_u8(0x0022).unwrap(), 0xBE);
    assert!(ch.reached_tc);
    assert!(ch.mask, "single mode masks the channel at TC");
    assert_eq!(ch.write_byte(&mut mem, 0x00), None, "no writes after TC");
}

#[test]
fn write_transfer_auto_init_reloads_from_base() {
    let mut ch = DmaChannel {
        base_addr: 0x0010,
        cur_addr: 0x0010,
        base_count: 1, // 2 transfers per cycle
        cur_count: 1,
        mask: false,
        ..Default::default()
    };
    ch.set_mode(0x55); // single, write, auto-init on

    let mut mem = Memory::new(0x0010 + 4).unwrap();
    ch.write_byte(&mut mem, 0x01).unwrap();
    ch.write_byte(&mut mem, 0x02).unwrap(); // TC -> reload
    assert!(ch.reached_tc);
    assert!(!ch.mask, "auto-init keeps the channel unmasked");
    assert_eq!(ch.cur_addr, ch.base_addr, "address reloaded from base");
    assert_eq!(ch.cur_count, ch.base_count, "count reloaded from base");
    // After reload the next write lands back at the base address.
    ch.write_byte(&mut mem, 0x03).unwrap();
    assert_eq!(mem.read_u8(0x0010).unwrap(), 0x03, "buffer restarts");
}

#[test]
fn write_word_stores_little_endian_on_the_slave_path() {
    let mut ch = DmaChannel {
        base_addr: 0x0008,
        cur_addr: 0x0008,
        base_count: 0,
        cur_count: 0,
        page: 0x02,
        mask: false,
        ..Default::default()
    };
    ch.set_mode(0x45); // single, write (kind 1)

    let byte_addr = slave_byte_addr(0x02, 0x0008) as usize;
    let mut mem = Memory::new(byte_addr + 4).unwrap();
    ch.write_word(&mut mem, 0xBEEF).unwrap();
    assert_eq!(mem.read_u8(byte_addr).unwrap(), 0xEF, "low byte first");
    assert_eq!(mem.read_u8(byte_addr + 1).unwrap(), 0xBE, "high byte next");
    assert!(ch.reached_tc);
}

#[test]
fn transfer_kind_gates_the_datapaths() {
    // A read-programmed channel refuses writes; a write-programmed one refuses
    // reads; and verify only runs when kind is 0.
    let mut read_ch = DmaChannel {
        cur_count: 1,
        mask: false,
        ..Default::default()
    };
    read_ch.set_mode(0x48); // kind 2 (read)
    let mut mem = Memory::new(8).unwrap();
    assert_eq!(
        read_ch.write_byte(&mut mem, 0xFF),
        None,
        "read channel refuses a write"
    );
    assert_eq!(read_ch.verify(), None, "read channel refuses a verify");
    assert!(read_ch.read_byte(&mut mem).is_some(), "read channel reads");

    let mut write_ch = DmaChannel {
        cur_count: 1,
        mask: false,
        ..Default::default()
    };
    write_ch.set_mode(0x44); // kind 1 (write)
    assert_eq!(
        write_ch.read_byte(&mut mem),
        None,
        "write channel refuses a read"
    );
    assert!(
        write_ch.write_byte(&mut mem, 0x01).is_some(),
        "write channel writes"
    );
}

#[test]
fn verify_transfer_steps_without_touching_memory() {
    let mut ch = DmaChannel {
        base_addr: 0x0030,
        cur_addr: 0x0030,
        base_count: 1, // 2 transfers
        cur_count: 1,
        mask: false,
        ..Default::default()
    };
    ch.set_mode(0x40); // single, verify (kind 0)
    ch.verify().unwrap();
    assert_eq!(ch.cur_addr, 0x0031, "verify still advances the address");
    assert_eq!(ch.cur_count, 0, "verify still decrements the count");
    ch.verify().unwrap(); // TC
    assert!(ch.reached_tc);
    assert!(ch.mask, "single mode masks the channel at verify TC");
    assert_eq!(ch.verify(), None, "no verify after TC");
}

#[test]
fn chip_write_latches_terminal_count_into_status() {
    // Drive the chip-level write wrapper so its TC-latch path is exercised.
    let mut chip = DmaChip::default();
    chip.channels[2].mask = false;
    chip.channels[2].cur_addr = 0x0040;
    chip.channels[2].base_addr = 0x0040;
    chip.channels[2].cur_count = 0; // one transfer -> immediate TC
    chip.channels[2].set_mode(0x44); // kind 1 (write), ch2 bits ignored here
    chip.set_hardware_request(2, true);
    let mut mem = Memory::new(0x0040 + 2).unwrap();
    chip.write_byte(2, &mut mem, 0x5A).unwrap();
    chip.set_hardware_request(2, false);
    assert_eq!(mem.read_u8(0x0040).unwrap(), 0x5A);
    // Status read returns the latched TC for channel 2 and clears it.
    assert_eq!(chip.read_local(8), Some(0x04));
    assert_eq!(chip.read_local(8), Some(0x00));
}

#[test]
fn chip_verify_latches_terminal_count_into_status() {
    let mut chip = DmaChip::default();
    chip.channels[3].mask = false;
    chip.channels[3].cur_count = 0; // one transfer -> immediate TC
    chip.channels[3].set_mode(0x40); // kind 0 (verify)
    chip.set_hardware_request(3, true);
    chip.verify(3).unwrap();
    chip.set_hardware_request(3, false);
    assert_eq!(chip.read_local(8), Some(0x08), "ch3 TC latched by verify");
}

#[test]
fn chip_write_word_latches_terminal_count_into_status() {
    let mut chip = DmaChip::default();
    chip.channels[1].mask = false;
    chip.channels[1].cur_addr = 0x0004;
    chip.channels[1].page = 0x00;
    chip.channels[1].cur_count = 0; // one transfer -> immediate TC
    chip.channels[1].set_mode(0x44); // kind 1 (write)
    chip.set_hardware_request(1, true);
    let byte_addr = (0x0004u32 << 1) as usize;
    let mut mem = Memory::new(byte_addr + 4).unwrap();
    chip.write_word(1, &mut mem, 0x1234).unwrap();
    chip.set_hardware_request(1, false);
    assert_eq!(mem.read_u8(byte_addr).unwrap(), 0x34);
    assert_eq!(mem.read_u8(byte_addr + 1).unwrap(), 0x12);
    assert_eq!(
        chip.read_local(8),
        Some(0x02),
        "ch1 TC latched by word write"
    );
}

// Command register and memory-to-memory transfer.

#[test]
fn command_register_round_trips_through_port_0x08() {
    let mut dma = DmaController::default();
    // Set every command bit and read each decoder back.
    dma.write_port(0x08, 0xFF);
    assert_eq!(dma.master.command, 0xFF, "command stored verbatim");
    assert!(dma.master.mem_to_mem_enabled(), "bit0 mem-to-mem enable");
    assert!(dma.master.channel0_hold(), "bit1 channel-0 address hold");
    assert!(dma.master.controller_disabled(), "bit2 controller disable");

    // Clear it and confirm the decoders flip back.
    dma.write_port(0x08, 0x00);
    assert_eq!(dma.master.command, 0x00);
    assert!(!dma.master.mem_to_mem_enabled());
    assert!(!dma.master.channel0_hold());
    assert!(!dma.master.controller_disabled());

    // A single bit at a time decodes independently.
    dma.write_port(0x08, 0x01);
    assert!(dma.master.mem_to_mem_enabled());
    assert!(!dma.master.channel0_hold());
    assert!(!dma.master.controller_disabled());
    dma.write_port(0x08, 0x04);
    assert!(!dma.master.mem_to_mem_enabled());
    assert!(dma.master.controller_disabled());
}

#[test]
fn controller_disable_bit_inhibits_a_transfer() {
    let mut dma = DmaController::default();
    // Program channel 1 for a normal read of one byte.
    dma.write_port(0x0B, 0x49); // mode ch1: single, read
    dma.write_port(0x02, 0x10); // address 0x0010
    dma.write_port(0x02, 0x00);
    dma.write_port(0x03, 0x00); // count 0 -> one transfer
    dma.write_port(0x03, 0x00);
    dma.write_port(0x0A, 0x01); // unmask ch1
    let mut mem = mem_with(0x0010, &[0x77]);

    // Controller disabled (command bit2): the read is inhibited.
    dma.write_port(0x08, 0x04);
    assert_eq!(
        dma.read_byte(1, &mut mem),
        None,
        "disabled controller refuses a read"
    );
    // Clearing the disable bit lets the same transfer through.
    dma.write_port(0x08, 0x00);
    assert_eq!(dma.read_byte(1, &mut mem), Some(0x77));
}

#[test]
fn mem_to_mem_copies_a_block_from_ch0_to_ch1() {
    let mut dma = DmaController::default();
    // Source at 0x0100, destination at 0x0200, four bytes (count 3 = n+1).
    dma.write_port(0x00, 0x00); // ch0 address 0x0100
    dma.write_port(0x00, 0x01);
    dma.write_port(0x02, 0x00); // ch1 address 0x0200
    dma.write_port(0x02, 0x02);
    dma.write_port(0x03, 0x03); // ch1 count 3 -> 4 bytes
    dma.write_port(0x03, 0x00);
    dma.write_port(0x0A, 0x00); // unmask ch0 (the requester)
    dma.write_port(0x08, 0x01); // command: mem-to-mem enable

    let mut mem = Memory::new(0x0300).unwrap();
    for (i, b) in [0xDE, 0xAD, 0xBE, 0xEF].into_iter().enumerate() {
        mem.write_u8(0x0100 + i, b).unwrap();
    }

    let copied = dma.mem_to_mem(&mut mem).expect("a block copy");
    assert_eq!(copied, 4, "copied ch1 count + 1 bytes");
    for (i, b) in [0xDE, 0xAD, 0xBE, 0xEF].into_iter().enumerate() {
        assert_eq!(mem.read_u8(0x0200 + i).unwrap(), b, "dest byte {i}");
    }
    // Channel 1 (the destination) reached terminal count and latched it.
    assert!(dma.master.channels[1].reached_tc);
    assert_eq!(dma.read_port(0x08).map(|s| s & 0x02), Some(0x02), "ch1 TC");
    // Both address counters advanced past the block.
    assert_eq!(dma.master.channels[0].cur_addr, 0x0104, "source advanced");
    assert_eq!(dma.master.channels[1].cur_addr, 0x0204, "dest advanced");
    assert_eq!(canonical_dma_event_total(&dma, 0), 4);
    assert_eq!(canonical_dma_event_total(&dma, 1), 4);
}

#[test]
fn mem_to_mem_software_request_is_reset_at_terminal_count() {
    let mut dma = DmaController::default();
    dma.write_port(0x00, 0x00); // ch0 source 0x0100
    dma.write_port(0x00, 0x01);
    dma.write_port(0x01, 0x0A); // ch0 count 10: large enough not to self-reach TC
    dma.write_port(0x01, 0x00);
    dma.write_port(0x02, 0x00); // ch1 dest 0x0200
    dma.write_port(0x02, 0x02);
    dma.write_port(0x03, 0x01); // ch1 count 1 -> 2 bytes
    dma.write_port(0x03, 0x00);
    dma.write_port(0x0A, 0x00); // unmask ch0
    dma.write_port(0x08, 0x01); // mem-to-mem enable
    dma.write_port(0x09, 0x04); // software request: set channel 0
    assert!(
        dma.mem_to_mem_request_armed(),
        "request armed before the copy"
    );

    let mut mem = Memory::new(0x0300).unwrap();
    dma.mem_to_mem(&mut mem).expect("a block copy");

    // Channel 0 did not exhaust its count, so it is not self-masked; the request
    // is no longer armed only because the software DREQ was reset at TC.
    assert!(!dma.master.channels[0].mask, "ch0 still unmasked");
    assert!(
        !dma.mem_to_mem_request_armed(),
        "software request reset at terminal count, no spurious re-arm"
    );
}

#[test]
fn mem_to_mem_address_hold_turns_the_copy_into_a_fill() {
    let mut dma = DmaController::default();
    // Source one byte at 0x0040, destination block at 0x0050, four bytes.
    dma.write_port(0x00, 0x40); // ch0 address 0x0040
    dma.write_port(0x00, 0x00);
    dma.write_port(0x02, 0x50); // ch1 address 0x0050
    dma.write_port(0x02, 0x00);
    dma.write_port(0x03, 0x03); // ch1 count 3 -> 4 bytes
    dma.write_port(0x03, 0x00);
    dma.write_port(0x0A, 0x00); // unmask ch0
    dma.write_port(0x08, 0x03); // command: mem-to-mem enable + ch0 hold

    let mut mem = Memory::new(0x0100).unwrap();
    mem.write_u8(0x0040, 0x5A).unwrap();

    let copied = dma.mem_to_mem(&mut mem).expect("a fill");
    assert_eq!(copied, 4);
    for i in 0..4 {
        assert_eq!(mem.read_u8(0x0050 + i).unwrap(), 0x5A, "fill byte {i}");
    }
    // The held source address never moved; only the count drained.
    assert_eq!(dma.master.channels[0].cur_addr, 0x0040, "source held");
    assert_eq!(dma.master.channels[1].cur_addr, 0x0054, "dest advanced");
    assert_eq!(canonical_dma_event_total(&dma, 0), 4);
    assert_eq!(canonical_dma_event_total(&dma, 1), 4);
}

#[test]
fn mem_to_mem_is_gated_by_enable_and_disable_bits() {
    let mut dma = DmaController::default();
    dma.write_port(0x02, 0x00); // ch1 address 0
    dma.write_port(0x02, 0x00);
    dma.write_port(0x03, 0x00); // ch1 count 0 -> one byte
    dma.write_port(0x03, 0x00);
    dma.write_port(0x0A, 0x00); // unmask ch0
    let mut mem = Memory::new(0x10).unwrap();

    // Mem-to-mem not enabled (bit0 clear): no transfer.
    dma.write_port(0x08, 0x00);
    assert_eq!(dma.mem_to_mem(&mut mem), None, "disabled mem-to-mem path");
    // Enabled but the controller is disabled (bit2): still no transfer.
    dma.write_port(0x08, 0x05);
    assert_eq!(dma.mem_to_mem(&mut mem), None, "controller disabled");
    // Enabled with channel 0 masked: the requester cannot run.
    dma.write_port(0x08, 0x01);
    dma.write_port(0x0A, 0x04); // mask ch0
    assert_eq!(dma.mem_to_mem(&mut mem), None, "masked channel 0");
    assert_eq!(canonical_dma_event_totals(&dma), vec![0; 64]);
}

#[test]
fn failed_mem_to_mem_cycle_does_not_change_event_totals() {
    let mut dma = DmaController::default();
    dma.write_port(0x00, 0x01); // source address 1, outside one-byte memory
    dma.write_port(0x00, 0x00);
    dma.write_port(0x02, 0x00);
    dma.write_port(0x02, 0x00);
    dma.write_port(0x03, 0x00);
    dma.write_port(0x03, 0x00);
    dma.write_port(0x0a, 0x00);
    dma.write_port(0x08, 0x01);
    let before_state = canonical_dma_payload(&dma);
    let before_totals = canonical_dma_event_totals(&dma);
    let mut memory = Memory::new(1).unwrap();

    assert_eq!(dma.mem_to_mem(&mut memory), None);
    assert_eq!(canonical_dma_event_totals(&dma), before_totals);
    let after_state = canonical_dma_payload(&dma);
    assert_eq!(after_state[4..72], before_state[4..72]);
}

#[test]
fn hardware_request_level_is_visible_until_the_device_drops_it() {
    let mut dma = DmaController::default();
    dma.master.set_hardware_request(2, true);
    assert_eq!(dma.read_port(0x08).unwrap() & 0x40, 0x40);
    assert_eq!(dma.read_port(0x08).unwrap() & 0x40, 0x40);
    dma.master.set_hardware_request(2, false);
    assert_eq!(dma.read_port(0x08).unwrap() & 0x40, 0);
}

#[test]
fn one_device_request_consumes_one_channel_cycle() {
    let mut dma = DmaController::default();
    dma.write_port(0x0B, 0x46); // channel 2, single, device->memory
    dma.write_port(0x04, 0x20);
    dma.write_port(0x04, 0x00);
    dma.write_port(0x05, 0x01); // two transfers
    dma.write_port(0x05, 0x00);
    dma.write_port(0x0A, 0x02);
    let mut memory = Memory::new(0x40).unwrap();

    assert_eq!(dma.master.channels[2].transfer_cycles, 0);
    assert_eq!(dma.write_byte(2, &mut memory, 0xA5), Some(0x20));
    let channel = &dma.master.channels[2];
    assert_eq!(channel.transfer_cycles, 1);
    assert_eq!(channel.cur_addr, 0x0021);
    assert_eq!(channel.cur_count, 0);
    assert!(!channel.dreq, "the one-cycle request pulse has ended");
    assert!(!channel.active, "the bus grant has ended");
}

#[test]
fn mask_and_cascade_mode_refuse_a_device_cycle_without_stepping() {
    let mut dma = DmaController::default();
    let mut memory = Memory::new(4).unwrap();
    dma.write_port(0x0B, 0x46);
    assert_eq!(dma.write_byte(2, &mut memory, 1), None, "masked");
    assert_eq!(dma.master.channels[2].transfer_cycles, 0);

    dma.write_port(0x0A, 0x02);
    dma.write_port(0x0B, 0xC6); // channel 2 cascade
    assert_eq!(dma.write_byte(2, &mut memory, 1), None, "cascade");
    assert_eq!(dma.master.channels[2].transfer_cycles, 0);
}

#[test]
fn block_mode_keeps_the_channel_active_until_terminal_count() {
    let mut dma = DmaController::default();
    dma.write_port(0x0B, 0x86); // channel 2, block, device->memory
    dma.write_port(0x04, 0x00);
    dma.write_port(0x04, 0x00);
    dma.write_port(0x05, 0x01); // two cycles
    dma.write_port(0x05, 0x00);
    dma.write_port(0x0A, 0x02);
    let mut memory = Memory::new(4).unwrap();

    dma.write_byte(2, &mut memory, 0x11).unwrap();
    assert!(dma.master.channels[2].active);
    dma.write_byte(2, &mut memory, 0x22).unwrap();
    assert!(dma.master.channels[2].reached_tc);
    assert!(!dma.master.channels[2].active);
    assert_eq!(dma.master.channels[2].transfer_cycles, 2);
}

#[test]
fn a_rejected_block_cycle_does_not_latch_the_channel_active() {
    let mut dma = DmaController::default();
    dma.write_port(0x0B, 0x8A); // channel 2, block, memory->device
    dma.write_port(0x0A, 0x02);
    let mut memory = Memory::new(4).unwrap();
    assert_eq!(dma.write_byte(2, &mut memory, 0x55), None);
    assert!(!dma.master.channels[2].active);
    assert_eq!(dma.master.channels[2].transfer_cycles, 0);
}

/// A 16-bit DMA buffer must be read from the address the driver programmed, not
/// one page-shift further on. This is the defect that silenced Quake: it runs
/// its SB16 output on channel 5 (16-bit, auto-init) and every counter looked
/// healthy -- transfers ticking, IRQs firing, the resampler producing a full
/// 44.1 kHz stream -- while the fetch came from the wrong 128 KB window, so the
/// mixer received a region that happened to be zeros. Doom was unaffected
/// because it drives the 8-bit path on channel 1.
#[test]
fn slave_channel_5_reads_the_buffer_the_driver_programmed() {
    // A buffer whose page byte has bit 0 set, so a page-shift error moves the
    // read somewhere else entirely rather than coincidentally landing right.
    const BUF: u32 = 0x0003_4000;
    let page = (BUF >> 16) as u8; // 0x03, exactly what a driver writes
    let counter = ((BUF >> 1) & 0xFFFF) as u16;
    assert_eq!(
        slave_byte_addr(page, counter),
        BUF,
        "the reference formula must reproduce the programmed address"
    );

    let mut dma = DmaController::default();
    dma.write_port(0xD6, 0x49); // slave ch1: single, read, auto-init off
    dma.write_port(0xC4, (counter & 0xFF) as u8);
    dma.write_port(0xC4, (counter >> 8) as u8);
    dma.write_port(0xC6, 0x00); // count 0 -> one word
    dma.write_port(0xC6, 0x00);
    dma.write_port(0x8B, page);
    dma.write_port(0xD4, 0x01); // unmask channel 5

    // Size memory to just past the buffer: a read from the wrong window is out
    // of range and fails outright rather than silently returning other data.
    let mut mem = Memory::new(BUF as usize + 4).unwrap();
    mem.write_u8(BUF as usize, 0xCD).unwrap();
    mem.write_u8(BUF as usize + 1, 0xAB).unwrap();

    assert_eq!(
        dma.read_word(5, &mut mem),
        Some(0xABCD),
        "channel 5 must fetch from the programmed physical address"
    );
}
