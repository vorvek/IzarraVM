// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const STATUS_BSY: u8 = 0x80;
const STATUS_DRQ: u8 = 0x08;
const SECONDARY_BM_STATUS: u16 = 0xf00a;

fn out(machine: &mut Machine, port: u16, value: u8) {
    with_bus(machine, |bus| {
        bus.write_io(port, BusWidth::Byte, u32::from(value), false)
            .unwrap();
    });
}

fn input(machine: &mut Machine, port: u16) -> u8 {
    with_bus(machine, |bus| {
        bus.read_io(port, BusWidth::Byte, 0, false).unwrap() as u8
    })
}

fn data_disc(sectors: u32) -> CdImage {
    let mut bytes = vec![0u8; sectors as usize * cdimage::DATA_SECTOR];
    for sector in 0..sectors as usize {
        bytes[sector * cdimage::DATA_SECTOR] = 0x60u8.wrapping_add(sector as u8);
    }
    CdImage::from_iso(bytes).unwrap()
}

fn cd_machine(sectors: u32) -> Machine {
    let mut machine = int15_machine(16);
    machine.mount_cd(data_disc(sectors));
    machine
}

fn advance_ide_deadline(machine: &mut Machine) -> u64 {
    let ticks = machine.ide.ticks_until_completion().unwrap();
    machine.advance_devices_ticks(ticks);
    ticks
}

fn begin_packet(machine: &mut Machine) {
    out(machine, ide::SECONDARY_CMD_BASE + 7, 0xa0);
    advance_ide_deadline(machine);
    let _ = input(machine, ide::SECONDARY_CMD_BASE + 7);
}

fn send_cdb(machine: &mut Machine, cdb: [u8; 12]) {
    begin_packet(machine);
    for byte in cdb {
        out(machine, ide::SECONDARY_CMD_BASE, byte);
    }
}

fn clear_unit_attention(machine: &mut Machine) {
    send_cdb(machine, [0u8; 12]);
    advance_ide_deadline(machine);
    let _ = input(machine, ide::SECONDARY_CMD_BASE + 7);
}

fn enable_irq15_wake(machine: &mut Machine) {
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
        (0xa1, 0x7f),
    ] {
        out(machine, port, value);
    }
}

#[test]
fn packet_acceptance_and_completion_have_exact_irq15_ordering() {
    let mut machine = cd_machine(8);
    machine.set_mode(GswMode::Gsw586);
    out(&mut machine, ide::SECONDARY_CMD_BASE + 7, 0xa0);
    let ticks = machine.ide.ticks_until_completion().unwrap();
    let expected_cap = machine
        .timeline
        .cpu_clocks_for_master_ticks_ceil(ticks)
        .max(1);
    assert_eq!(machine.event_batch_cap(u64::MAX), expected_cap);
    assert_eq!(
        input(&mut machine, ide::SECONDARY_CTRL) & STATUS_BSY,
        STATUS_BSY
    );

    machine.advance_devices_ticks(ticks - 1);
    assert_eq!(
        input(&mut machine, ide::SECONDARY_CTRL) & STATUS_BSY,
        STATUS_BSY
    );
    assert_eq!(input(&mut machine, SECONDARY_BM_STATUS) & 0x04, 0);
    machine.advance_devices_ticks(1);
    assert_eq!(
        input(&mut machine, ide::SECONDARY_CTRL) & STATUS_DRQ,
        STATUS_DRQ
    );
    assert_eq!(input(&mut machine, SECONDARY_BM_STATUS) & 0x04, 0);

    for byte in [0u8; 12] {
        out(&mut machine, ide::SECONDARY_CMD_BASE, byte);
    }
    let completion = machine.ide.ticks_until_completion().unwrap();
    machine.advance_devices_ticks(completion - 1);
    assert_eq!(input(&mut machine, SECONDARY_BM_STATUS) & 0x04, 0);
    machine.advance_devices_ticks(1);
    assert_ne!(input(&mut machine, SECONDARY_BM_STATUS) & 0x04, 0);
}

#[test]
fn read_deadlines_keep_master_ticks_across_a_live_cpu_switch() {
    let mut machine = cd_machine(8);
    clear_unit_attention(&mut machine);
    machine.set_mode(GswMode::Gsw586);
    let mut cdb = [0u8; 12];
    cdb[0] = 0x28;
    cdb[2..6].copy_from_slice(&2u32.to_be_bytes());
    cdb[8] = 1;
    send_cdb(&mut machine, cdb);

    let command = machine.ide.ticks_until_completion().unwrap();
    let first = command / 2;
    machine.advance_devices_ticks(first);
    machine.set_mode(GswMode::Gsw386Slow);
    assert_eq!(machine.ide.ticks_until_completion(), Some(command - first));
    machine.advance_devices_ticks(command - first);

    let media = machine.ide.ticks_until_completion().unwrap();
    let expected_cap = machine
        .timeline
        .cpu_clocks_for_master_ticks_ceil(media)
        .max(1);
    assert!(machine.event_batch_cap(u64::MAX) <= expected_cap);
    machine.advance_devices_ticks(media - 1);
    assert_eq!(
        input(&mut machine, ide::SECONDARY_CTRL) & STATUS_BSY,
        STATUS_BSY
    );
    machine.advance_devices_ticks(1);
    assert_eq!(
        input(&mut machine, ide::SECONDARY_CTRL) & STATUS_DRQ,
        STATUS_DRQ
    );
    assert_eq!(input(&mut machine, ide::SECONDARY_CMD_BASE), 0x62);
}

#[test]
fn halted_cpu_uses_the_secondary_ide_deadline_in_every_mode() {
    for mode in [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ] {
        let mut machine = cd_machine(8);
        machine.set_mode(mode);
        enable_irq15_wake(&mut machine);
        out(&mut machine, ide::SECONDARY_CMD_BASE + 7, 0xa1);
        let ticks = machine.ide.ticks_until_completion().unwrap();
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
