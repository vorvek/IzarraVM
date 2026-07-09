// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::cdimage::{CdImage, DATA_SECTOR};

fn data_disc(sectors: u32) -> CdImage {
    let mut bytes = vec![0u8; sectors as usize * DATA_SECTOR];
    for s in 0..sectors as usize {
        bytes[s * DATA_SECTOR] = (s as u8).wrapping_add(0x50);
    }
    CdImage::from_iso(bytes).unwrap()
}

/// Drive the full PACKET handshake for a READ(10) of one sector at `lba` and
/// return the drained data-in buffer.
fn packet_read10(ch: &mut IdeChannel, lba: u32) -> Vec<u8> {
    // PACKET command.
    ch.write_port(SECONDARY_CMD_BASE + 7, 0xA0);
    assert_eq!(ch.status & status::DRQ, status::DRQ);
    // Feed the 12-byte CDB through the data register.
    let mut cdb = [0u8; 12];
    cdb[0] = 0x28;
    cdb[2..6].copy_from_slice(&lba.to_be_bytes());
    cdb[7] = 0;
    cdb[8] = 1; // one sector
    for b in cdb {
        ch.write_port(SECONDARY_CMD_BASE, b);
    }
    // After the packet, data-in is armed and the byte count is set.
    let count = u16::from_le_bytes([ch.lba_mid, ch.lba_high]) as usize;
    assert_eq!(count, DATA_SECTOR);
    assert_eq!(ch.status & status::DRQ, status::DRQ);
    // Drain the data register.
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(ch.read_port(SECONDARY_CMD_BASE).unwrap());
    }
    out
}

#[test]
fn packet_read10_round_trips_a_sector() {
    let mut ch = IdeChannel::new();
    ch.device_mut().insert(data_disc(8));
    // Clear the post-insert unit attention with a TEST UNIT READY.
    ch.write_port(SECONDARY_CMD_BASE + 7, 0xA0);
    for b in [0u8; 12] {
        ch.write_port(SECONDARY_CMD_BASE, b);
    }
    let buf = packet_read10(&mut ch, 2);
    assert_eq!(buf.len(), DATA_SECTOR);
    assert_eq!(buf[0], 0x52); // 0x50 + 2
    // After draining, DRQ drops and the channel is idle/ready.
    assert_eq!(ch.status & status::DRQ, 0);
    assert_eq!(ch.status & status::DRDY, status::DRDY);
}

#[test]
fn packet_command_raises_irq_when_enabled() {
    let mut ch = IdeChannel::new();
    ch.device_mut().insert(data_disc(4));
    // Run a TEST UNIT READY packet.
    ch.write_port(SECONDARY_CMD_BASE + 7, 0xA0);
    for b in [0u8; 12] {
        ch.write_port(SECONDARY_CMD_BASE, b);
    }
    assert!(ch.take_irq());
    // A second take clears it.
    assert!(!ch.take_irq());
}

#[test]
fn nien_suppresses_the_irq() {
    let mut ch = IdeChannel::new();
    ch.device_mut().insert(data_disc(4));
    // Set nIEN via the control register.
    ch.write_port(SECONDARY_CTRL, 0x02);
    ch.write_port(SECONDARY_CMD_BASE + 7, 0xA0);
    for b in [0u8; 12] {
        ch.write_port(SECONDARY_CMD_BASE, b);
    }
    assert!(!ch.take_irq());
}

#[test]
fn identify_packet_device_returns_512_bytes() {
    let mut ch = IdeChannel::new();
    ch.write_port(SECONDARY_CMD_BASE + 7, 0xA1);
    let count = u16::from_le_bytes([ch.lba_mid, ch.lba_high]) as usize;
    assert_eq!(count, 512);
    let mut block = Vec::new();
    for _ in 0..512 {
        block.push(ch.read_port(SECONDARY_CMD_BASE).unwrap());
    }
    // Word 0 low/high bytes: 0x85C0 little-endian.
    assert_eq!(block[0], 0xC0);
    assert_eq!(block[1], 0x85);
}

#[test]
fn soft_reset_leaves_the_atapi_signature() {
    let mut ch = IdeChannel::new();
    ch.write_port(SECONDARY_CTRL, 0x04); // SRST
    ch.write_port(SECONDARY_CTRL, 0x00);
    assert_eq!((ch.lba_mid, ch.lba_high), (0x14, 0xEB));
}

#[test]
fn slave_select_makes_commands_error() {
    let mut ch = IdeChannel::new();
    ch.write_port(SECONDARY_CMD_BASE + 6, 0x10); // select slave
    ch.write_port(SECONDARY_CMD_BASE + 7, 0xA0);
    assert_eq!(ch.status & status::ERR, status::ERR);
}

#[test]
fn read10_charges_seek_and_transfer_time() {
    let mut ch = IdeChannel::new();
    ch.device_mut().insert(data_disc(8));
    // clear unit attention
    ch.write_port(SECONDARY_CMD_BASE + 7, 0xA0);
    for b in [0u8; 12] {
        ch.write_port(SECONDARY_CMD_BASE, b);
    }
    let _ = ch.take_stall_secs();
    let _ = packet_read10(&mut ch, 0);
    let secs = ch.take_stall_secs();
    assert!(secs > 0.0);
    assert_eq!(ch.take_access_bytes(), DATA_SECTOR);
}

#[test]
fn nop_command_always_aborts() {
    let mut ch = IdeChannel::new();
    ch.write_port(SECONDARY_CMD_BASE + 7, 0x00); // NOP
    assert_eq!(ch.status & status::DRDY, status::DRDY);
    assert_eq!(ch.status & status::ERR, status::ERR);
    assert_eq!(ch.error, 0x04);
}

