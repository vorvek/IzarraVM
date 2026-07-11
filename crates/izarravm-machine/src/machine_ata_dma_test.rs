// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const BMIDE_BASE: u16 = 0xf000;
const BM_COMMAND: u16 = BMIDE_BASE;
const BM_STATUS: u16 = BMIDE_BASE + 2;
const BM_PRD: u16 = BMIDE_BASE + 4;
const ATA_STATUS: u16 = ata::PRIMARY_CMD_BASE + 7;
const STATUS_BSY: u8 = 0x80;
const STATUS_DRQ: u8 = 0x08;

fn out(machine: &mut Machine, port: u16, width: BusWidth, value: u32) {
    with_bus(machine, |bus| {
        bus.write_io(port, width, value, false).unwrap();
    });
}

fn input(machine: &mut Machine, port: u16, width: BusWidth) -> u32 {
    with_bus(machine, |bus| bus.read_io(port, width, 0, false).unwrap())
}

fn program_lba(machine: &mut Machine, lba: u32, sectors: u8, command: u8) {
    out(
        machine,
        ata::PRIMARY_CMD_BASE + 2,
        BusWidth::Byte,
        u32::from(sectors),
    );
    out(
        machine,
        ata::PRIMARY_CMD_BASE + 3,
        BusWidth::Byte,
        lba & 0xff,
    );
    out(
        machine,
        ata::PRIMARY_CMD_BASE + 4,
        BusWidth::Byte,
        (lba >> 8) & 0xff,
    );
    out(
        machine,
        ata::PRIMARY_CMD_BASE + 5,
        BusWidth::Byte,
        (lba >> 16) & 0xff,
    );
    out(
        machine,
        ata::PRIMARY_CMD_BASE + 6,
        BusWidth::Byte,
        0x40 | ((lba >> 24) & 0x0f),
    );
    out(machine, ATA_STATUS, BusWidth::Byte, u32::from(command));
}

fn arm_dma(machine: &mut Machine, memory: u32, lba: u32, read_from_disk: bool) -> u64 {
    const PRD: u32 = 0x1000;
    machine.write_physical_u32(PRD, memory);
    machine.write_physical_u32(PRD + 4, 0x8000_0200);
    out(machine, BM_PRD, BusWidth::Dword, PRD);
    out(
        machine,
        BM_COMMAND,
        BusWidth::Byte,
        if read_from_disk { 0x09 } else { 0x01 },
    );
    program_lba(machine, lba, 1, if read_from_disk { 0xc8 } else { 0xca });
    machine.bmide.ticks_until_completion().unwrap()
}

fn enable_irq14_wake(machine: &mut Machine) {
    machine.cpu.registers.eflags |= 0x0200;
    for (port, value) in [
        (0x20, 0x11),
        (0x21, 0x08),
        (0x21, 0x04),
        (0x21, 0x01),
        (0xa0, 0x11),
        (0xa1, 0x70),
        (0xa1, 0x02),
        (0xa1, 0x01),
        (0x21, 0xfb),
        (0xa1, 0xbf),
    ] {
        out(machine, port, BusWidth::Byte, value);
    }
}

#[test]
fn piix4_ide_function_exposes_bar4_and_honors_io_decode() {
    let mut machine = machine_with_hdd(8);
    out(&mut machine, 0xcf8, BusWidth::Dword, 0x8000_3900);
    assert_eq!(input(&mut machine, 0xcfc, BusWidth::Dword), 0x7111_8086);

    out(&mut machine, 0xcf8, BusWidth::Dword, 0x8000_3920);
    assert_eq!(input(&mut machine, 0xcfc, BusWidth::Dword), 0x0000_f001);

    out(&mut machine, 0xcf8, BusWidth::Dword, 0x8000_3904);
    out(&mut machine, 0xcfc, BusWidth::Word, 0x0004);
    assert_eq!(input(&mut machine, BM_STATUS, BusWidth::Byte), 0xff);
    out(&mut machine, 0xcfc, BusWidth::Word, 0x0005);
    assert_eq!(input(&mut machine, BM_STATUS, BusWidth::Byte), 0x00);
}

