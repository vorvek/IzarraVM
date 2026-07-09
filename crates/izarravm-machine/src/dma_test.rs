// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_bus::Memory;

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
    dma.write_port(0x8B, 0x01); // page -> byte base 0x01_0000 + (0x0010<<1)
    dma.write_port(0xD4, 0x01); // unmask slave ch1 (channel 5)

    // Seed two bytes at the word-aligned byte address.
    let byte_addr = (0x01u32 << 17) | (0x0010u32 << 1);
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
    dma.write_port(0x8B, 0x01); // page 0x01 -> byte base 0x2_0000
    dma.write_port(0xD4, 0x01); // unmask slave ch1

    let byte_addr = (0x01u32 << 17) | (0x0002u32 << 1);
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

// --- Slice 1: page register read-back ---

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

// --- Slice 2: status request-active bits ---

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

// --- Slice 3: mode register transfer-mode field ---

#[test]
fn set_mode_decodes_the_transfer_mode_field() {
    for (bits76, want) in [(0u8, 0u8), (1, 1), (2, 2), (3, 3)] {
        let mut ch = DmaChannel::default();
        // bits 6-7 carry the mode; keep the rest at a benign read encoding.
        ch.set_mode((bits76 << 6) | 0x08);
        assert_eq!(ch.transfer_mode, want, "mode bits {bits76:02b}");
    }
}

// --- Slice 4: device->memory write and verify datapaths ---

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
        page: 0x01,
        mask: false,
        ..Default::default()
    };
    ch.set_mode(0x45); // single, write (kind 1)

    let byte_addr = ((0x01u32 << 17) | (0x0008u32 << 1)) as usize;
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
    let mut mem = Memory::new(0x0040 + 2).unwrap();
    chip.write_byte(2, &mut mem, 0x5A).unwrap();
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
    chip.verify(3).unwrap();
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
    let byte_addr = (0x0004u32 << 1) as usize;
    let mut mem = Memory::new(byte_addr + 4).unwrap();
    chip.write_word(1, &mut mem, 0x1234).unwrap();
    assert_eq!(mem.read_u8(byte_addr).unwrap(), 0x34);
    assert_eq!(mem.read_u8(byte_addr + 1).unwrap(), 0x12);
    assert_eq!(
        chip.read_local(8),
        Some(0x02),
        "ch1 TC latched by word write"
    );
}

// --- Slice 5: command register and memory-to-memory transfer ---

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
}
