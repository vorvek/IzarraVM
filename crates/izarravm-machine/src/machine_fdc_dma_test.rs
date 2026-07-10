// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_core::MASTER_CLOCK_HZ;

fn clear_reset_interrupt(bus: &mut MachineBus<'_>) {
    bus.write_io(0x3F5, BusWidth::Byte, 0x08, false).unwrap();
    while bus.read_io(0x3F4, BusWidth::Byte, 0, false).unwrap() & 0x40 != 0 {
        let _ = bus.read_io(0x3F5, BusWidth::Byte, 0, false).unwrap();
    }
}

fn issue_seek(machine: &mut Machine, cylinder: u8) {
    with_bus(machine, |bus| {
        bus.write_io(0x3F2, BusWidth::Byte, 0x0C, false).unwrap();
        clear_reset_interrupt(bus);
        for byte in [0x03u8, 0xF0, 0x00, 0x0F, 0x00, cylinder] {
            bus.write_io(0x3F5, BusWidth::Byte, u32::from(byte), false)
                .unwrap();
        }
    });
}

fn prepare_one_sector_read() -> Machine {
    let mut machine = test_machine();
    let mut image = vec![0u8; 737_280];
    for (index, byte) in image[..512].iter_mut().enumerate() {
        *byte = (index as u8).wrapping_mul(17);
    }
    machine.mount_floppy(image).unwrap();
    with_bus(&mut machine, |bus| {
        bus.write_io(0x0B, BusWidth::Byte, 0x46, false).unwrap();
        bus.write_io(0x0C, BusWidth::Byte, 0, false).unwrap();
        for byte in [0x00u8, 0x40] {
            bus.write_io(0x04, BusWidth::Byte, u32::from(byte), false)
                .unwrap();
        }
        bus.write_io(0x81, BusWidth::Byte, 0, false).unwrap();
        for byte in [0xFFu8, 0x01] {
            bus.write_io(0x05, BusWidth::Byte, u32::from(byte), false)
                .unwrap();
        }
        bus.write_io(0x0A, BusWidth::Byte, 0x02, false).unwrap();

        bus.write_io(0x3F2, BusWidth::Byte, 0x1C, false).unwrap();
        clear_reset_interrupt(bus);
        for byte in [0xE6u8, 0, 0, 0, 1, 2, 1, 0x1B, 0xFF] {
            bus.write_io(0x3F5, BusWidth::Byte, u32::from(byte), false)
                .unwrap();
        }
    });
    machine
}

fn advance_fdc_by_deadline(machine: &mut Machine) {
    while machine.fdc.read_port(0x3F4).unwrap() & 0x40 == 0 {
        let ticks = machine
            .fdc
            .ticks_until_event(machine.master_ticks())
            .expect("active FDC command deadline");
        machine.advance_devices_ticks(ticks);
    }
}

fn drain_fdc_result(machine: &mut Machine) -> Vec<u8> {
    let mut result = Vec::new();
    while machine.fdc.read_port(0x3F4).unwrap() & 0x40 != 0 {
        result.push(machine.fdc.read_port(0x3F5).unwrap());
    }
    result
}

#[test]
fn seek_irq6_is_absent_one_tick_before_the_deadline_and_present_on_it() {
    let mut machine = test_machine();
    issue_seek(&mut machine, 7);
    let ticks = machine
        .fdc
        .ticks_until_event(machine.master_ticks())
        .unwrap();

    machine.advance_devices_ticks(ticks - 1);
    assert!(!machine.pic.interrupt_pending());
    machine.advance_devices_ticks(1);
    assert!(machine.pic.interrupt_pending());
}

#[test]
fn halted_wake_estimator_uses_the_next_fdc_deadline() {
    let mut machine = test_machine();
    machine.cpu.registers.eflags |= 0x0200;
    with_bus(&mut machine, |bus| {
        bus.write_io(0x21, BusWidth::Byte, 0xBF, false).unwrap(); // IRQ6 only
    });
    issue_seek(&mut machine, 3);
    let ticks = machine
        .fdc
        .ticks_until_event(machine.master_ticks())
        .unwrap();
    let expected = machine
        .timeline
        .cpu_clocks_for_master_ticks_ceil(ticks)
        .max(1);
    assert_eq!(
        machine.next_timer_wake(machine.master_ticks() + ticks),
        Some(expected)
    );
}

