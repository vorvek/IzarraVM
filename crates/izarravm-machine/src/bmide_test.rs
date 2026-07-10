// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::ata::{PRIMARY_CMD_BASE, PRIMARY_CTRL, SECTOR};

const BASE: u16 = 0xf000;
const TABLE: usize = 0x1000;
const BUFFER: usize = 0x20_000;

fn marked_disk(sectors: usize) -> AtaDisk {
    let mut bytes = vec![0u8; sectors * SECTOR];
    for sector in 0..sectors {
        for byte in &mut bytes[sector * SECTOR..(sector + 1) * SECTOR] {
            *byte = sector as u8 ^ 0x5a;
        }
    }
    AtaDisk::new(bytes)
}

fn program_lba(disk: &mut AtaDisk, lba: u32, count: u8) {
    disk.write_port(PRIMARY_CMD_BASE + 2, count);
    disk.write_port(PRIMARY_CMD_BASE + 3, lba as u8);
    disk.write_port(PRIMARY_CMD_BASE + 4, (lba >> 8) as u8);
    disk.write_port(PRIMARY_CMD_BASE + 5, (lba >> 16) as u8);
    disk.write_port(PRIMARY_CMD_BASE + 6, 0x40 | ((lba >> 24) as u8 & 0x0f));
}

fn write_prd(memory: &mut Memory, index: usize, address: u32, count: u16, end: bool) {
    let entry = TABLE + index * 8;
    memory.write_u32(entry, address).unwrap();
    memory
        .write_u32(entry + 4, u32::from(count) | if end { PRD_EOT } else { 0 })
        .unwrap();
}

fn write_bm(
    controller: &mut BusMasterIde,
    memory: &Memory,
    disk: Option<&mut AtaDisk>,
    port: u16,
    width: BusWidth,
    value: u32,
) {
    match disk {
        Some(disk) => {
            controller.write_io(port, width, value, Some(&mut *disk), BASE);
            controller.synchronize(true, memory, disk);
        }
        None => controller.write_io(port, width, value, None, BASE),
    }
}

fn set_prd(controller: &mut BusMasterIde, memory: &Memory, disk: &mut AtaDisk) {
    write_bm(
        controller,
        memory,
        Some(disk),
        BASE + 4,
        BusWidth::Dword,
        TABLE as u32,
    );
}

fn bm_status(controller: &BusMasterIde) -> u8 {
    controller.read_io(BASE + 2, BusWidth::Byte, BASE) as u8
}

fn ata_status(disk: &mut AtaDisk) -> u8 {
    disk.read_port(PRIMARY_CTRL).unwrap()
}

fn active_read(start_first: bool) -> (BusMasterIde, Memory, AtaDisk) {
    let mut controller = BusMasterIde::default();
    let mut memory = Memory::new(2 * 1024 * 1024).unwrap();
    let mut disk = marked_disk(64);
    write_prd(&mut memory, 0, BUFFER as u32, SECTOR as u16, true);
    set_prd(&mut controller, &memory, &mut disk);
    program_lba(&mut disk, 3, 1);
    if start_first {
        write_bm(
            &mut controller,
            &memory,
            Some(&mut disk),
            BASE,
            BusWidth::Byte,
            u32::from(COMMAND_START | COMMAND_READ_FROM_DISK),
        );
    }
    disk.write_port(PRIMARY_CMD_BASE + 7, 0xc8);
    controller.synchronize(true, &memory, &mut disk);
    if !start_first {
        write_bm(
            &mut controller,
            &memory,
            Some(&mut disk),
            BASE,
            BusWidth::Byte,
            u32::from(COMMAND_START | COMMAND_READ_FROM_DISK),
        );
    }
    (controller, memory, disk)
}

#[test]
fn read_dma_is_asynchronous_and_supports_both_arming_orders() {
    for start_first in [false, true] {
        let (mut controller, mut memory, mut disk) = active_read(start_first);
        assert_eq!(bm_status(&controller) & STATUS_ACTIVE, STATUS_ACTIVE);
        assert_eq!(ata_status(&mut disk) & 0x80, 0x80, "ATA stays busy");
        assert_eq!(memory.read_u8(BUFFER).unwrap(), 0);

        let deadline = controller.ticks_until_completion().unwrap();
        assert!(!controller.advance_master_ticks(deadline - 1, &mut memory, &mut disk));
        assert_eq!(memory.read_u8(BUFFER).unwrap(), 0);
        assert!(controller.advance_master_ticks(1, &mut memory, &mut disk));
        assert_eq!(memory.read_u8(BUFFER).unwrap(), 3 ^ 0x5a);
        assert_eq!(bm_status(&controller) & STATUS_ACTIVE, 0);
        assert!(disk.take_irq());
        controller.note_ide_irq(false);
        assert_eq!(bm_status(&controller) & STATUS_INTERRUPT, STATUS_INTERRUPT);
        assert_eq!(ata_status(&mut disk) & 0x40, 0x40, "ATA returns ready");

        write_bm(
            &mut controller,
            &memory,
            Some(&mut disk),
            BASE + 2,
            BusWidth::Byte,
            u32::from(STATUS_ERROR | STATUS_INTERRUPT),
        );
        assert_eq!(
            bm_status(&controller) & (STATUS_ERROR | STATUS_INTERRUPT),
            0
        );
        assert_eq!(bm_status(&controller) & STATUS_DRIVE0_DMA, 0);
    }
}

