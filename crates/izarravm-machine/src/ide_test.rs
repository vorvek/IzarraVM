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

fn advance_next(ch: &mut IdeChannel) -> u64 {
    let ticks = ch.ticks_until_completion().expect("pending IDE deadline");
    ch.advance_master_ticks(ticks);
    ticks
}

fn begin_packet(ch: &mut IdeChannel) {
    ch.write_port(SECONDARY_CMD_BASE + 7, 0xA0);
    assert_eq!(ch.status & status::BSY, status::BSY);
    advance_next(ch);
    assert_eq!(ch.status & status::DRQ, status::DRQ);
    assert!(!ch.take_irq());
}

fn execute_cdb(ch: &mut IdeChannel, cdb: [u8; 12]) {
    begin_packet(ch);
    for byte in cdb {
        ch.write_port(SECONDARY_CMD_BASE, byte);
    }
    while ch.status & status::BSY != 0 {
        advance_next(ch);
    }
}

fn clear_unit_attention(ch: &mut IdeChannel) {
    execute_cdb(ch, [0u8; 12]);
    let _ = ch.take_irq();
}

/// Drive the full PACKET handshake for a READ(10) of one sector at `lba` and
/// return the drained data-in buffer.
fn packet_read10(ch: &mut IdeChannel, lba: u32) -> Vec<u8> {
    let mut cdb = [0u8; 12];
    cdb[0] = 0x28;
    cdb[2..6].copy_from_slice(&lba.to_be_bytes());
    cdb[7] = 0;
    cdb[8] = 1; // one sector
    execute_cdb(ch, cdb);
    // After the packet, data-in is armed and the byte count is set.
    let count = u16::from_le_bytes([ch.lba_mid, ch.lba_high]) as usize;
    assert_eq!(count, DATA_SECTOR);
    assert_eq!(ch.status & status::DRQ, status::DRQ);
    // Drain the data register.
    let mut out = Vec::with_capacity(count);
    while out.len() < DATA_SECTOR {
        let block = u16::from_le_bytes([ch.lba_mid, ch.lba_high]) as usize;
        for _ in 0..block {
            out.push(ch.read_port(SECONDARY_CMD_BASE).unwrap());
        }
        if ch.status & status::BSY != 0 {
            advance_next(ch);
        }
    }
    out
}

#[test]
fn packet_read10_round_trips_a_sector() {
    let mut ch = IdeChannel::new();
    ch.device_mut().insert(data_disc(8));
    clear_unit_attention(&mut ch);
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
    execute_cdb(&mut ch, [0u8; 12]);
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
    execute_cdb(&mut ch, [0u8; 12]);
    assert!(!ch.take_irq());
}

#[test]
fn identify_packet_device_returns_512_bytes() {
    let mut ch = IdeChannel::new();
    ch.write_port(SECONDARY_CMD_BASE + 7, 0xA1);
    assert_eq!(ch.status & status::BSY, status::BSY);
    advance_next(&mut ch);
    let count = u16::from_le_bytes([ch.lba_mid, ch.lba_high]) as usize;
    assert_eq!(count, 512);
    let mut block = Vec::new();
    for _ in 0..512 {
        block.push(ch.read_port(SECONDARY_CMD_BASE).unwrap());
    }
    // Word 0 low/high bytes: 0x85C0 little-endian.
    assert_eq!(block[0], 0xC0);
    assert_eq!(block[1], 0x85);
    let capabilities = u16::from_le_bytes([block[98], block[99]]);
    assert_eq!(capabilities & (1 << 8), 0, "ATAPI DMA is not advertised");
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
    assert_eq!(ch.status & status::BSY, status::BSY);
    advance_next(&mut ch);
    assert_eq!(ch.status & status::ERR, status::ERR);
}

#[test]
fn read10_reaches_each_boundary_on_its_exact_tick() {
    let mut ch = IdeChannel::new();
    ch.device_mut().insert(data_disc(8));
    clear_unit_attention(&mut ch);

    let mut cdb = [0u8; 12];
    cdb[0] = 0x28;
    cdb[8] = 1;
    begin_packet(&mut ch);
    for byte in cdb {
        ch.write_port(SECONDARY_CMD_BASE, byte);
    }
    let command_ticks = ch.ticks_until_completion().unwrap();
    ch.advance_master_ticks(command_ticks - 1);
    assert_eq!(ch.status & status::BSY, status::BSY);
    assert!(!ch.take_irq());
    ch.advance_master_ticks(1);
    let media_ticks = ch.ticks_until_completion().unwrap();
    ch.advance_master_ticks(media_ticks - 1);
    assert_eq!(ch.status & status::BSY, status::BSY);
    assert_eq!(ch.take_access_bytes(), 0);
    ch.advance_master_ticks(1);
    assert_eq!(ch.status & status::DRQ, status::DRQ);
    assert!(ch.take_irq());
    assert_eq!(ch.take_access_bytes(), DATA_SECTOR);
}