#[test]
fn live_cpu_mode_switch_does_not_move_a_seek_deadline() {
    let mut machine = test_machine();
    issue_seek(&mut machine, 9);
    let total = machine
        .fdc
        .ticks_until_event(machine.master_ticks())
        .unwrap();
    let first = total / 2;
    machine.advance_devices_ticks(first);
    let remaining = total - first;
    assert_eq!(
        machine
            .fdc
            .ticks_until_event(machine.master_ticks())
            .unwrap(),
        remaining
    );

    machine.set_mode(GswMode::Gsw586);
    assert_eq!(
        machine
            .fdc
            .ticks_until_event(machine.master_ticks())
            .unwrap(),
        remaining
    );
    machine.advance_devices_ticks(remaining - 1);
    assert!(!machine.pic.interrupt_pending());
    machine.advance_devices_ticks(1);
    assert!(machine.pic.interrupt_pending());
}

#[test]
fn fdc_and_dma_results_are_invariant_to_advance_batch_size() {
    let mut whole = prepare_one_sector_read();
    let mut split = prepare_one_sector_read();

    whole.advance_devices_ticks(2 * MASTER_CLOCK_HZ);
    advance_fdc_by_deadline(&mut split);

    let whole_bytes: Vec<u8> = (0..512)
        .map(|offset| whole.read_physical_u8(0x4000 + offset))
        .collect();
    let split_bytes: Vec<u8> = (0..512)
        .map(|offset| split.read_physical_u8(0x4000 + offset))
        .collect();
    assert_eq!(whole_bytes, split_bytes);
    assert_eq!(whole.dma.master.channels[2], split.dma.master.channels[2]);
    assert_eq!(whole.dma.master.channels[2].transfer_cycles, 512);
    assert_eq!(drain_fdc_result(&mut whole), drain_fdc_result(&mut split));
    assert!(whole.pic.interrupt_pending());
    assert!(split.pic.interrupt_pending());
}

#[test]
fn an_fdc_deadline_inside_the_fallback_caps_the_next_batch() {
    let mut machine = test_machine();
    issue_seek(&mut machine, 1);
    let ticks = machine
        .fdc
        .ticks_until_event(machine.master_ticks())
        .unwrap();
    machine.advance_devices_ticks(ticks - 100);
    let expected = machine
        .timeline
        .cpu_clocks_for_master_ticks_ceil(100)
        .max(1);
    assert_eq!(machine.event_batch_cap(u64::MAX), expected);
}

#[test]
fn write_data_pulls_each_byte_from_channel_two_at_its_sector_deadline() {
    let mut machine = test_machine();
    machine.mount_floppy(vec![0u8; 737_280]).unwrap();
    for offset in 0..512u32 {
        machine.write_physical_u8(0x5000 + offset, (offset as u8).wrapping_mul(29));
    }
    with_bus(&mut machine, |bus| {
        bus.write_io(0x0B, BusWidth::Byte, 0x4A, false).unwrap(); // channel 2, single, memory->device
        bus.write_io(0x0C, BusWidth::Byte, 0, false).unwrap();
        for byte in [0x00u8, 0x50] {
            bus.write_io(0x04, BusWidth::Byte, u32::from(byte), false)
                .unwrap();
        }
        bus.write_io(0x81, BusWidth::Byte, 0, false).unwrap();
        for byte in [0xFFu8, 0x01] {
            bus.write_io(0x05, BusWidth::Byte, u32::from(byte), false)
                .unwrap();
        }
        bus.write_io(0x0A, BusWidth::Byte, 0x02, false).unwrap();
        bus.write_io(0x3F2, BusWidth::Byte, 0x1C, false).unwrap();
        clear_reset_interrupt(bus);
        for byte in [0xC5u8, 0, 0, 0, 1, 2, 1, 0x1B, 0xFF] {
            bus.write_io(0x3F5, BusWidth::Byte, u32::from(byte), false)
                .unwrap();
        }
    });

    advance_fdc_by_deadline(&mut machine);
    let sector = machine
        .floppy
        .as_ref()
        .unwrap()
        .read_sector(0, 0, 1)
        .unwrap();
    for (offset, &byte) in sector.iter().enumerate() {
        assert_eq!(byte, (offset as u8).wrapping_mul(29));
    }
    assert!(machine.floppy_dirty());
    assert_eq!(machine.dma.master.channels[2].transfer_cycles, 512);
    assert_eq!(drain_fdc_result(&mut machine)[0] & 0xC0, 0);
}