#[test]
fn write_dma_gathers_multiple_prds_without_a_pio_loop() {
    let mut controller = BusMasterIde::default();
    let mut memory = Memory::new(2 * 1024 * 1024).unwrap();
    let mut disk = marked_disk(64);
    let payload: Vec<u8> = (0..SECTOR * 2).map(|index| index as u8 ^ 0xa7).collect();
    memory.as_mut_slice()[BUFFER..BUFFER + payload.len()].copy_from_slice(&payload);
    write_prd(&mut memory, 0, BUFFER as u32, 256, false);
    write_prd(&mut memory, 1, (BUFFER + 256) as u32, 768, true);
    set_prd(&mut controller, &memory, &mut disk);
    program_lba(&mut disk, 9, 2);
    disk.write_port(PRIMARY_CMD_BASE + 7, 0xca);
    controller.synchronize(true, &memory, &mut disk);
    write_bm(
        &mut controller,
        &memory,
        Some(&mut disk),
        BASE,
        BusWidth::Byte,
        u32::from(COMMAND_START),
    );

    let deadline = controller.ticks_until_completion().unwrap();
    assert!(!controller.advance_master_ticks(deadline, &mut memory, &mut disk));
    let mut written = Vec::new();
    written.extend_from_slice(&disk.read_lba(9).unwrap());
    written.extend_from_slice(&disk.read_lba(10).unwrap());
    assert_eq!(written, payload);
    assert!(disk.dirty);
}

#[test]
fn master_tick_splitting_does_not_change_completion() {
    let (mut one, mut one_memory, mut one_disk) = active_read(false);
    let (mut split, mut split_memory, mut split_disk) = active_read(false);
    let deadline = one.ticks_until_completion().unwrap();
    one.advance_master_ticks(deadline, &mut one_memory, &mut one_disk);
    let first = deadline / 3;
    let second = deadline / 5;
    split.advance_master_ticks(first, &mut split_memory, &mut split_disk);
    split.advance_master_ticks(second, &mut split_memory, &mut split_disk);
    split.advance_master_ticks(
        deadline - first - second,
        &mut split_memory,
        &mut split_disk,
    );
    assert_eq!(
        &one_memory.as_slice()[BUFFER..BUFFER + SECTOR],
        &split_memory.as_slice()[BUFFER..BUFFER + SECTOR]
    );
    assert_eq!(bm_status(&one), bm_status(&split));
    assert_eq!(ata_status(&mut one_disk), ata_status(&mut split_disk));
}

#[test]
fn prd_pointer_is_dword_aligned_and_transfer_stops_at_the_ata_byte_count() {
    let mut controller = BusMasterIde::default();
    let mut memory = Memory::new(2 * 1024 * 1024).unwrap();
    let mut disk = marked_disk(64);
    memory.write_u8(BUFFER + SECTOR, 0xcc).unwrap();
    write_prd(&mut memory, 0, BUFFER as u32, (SECTOR * 2) as u16, true);
    write_bm(
        &mut controller,
        &memory,
        Some(&mut disk),
        BASE + 4,
        BusWidth::Dword,
        TABLE as u32 + 3,
    );
    assert_eq!(
        controller.read_io(BASE + 4, BusWidth::Dword, BASE),
        TABLE as u32
    );
    program_lba(&mut disk, 2, 1);
    disk.write_port(PRIMARY_CMD_BASE + 7, 0xc8);
    write_bm(
        &mut controller,
        &memory,
        Some(&mut disk),
        BASE,
        BusWidth::Byte,
        u32::from(COMMAND_START | COMMAND_READ_FROM_DISK),
    );
    let deadline = controller.ticks_until_completion().unwrap();
    controller.advance_master_ticks(deadline, &mut memory, &mut disk);
    assert_eq!(memory.read_u8(BUFFER).unwrap(), 2 ^ 0x5a);
    assert_eq!(memory.read_u8(BUFFER + SECTOR).unwrap(), 0xcc);
    assert_eq!(bm_status(&controller) & STATUS_ACTIVE, STATUS_ACTIVE);
    assert!(disk.take_irq());
    controller.note_ide_irq(false);
    write_bm(
        &mut controller,
        &memory,
        Some(&mut disk),
        BASE,
        BusWidth::Byte,
        0,
    );
    assert_eq!(bm_status(&controller) & STATUS_ACTIVE, 0);
    assert_eq!(bm_status(&controller) & STATUS_ERROR, 0);
}