#[test]
fn secondary_pio_interrupt_latches_in_the_bmide_status_bank() {
    let mut machine = machine_with_hdd(8);
    out(
        &mut machine,
        ide::SECONDARY_CMD_BASE + 7,
        BusWidth::Byte,
        0xa1,
    );
    let deadline = machine.ide.ticks_until_completion().unwrap();
    machine.advance_devices_ticks(deadline - 1);
    assert_eq!(
        input(&mut machine, BMIDE_BASE + 10, BusWidth::Byte) & 0x04,
        0
    );
    machine.advance_devices_ticks(1);
    assert_ne!(
        input(&mut machine, BMIDE_BASE + 10, BusWidth::Byte) & 0x04,
        0
    );
}

#[test]
fn dma_read_lands_on_its_exact_master_tick_across_a_live_mode_switch() {
    let mut machine = machine_with_hdd(8);
    machine.set_mode(GswMode::Gsw586);
    let deadline = arm_dma(&mut machine, 0x2000, 2, true);
    assert_eq!(
        input(&mut machine, ATA_STATUS, BusWidth::Byte) as u8,
        STATUS_BSY
    );
    assert_eq!(input(&mut machine, BM_STATUS, BusWidth::Byte) & 1, 1);
    assert_eq!(machine.read_physical_u8(0x2000), 0);
    let expected_cap = machine
        .timeline
        .cpu_clocks_for_master_ticks_ceil(deadline)
        .max(1);
    assert_eq!(machine.event_batch_cap(u64::MAX), expected_cap);

    let generation = machine.cpu.decode_cache_generation();
    machine.advance_devices_ticks(deadline - 1);
    assert_eq!(machine.read_physical_u8(0x2000), 0);
    assert_eq!(machine.cpu.decode_cache_generation(), generation);
    machine.set_mode(GswMode::Gsw386Slow);
    let generation = machine.cpu.decode_cache_generation();
    machine.advance_devices_ticks(1);

    assert_eq!(machine.read_physical_u8(0x2000), 0x12);
    assert_eq!(
        machine.cpu.decode_cache_generation(),
        generation,
        "DMA into unrelated data must preserve decoded and compiled code"
    );
    assert_eq!(input(&mut machine, BM_STATUS, BusWidth::Byte) & 0x05, 0x04);
    assert_eq!(
        input(&mut machine, ATA_STATUS, BusWidth::Byte) as u8 & STATUS_BSY,
        0
    );
    assert_eq!(
        input(&mut machine, ata::PRIMARY_CMD_BASE + 2, BusWidth::Byte),
        0
    );
    assert_eq!(
        input(&mut machine, ata::PRIMARY_CMD_BASE + 3, BusWidth::Byte),
        3
    );
}

#[test]
fn pio_read_raises_one_ide_interrupt_at_each_sector_boundary() {
    let mut machine = machine_with_hdd(8);
    program_lba(&mut machine, 0, 2, 0x20);
    let first = machine
        .ata
        .as_ref()
        .and_then(ata::AtaDisk::ticks_until_completion)
        .unwrap();
    machine.advance_devices_ticks(first - 1);
    assert_eq!(
        input(&mut machine, ATA_STATUS, BusWidth::Byte) as u8,
        STATUS_BSY
    );
    machine.advance_devices_ticks(1);
    assert_ne!(
        input(&mut machine, ATA_STATUS, BusWidth::Byte) as u8 & STATUS_DRQ,
        0
    );
    assert_ne!(input(&mut machine, BM_STATUS, BusWidth::Byte) & 0x04, 0);
    out(&mut machine, BM_STATUS, BusWidth::Byte, 0x04);

    let first_word = input(&mut machine, ata::PRIMARY_CMD_BASE, BusWidth::Dword);
    for _ in 1..128 {
        input(&mut machine, ata::PRIMARY_CMD_BASE, BusWidth::Dword);
    }
    assert_eq!(first_word as u8, 0x10);
    assert_eq!(
        input(&mut machine, ATA_STATUS, BusWidth::Byte) as u8,
        STATUS_BSY
    );
    assert_eq!(
        input(&mut machine, ata::PRIMARY_CMD_BASE + 2, BusWidth::Byte),
        1
    );
    assert_eq!(
        input(&mut machine, ata::PRIMARY_CMD_BASE + 3, BusWidth::Byte),
        1
    );

    let second = machine
        .ata
        .as_ref()
        .and_then(ata::AtaDisk::ticks_until_completion)
        .unwrap();
    machine.advance_devices_ticks(second);
    assert_ne!(input(&mut machine, BM_STATUS, BusWidth::Byte) & 0x04, 0);
    let second_word = input(&mut machine, ata::PRIMARY_CMD_BASE, BusWidth::Dword);
    for _ in 1..128 {
        input(&mut machine, ata::PRIMARY_CMD_BASE, BusWidth::Dword);
    }
    assert_eq!(second_word as u8, 0x11);
    assert_eq!(
        input(&mut machine, ATA_STATUS, BusWidth::Byte) as u8 & STATUS_DRQ,
        0
    );
    assert_eq!(
        input(&mut machine, ata::PRIMARY_CMD_BASE + 2, BusWidth::Byte),
        0
    );
    assert_eq!(
        input(&mut machine, ata::PRIMARY_CMD_BASE + 3, BusWidth::Byte),
        2
    );
}