#[test]
fn execute_diagnostic_passes_with_atapi_signature() {
    let mut ch = IdeChannel::new();
    ch.write_port(SECONDARY_CMD_BASE + 7, 0x90); // EXECUTE DEVICE DIAGNOSTIC
    // Device 0 passed and no error bit, so BIOS detection still sees it.
    assert_eq!(ch.error, 0x01);
    assert_eq!(ch.status & status::ERR, 0);
    assert_eq!(ch.status & status::DRDY, status::DRDY);
    // The ATAPI signature stays in the byte-count registers.
    assert_eq!((ch.sector_count, ch.lba_low), (0x01, 0x01));
    assert_eq!((ch.lba_mid, ch.lba_high), (0x14, 0xEB));
    // Completion raises the IRQ.
    assert!(ch.take_irq());
}

#[test]
fn packet_read_chunks_to_the_byte_count_limit() {
    let mut ch = IdeChannel::new();
    ch.device_mut().insert(data_disc(8));
    // Clear the post-insert unit attention with a TEST UNIT READY.
    ch.write_port(SECONDARY_CMD_BASE + 7, 0xA0);
    for b in [0u8; 12] {
        ch.write_port(SECONDARY_CMD_BASE, b);
    }

    // Discard the TUR completion interrupt so the block IRQs below are the
    // only ones the assertions see.
    ch.take_irq();

    // Read two sectors so the data-in buffer is larger than one limit block.
    // The limit is deliberately NOT a divisor of the total, so the final block
    // is a short remainder (1500, 1500, 1096 over 4096) exercising the partial
    // block path, not just full-limit blocks.
    let sectors = 2usize;
    let total = sectors * DATA_SECTOR; // 4096
    let limit = 1500usize;

    // Program a byte-count limit smaller than the data before PACKET.
    ch.write_port(SECONDARY_CMD_BASE + 4, (limit & 0xFF) as u8); // cyl low
    ch.write_port(SECONDARY_CMD_BASE + 5, (limit >> 8) as u8); // cyl high
    ch.write_port(SECONDARY_CMD_BASE + 7, 0xA0); // PACKET
    let mut cdb = [0u8; 12];
    cdb[0] = 0x28; // READ(10)
    cdb[2..6].copy_from_slice(&0u32.to_be_bytes()); // lba 0
    cdb[7] = 0;
    cdb[8] = sectors as u8;
    for b in cdb {
        ch.write_port(SECONDARY_CMD_BASE, b);
    }

    // The first DRQ block arms an interrupt.
    assert!(ch.take_irq(), "the first data block raises IRQ15");

    // Drain block by block. Each block's byte count is the limit, except the
    // last, which is the remainder. Each new block re-raises the interrupt.
    let mut out = Vec::with_capacity(total);
    let mut drained = 0usize;
    while drained < total {
        assert_eq!(ch.status & status::DRQ, status::DRQ);
        let count = u16::from_le_bytes([ch.lba_mid, ch.lba_high]) as usize;
        let expected = (total - drained).min(limit);
        assert_eq!(count, expected);
        for _ in 0..count {
            out.push(ch.read_port(SECONDARY_CMD_BASE).unwrap());
        }
        drained += count;
        if drained < total {
            assert!(ch.take_irq(), "each new data block re-raises IRQ15");
        }
    }

    // After the last block, DRQ drops and the channel is idle/ready.
    assert_eq!(ch.status & status::DRQ, 0);
    assert_eq!(ch.status & status::DRDY, status::DRDY);
    // The reassembled data matches the two sectors read from lba 0.
    assert_eq!(out.len(), total);
    assert_eq!(out[0], 0x50); // sector 0 marker
    assert_eq!(out[DATA_SECTOR], 0x51); // sector 1 marker
}

#[test]
fn interrupt_reason_walks_the_packet_phases() {
    use crate::atapi::interrupt_reason as ir;
    const SECTOR_COUNT: u16 = SECONDARY_CMD_BASE + 2; // interrupt-reason register

    let mut ch = IdeChannel::new();
    ch.device_mut().insert(data_disc(8));
    // Clear the post-insert unit attention with a TEST UNIT READY.
    ch.write_port(SECONDARY_CMD_BASE + 7, 0xA0);
    for b in [0u8; 12] {
        ch.write_port(SECONDARY_CMD_BASE, b);
    }

    // PACKET (0xA0): awaiting the CDB. C/D=1, I/O=0 (command, from host).
    ch.write_port(SECONDARY_CMD_BASE + 7, 0xA0);
    assert_eq!(ch.read_port(SECTOR_COUNT).unwrap(), ir::AWAIT_PACKET);
    assert_eq!(ir::AWAIT_PACKET, 0x01);

    // Feed a READ(10) of one sector. The data phase arms: C/D=0, I/O=1.
    let mut cdb = [0u8; 12];
    cdb[0] = 0x28;
    cdb[5] = 0; // lba 0
    cdb[8] = 1; // one sector
    for b in cdb {
        ch.write_port(SECONDARY_CMD_BASE, b);
    }
    assert_eq!(ch.status & status::DRQ, status::DRQ);
    assert_eq!(ch.read_port(SECTOR_COUNT).unwrap(), ir::DATA_IN);
    assert_eq!(ir::DATA_IN, 0x02);

    // Drain the data block. The reason holds at data-in until the last byte.
    for _ in 0..DATA_SECTOR {
        ch.read_port(SECONDARY_CMD_BASE).unwrap();
    }

    // Transfer complete: C/D=1, I/O=1 (status phase).
    assert_eq!(ch.status & status::DRQ, 0);
    assert_eq!(ch.read_port(SECTOR_COUNT).unwrap(), ir::COMMAND_COMPLETE);
    assert_eq!(ir::COMMAND_COMPLETE, 0x03);
}