#[test]
fn region_address_bit1_follows_piix4_read_and_write_semantics() {
    // Disk to memory honors A1, so the lower word of the first dword is not
    // touched when the region starts at address 2 mod 4.
    let mut read_controller = BusMasterIde::default();
    let mut read_memory = Memory::new(2 * 1024 * 1024).unwrap();
    let mut read_disk = marked_disk(64);
    read_memory.write_u8(BUFFER, 0xaa).unwrap();
    read_memory.write_u8(BUFFER + 1, 0xbb).unwrap();
    write_prd(&mut read_memory, 0, BUFFER as u32 + 2, SECTOR as u16, true);
    set_prd(&mut read_controller, &read_memory, &mut read_disk);
    program_lba(&mut read_disk, 4, 1);
    read_disk.write_port(PRIMARY_CMD_BASE + 7, 0xc8);
    write_bm(
        &mut read_controller,
        &read_memory,
        Some(&mut read_disk),
        BASE,
        BusWidth::Byte,
        u32::from(COMMAND_START | COMMAND_READ_FROM_DISK),
    );
    let deadline = read_controller.ticks_until_completion().unwrap();
    read_controller.advance_master_ticks(deadline, &mut read_memory, &mut read_disk);
    assert_eq!(&read_memory.as_slice()[BUFFER..BUFFER + 2], &[0xaa, 0xbb]);
    assert_eq!(read_memory.read_u8(BUFFER + 2).unwrap(), 4 ^ 0x5a);

    // Memory to disk masks A1 and asserts all byte enables, so the first source
    // byte comes from the aligned dword rather than the programmed address +2.
    let mut write_controller = BusMasterIde::default();
    let mut write_memory = Memory::new(2 * 1024 * 1024).unwrap();
    let mut write_disk = marked_disk(64);
    let payload: Vec<u8> = (0..SECTOR).map(|index| index as u8 ^ 0xc3).collect();
    write_memory.as_mut_slice()[BUFFER..BUFFER + SECTOR].copy_from_slice(&payload);
    write_prd(&mut write_memory, 0, BUFFER as u32 + 2, SECTOR as u16, true);
    set_prd(&mut write_controller, &write_memory, &mut write_disk);
    program_lba(&mut write_disk, 5, 1);
    write_disk.write_port(PRIMARY_CMD_BASE + 7, 0xca);
    write_bm(
        &mut write_controller,
        &write_memory,
        Some(&mut write_disk),
        BASE,
        BusWidth::Byte,
        u32::from(COMMAND_START),
    );
    let deadline = write_controller.ticks_until_completion().unwrap();
    write_controller.advance_master_ticks(deadline, &mut write_memory, &mut write_disk);
    assert_eq!(&write_disk.read_lba(5).unwrap()[..], payload.as_slice());
}

#[test]
fn invalid_or_short_prds_abort_without_touching_memory() {
    let cases = [
        (BUFFER as u32 | 1, 512, true),
        (BUFFER as u32, 511, true),
        (0x0002_ff00, 512, true),
        (BUFFER as u32, 256, true),
        (0x0020_0000, 512, true),
    ];
    for (address, count, end) in cases {
        let mut controller = BusMasterIde::default();
        let mut memory = Memory::new(2 * 1024 * 1024).unwrap();
        let mut disk = marked_disk(64);
        write_prd(&mut memory, 0, address, count, end);
        set_prd(&mut controller, &memory, &mut disk);
        program_lba(&mut disk, 1, 1);
        disk.write_port(PRIMARY_CMD_BASE + 7, 0xc8);
        write_bm(
            &mut controller,
            &memory,
            Some(&mut disk),
            BASE,
            BusWidth::Byte,
            u32::from(COMMAND_START | COMMAND_READ_FROM_DISK),
        );
        assert_eq!(controller.ticks_until_completion(), None);
        assert_eq!(bm_status(&controller) & STATUS_ERROR, STATUS_ERROR);
        assert_eq!(bm_status(&controller) & STATUS_INTERRUPT, 0);
        assert!(disk.take_irq());
        controller.note_ide_irq(false);
        assert_eq!(bm_status(&controller) & STATUS_INTERRUPT, STATUS_INTERRUPT);
        assert_eq!(ata_status(&mut disk) & 0x01, 0x01);
        assert_eq!(memory.read_u8(BUFFER).unwrap(), 0);
    }
}