#[test]
fn live_cpu_switch_does_not_move_a_pio_deadline() {
    let mut machine = machine_with_hdd(8);
    machine.set_mode(GswMode::Gsw586);
    out(&mut machine, ATA_STATUS, BusWidth::Byte, 0xec);
    let deadline = machine
        .ata
        .as_ref()
        .and_then(ata::AtaDisk::ticks_until_completion)
        .unwrap();
    let first = deadline / 2;
    machine.advance_devices_ticks(first);
    machine.set_mode(GswMode::Gsw386Slow);
    assert_eq!(
        machine
            .ata
            .as_ref()
            .and_then(ata::AtaDisk::ticks_until_completion),
        Some(deadline - first)
    );
    machine.advance_devices_ticks(deadline - first - 1);
    assert_eq!(
        input(&mut machine, ATA_STATUS, BusWidth::Byte) as u8,
        STATUS_BSY
    );
    machine.advance_devices_ticks(1);
    assert_ne!(
        input(&mut machine, ATA_STATUS, BusWidth::Byte) as u8 & STATUS_DRQ,
        0
    );
}

#[test]
fn dma_write_is_visible_through_the_int13_disk_path() {
    let mut machine = machine_with_hdd(8);
    for offset in 0..ata::SECTOR as u32 {
        machine.write_physical_u8(0x3000 + offset, (offset as u8) ^ 0xa5);
    }
    let deadline = arm_dma(&mut machine, 0x3000, 0, false);
    machine.advance_devices_ticks(deadline);

    machine.cpu.registers.set_eax(0x0201);
    machine.cpu.registers.set_ecx(0x0001);
    machine.cpu.registers.set_edx(0x0080);
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x4000));
    machine.cpu.registers.set_ebx(0);
    machine.handle_int13();

    assert_eq!((machine.cpu.registers.eax() >> 8) as u8, 0);
    for offset in 0..ata::SECTOR as u32 {
        assert_eq!(
            machine.read_physical_u8(0x4_0000 + offset),
            (offset as u8) ^ 0xa5
        );
    }
}

#[test]
fn halted_cpu_wake_uses_the_primary_ide_deadline_in_every_mode() {
    for mode in [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ] {
        let mut machine = machine_with_hdd(8);
        machine.set_mode(mode);
        enable_irq14_wake(&mut machine);
        out(&mut machine, ATA_STATUS, BusWidth::Byte, 0xec);
        let ticks = machine
            .ata
            .as_ref()
            .and_then(ata::AtaDisk::ticks_until_completion)
            .unwrap();
        let expected = machine
            .timeline
            .cpu_clocks_for_master_ticks_ceil(ticks)
            .max(1);
        assert_eq!(
            machine.next_timer_wake(machine.master_ticks() + ticks),
            Some(expected),
            "{mode:?}"
        );
    }
}

#[test]
fn initial_write_drq_is_a_batch_deadline_but_not_a_halt_wake() {
    let mut machine = machine_with_hdd(8);
    machine.set_mode(GswMode::Gsw586);
    enable_irq14_wake(&mut machine);
    program_lba(&mut machine, 0, 1, 0x30);
    let ticks = machine
        .ata
        .as_ref()
        .and_then(ata::AtaDisk::ticks_until_completion)
        .unwrap();
    assert_eq!(
        machine.event_batch_cap(u64::MAX),
        machine.timeline.cpu_clocks_for_master_ticks_ceil(ticks)
    );
    assert_eq!(
        machine.next_timer_wake(machine.master_ticks() + ticks),
        None
    );
}
