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

    for base in [WAVETABLE_MPU_BASE, MIDI_INPUT_MPU_BASE] {
        assert_eq!(read_byte(&mut machine, base + 1), 0x80);
        write_byte(&mut machine, base + 1, 0xff);
        assert_eq!(read_byte(&mut machine, base + 1), 0x00);
        assert_eq!(read_byte(&mut machine, base), 0xfe);
        assert_eq!(read_byte(&mut machine, base + 1), 0x80);
    }
}

#[test]
fn wavetable_output_is_mapped_to_p300() {
    let mut machine = test_machine();
    for byte in [0x90, 60, 100] {
        write_byte(&mut machine, WAVETABLE_MPU_BASE, byte);
    }

    let message = machine
        .take_wavetable_midi_message()
        .expect("P300 produced a MIDI message");
    assert_eq!(message.bytes, [0x90, 60, 100]);

    for byte in [0x80, 60, 0] {
        write_byte(&mut machine, MIDI_INPUT_MPU_BASE, byte);
    }
    assert!(machine.take_wavetable_midi_message().is_none());
}

#[test]
fn host_midi_input_is_mapped_only_to_p330() {
    let mut machine = test_machine();
    assert_eq!(machine.inject_midi_input(&[0x90, 64, 127]), 3);
    assert!(machine.pic.irr_bit(MIDI_INPUT_IRQ));

    assert_eq!(read_byte(&mut machine, WAVETABLE_MPU_BASE + 1), 0x80);
    assert_eq!(read_byte(&mut machine, MIDI_INPUT_MPU_BASE + 1), 0x00);
    assert_eq!(read_byte(&mut machine, MIDI_INPUT_MPU_BASE), 0x90);
    assert_eq!(read_byte(&mut machine, MIDI_INPUT_MPU_BASE), 64);
    assert_eq!(read_byte(&mut machine, MIDI_INPUT_MPU_BASE), 127);
    assert_eq!(read_byte(&mut machine, MIDI_INPUT_MPU_BASE + 1), 0x80);
}