#[test]
fn status_capability_bits_are_software_controlled_and_irqs_latch_per_channel() {
    let mut controller = BusMasterIde::default();
    let memory = Memory::new(2 * 1024 * 1024).unwrap();
    assert_eq!(bm_status(&controller) & 0x60, 0);
    write_bm(
        &mut controller,
        &memory,
        None,
        BASE + 2,
        BusWidth::Byte,
        0x60,
    );
    write_bm(
        &mut controller,
        &memory,
        None,
        BASE + 10,
        BusWidth::Byte,
        0x20,
    );
    assert_eq!(bm_status(&controller) & 0x60, 0x60);
    assert_eq!(
        controller.read_io(BASE + 10, BusWidth::Byte, BASE) & 0x60,
        0x20
    );

    controller.note_ide_irq(false);
    controller.note_ide_irq(true);
    assert_eq!(bm_status(&controller) & STATUS_INTERRUPT, STATUS_INTERRUPT);
    assert_eq!(
        controller.read_io(BASE + 10, BusWidth::Byte, BASE) & u32::from(STATUS_INTERRUPT),
        u32::from(STATUS_INTERRUPT)
    );
    write_bm(
        &mut controller,
        &memory,
        None,
        BASE + 2,
        BusWidth::Byte,
        u32::from(STATUS_INTERRUPT | 0x60),
    );
    assert_eq!(bm_status(&controller) & STATUS_INTERRUPT, 0);
    assert_eq!(bm_status(&controller) & 0x60, 0x60);
}

#[test]
fn prd_table_cannot_continue_into_a_second_4k_page() {
    let mut controller = BusMasterIde::default();
    let mut memory = Memory::new(2 * 1024 * 1024).unwrap();
    let mut disk = marked_disk(64);
    memory.write_u32(0x1ff8, BUFFER as u32).unwrap();
    memory.write_u32(0x1ffc, 256).unwrap();
    memory.write_u32(0x2000, (BUFFER + 256) as u32).unwrap();
    memory.write_u32(0x2004, PRD_EOT | 256).unwrap();
    write_bm(
        &mut controller,
        &memory,
        Some(&mut disk),
        BASE + 4,
        BusWidth::Dword,
        0x1ff8,
    );
    program_lba(&mut disk, 1, 1);
    disk.write_port(PRIMARY_CMD_BASE + 7, 0xc8);
    write_bm(
        &mut controller,
        &memory,
        Some(&mut disk),
        BASE,
        BusWidth::Byte,
        u32::from(COMMAND_START | COMMAND_READ_FROM_DISK),
    );
    assert_eq!(controller.ticks_until_completion(), None);
    assert_eq!(bm_status(&controller) & STATUS_ERROR, STATUS_ERROR);
    assert_eq!(memory.read_u8(BUFFER).unwrap(), 0);
}

#[test]
fn zero_prd_count_means_64k_and_nien_only_masks_the_ata_irq() {
    let mut controller = BusMasterIde::default();
    let mut memory = Memory::new(2 * 1024 * 1024).unwrap();
    let mut disk = marked_disk(200);
    write_prd(&mut memory, 0, 0x1_0000, 0, true);
    set_prd(&mut controller, &memory, &mut disk);
    disk.write_port(PRIMARY_CTRL, 0x02);
    program_lba(&mut disk, 0, 128);
    disk.write_port(PRIMARY_CMD_BASE + 7, 0xc8);
    write_bm(
        &mut controller,
        &memory,
        Some(&mut disk),
        BASE,
        BusWidth::Byte,
        u32::from(COMMAND_START | COMMAND_READ_FROM_DISK),
    );
    let deadline = controller.ticks_until_completion().unwrap();
    controller.advance_master_ticks(deadline, &mut memory, &mut disk);
    assert_eq!(memory.read_u8(0x1_0000).unwrap(), 0x5a);
    assert!(!disk.take_irq(), "nIEN masks IRQ14");
    assert_eq!(bm_status(&controller) & STATUS_INTERRUPT, 0);
}

#[test]
fn disabling_or_stopping_an_active_transfer_aborts_it() {
    for disable_pci in [false, true] {
        let (mut controller, memory, mut disk) = active_read(false);
        if disable_pci {
            controller.synchronize(false, &memory, &mut disk);
        } else {
            write_bm(
                &mut controller,
                &memory,
                Some(&mut disk),
                BASE,
                BusWidth::Byte,
                0,
            );
        }
        assert_eq!(controller.ticks_until_completion(), None);
        assert_eq!(bm_status(&controller) & STATUS_ACTIVE, 0);
        assert_eq!(bm_status(&controller) & STATUS_ERROR, STATUS_ERROR);
        assert_eq!(ata_status(&mut disk) & 0x01, 0x01);
    }
}