#[test]
fn splitting_master_time_does_not_move_a_read_boundary() {
    fn armed_read() -> IdeChannel {
        let mut ch = IdeChannel::new();
        ch.device_mut().insert(data_disc(8));
        clear_unit_attention(&mut ch);
        let mut cdb = [0u8; 12];
        cdb[0] = 0x28;
        cdb[8] = 1;
        begin_packet(&mut ch);
        for byte in cdb {
            ch.write_port(SECONDARY_CMD_BASE, byte);
        }
        ch
    }

    let mut whole = armed_read();
    let mut split = armed_read();
    let elapsed = COMMAND_LATENCY_TICKS + SPIN_UP_TICKS + sector_transfer_ticks();
    whole.advance_master_ticks(elapsed);
    for part in [
        17,
        elapsed / 3,
        elapsed / 5,
        elapsed - 17 - elapsed / 3 - elapsed / 5,
    ] {
        split.advance_master_ticks(part);
    }

    assert_eq!(whole.status, split.status);
    assert_eq!(whole.phase, split.phase);
    assert_eq!(whole.data_in_pos, split.data_in_pos);
    assert_eq!(whole.data_in_ready_end, split.data_in_ready_end);
    assert_eq!(
        whole.ticks_until_completion(),
        split.ticks_until_completion()
    );
    assert_eq!(whole.take_access_bytes(), split.take_access_bytes());
    assert_eq!(whole.take_irq(), split.take_irq());
}

#[test]
fn nop_command_always_aborts() {
    let mut ch = IdeChannel::new();
    ch.write_port(SECONDARY_CMD_BASE + 7, 0x00); // NOP
    assert_eq!(ch.status & status::BSY, status::BSY);
    advance_next(&mut ch);
    assert_eq!(ch.status & status::DRDY, status::DRDY);
    assert_eq!(ch.status & status::ERR, status::ERR);
    assert_eq!(ch.error, 0x04);
}

#[test]
fn execute_diagnostic_passes_with_atapi_signature() {
    let mut ch = IdeChannel::new();
    ch.write_port(SECONDARY_CMD_BASE + 7, 0x90); // EXECUTE DEVICE DIAGNOSTIC
    assert_eq!(ch.status & status::BSY, status::BSY);
    advance_next(&mut ch);
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
    clear_unit_attention(&mut ch);

    // Discard the TUR completion interrupt so the block IRQs below are the
    // only ones the assertions see.
    ch.take_irq();

    // Read two sectors with a limit that splits each ready sector into 1500 and
    // 548 byte blocks.
    let sectors = 2usize;
    let total = sectors * DATA_SECTOR; // 4096
    let limit = 1500usize;

    // Program a byte-count limit smaller than the data before PACKET.
    ch.write_port(SECONDARY_CMD_BASE + 4, (limit & 0xFF) as u8); // cyl low
    ch.write_port(SECONDARY_CMD_BASE + 5, (limit >> 8) as u8); // cyl high
    begin_packet(&mut ch);
    let mut cdb = [0u8; 12];
    cdb[0] = 0x28; // READ(10)
    cdb[2..6].copy_from_slice(&0u32.to_be_bytes()); // lba 0
    cdb[7] = 0;
    cdb[8] = sectors as u8;
    for b in cdb {
        ch.write_port(SECONDARY_CMD_BASE, b);
    }
    while ch.status & status::BSY != 0 {
        advance_next(&mut ch);
    }

    // The first DRQ block arms an interrupt.
    assert!(ch.take_irq(), "the first data block raises IRQ15");

    // Drain block by block. The next sector remains BSY until its transfer
    // boundary, while byte-count subblocks within one sector re-arm immediately.
    let mut out = Vec::with_capacity(total);
    let mut drained = 0usize;
    while drained < total {
        assert_eq!(ch.status & status::DRQ, status::DRQ);
        let count = u16::from_le_bytes([ch.lba_mid, ch.lba_high]) as usize;
        let ready_remaining = DATA_SECTOR - (drained % DATA_SECTOR);
        let expected = (total - drained).min(limit).min(ready_remaining);
        assert_eq!(count, expected);
        for _ in 0..count {
            out.push(ch.read_port(SECONDARY_CMD_BASE).unwrap());
        }
        drained += count;
        if drained < total {
            if ch.status & status::BSY != 0 {
                assert!(!ch.take_irq());
                advance_next(&mut ch);
            }
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
    clear_unit_attention(&mut ch);

    // PACKET (0xA0): awaiting the CDB. C/D=1, I/O=0 (command, from host).
    begin_packet(&mut ch);
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
    while ch.status & status::BSY != 0 {
        advance_next(&mut ch);
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
