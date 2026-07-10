// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn write_byte(machine: &mut Machine, port: u16, value: u8) {
    with_bus(machine, |bus| {
        bus.write_io(port, BusWidth::Byte, u32::from(value), false)
            .unwrap();
    });
}

fn read_byte(machine: &mut Machine, port: u16) -> u8 {
    with_bus(machine, |bus| {
        bus.read_io(port, BusWidth::Byte, 0, false).unwrap() as u8
    })
}

#[test]
fn both_mpu_port_pairs_reset_and_acknowledge() {
    let mut machine = test_machine();

    for base in [WAVETABLE_MPU_BASE, MIDI_MPU_BASE] {
        assert_eq!(read_byte(&mut machine, base + 1), 0x80);
        write_byte(&mut machine, base + 1, 0xff);
        assert_eq!(read_byte(&mut machine, base + 1), 0x00);
        assert_eq!(read_byte(&mut machine, base), 0xfe);
        assert_eq!(read_byte(&mut machine, base + 1), 0x80);
    }
}

#[test]
fn p300_wavetable_and_p330_midi_outputs_are_independent() {
    let mut machine = test_machine();
    for byte in [0x90, 60, 100] {
        write_byte(&mut machine, WAVETABLE_MPU_BASE, byte);
    }

    let message = machine
        .take_wavetable_midi_message()
        .expect("P300 produced a MIDI message");
    assert_eq!(message.bytes, [0x90, 60, 100]);

    for byte in [0x80, 60, 0] {
        write_byte(&mut machine, MIDI_MPU_BASE, byte);
    }
    assert!(machine.take_wavetable_midi_message().is_none());
    assert_eq!(
        machine
            .take_midi_message()
            .expect("P330 produced a MIDI message")
            .bytes,
        [0x80, 60, 0]
    );
    assert!(machine.take_midi_message().is_none());
}

#[test]
fn wavetable_timestamps_follow_the_exact_in_batch_master_tick() {
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw486);
    machine.advance_devices_clocks(7);
    let batch_start = machine.master_ticks();

    let expected = with_bus(&mut machine, |bus| {
        let mut expected = Vec::new();
        for (index, (clocks, byte)) in [(10, 0x90), (20, 60), (30, 100), (40, 61), (50, 110)]
            .into_iter()
            .enumerate()
        {
            CpuBus::write_io(bus, WAVETABLE_MPU_BASE, BusWidth::Byte, byte, clocks, false).unwrap();
            if index == 2 || index == 4 {
                expected.push(bus.guest_tick_now());
            }
        }
        expected
    });

    let first = machine.take_wavetable_midi_message().unwrap();
    let second = machine.take_wavetable_midi_message().unwrap();
    assert_eq!(first.guest_tick, expected[0]);
    assert_eq!(second.guest_tick, expected[1]);
    assert!(first.guest_tick >= batch_start + 30 * 100);
    assert!(second.guest_tick > first.guest_tick);
}

#[test]
fn wavetable_timestamps_remain_monotonic_across_cpu_mode_switches() {
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw586);
    for byte in [0x90, 60, 100] {
        write_byte(&mut machine, WAVETABLE_MPU_BASE, byte);
    }
    let first = machine.take_wavetable_midi_message().unwrap();

    machine.advance_devices_clocks(100);
    machine.set_mode(GswMode::Gsw386Slow);
    machine.advance_devices_clocks(1);
    for byte in [0x80, 60, 0] {
        write_byte(&mut machine, WAVETABLE_MPU_BASE, byte);
    }
    let second = machine.take_wavetable_midi_message().unwrap();

    assert!(second.guest_tick > first.guest_tick);
    assert!(second.guest_tick - first.guest_tick >= 100 * 33 + 900);
}
