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

fn initialize_mpu_pic(machine: &mut Machine) {
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
        (0xa1, 0xfd),
    ] {
        write_byte(machine, port, value);
    }
}

fn acknowledge_irq9(machine: &mut Machine) {
    assert_eq!(machine.pic.acknowledge(), Some(0x71));
    write_byte(machine, 0xa0, 0x20);
    write_byte(machine, 0x20, 0x20);
}

fn arm_first_midi_track(machine: &mut Machine) {
    let now = machine.master_ticks();
    machine.midi_mpu.write_command_at(0xec, now);
    assert_eq!(machine.midi_mpu.read_data_at(now), 0xfe);
    machine.midi_mpu.write_data(0x01, now);
    for command in [0xb8, 0x08] {
        machine.midi_mpu.write_command_at(command, now);
        assert_eq!(machine.midi_mpu.read_data_at(now), 0xfe);
    }
}

#[test]
fn both_mpu_port_pairs_reset_and_acknowledge() {
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.sound_blaster.enabled = false;
    let mut machine = Machine::new(profile, I386DX25_TEST_ROM).unwrap();

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

#[test]
fn intelligent_track_request_irq9_is_exact_and_batch_invariant() {
    let mut whole = test_machine();
    let mut split = test_machine();
    for machine in [&mut whole, &mut split] {
        initialize_mpu_pic(machine);
        arm_first_midi_track(machine);
    }

    let deadline = whole.midi_mpu.ticks_until_event().unwrap();
    assert_eq!(split.midi_mpu.ticks_until_event(), Some(deadline));
    let deadline_clocks = whole
        .timeline
        .cpu_clocks_for_master_ticks_ceil(deadline)
        .max(1);
    assert!(whole.event_batch_cap(u64::MAX) <= deadline_clocks);

    whole.advance_devices_ticks(deadline);
    split.advance_devices_ticks(deadline / 3);
    assert!(!split.pic.irr_bit(9));
    split.advance_devices_ticks(deadline - deadline / 3 - 1);
    assert!(!split.pic.irr_bit(9));
    split.advance_devices_ticks(1);

    for machine in [&mut whole, &mut split] {
        assert!(
            machine.midi_mpu.irq_level(),
            "MPU request line should be active"
        );
        assert!(machine.pic.irr_bit(9));
        assert_eq!(read_byte(machine, MIDI_MPU_BASE), 0xf0);
        acknowledge_irq9(machine);
    }
    assert_eq!(whole.master_ticks(), split.master_ticks());
}

#[test]
fn guest_services_intelligent_track_requests_through_irq9() {
    let mut code = vec![
        0xfa, 0x31, 0xc0, 0x8e, 0xd8, 0x8e, 0xc0, // cli; zero DS and ES
        0xc7, 0x06, 0xc4, 0x01, 0x00, 0x00, // IVT 71h offset, patched below
        0xc7, 0x06, 0xc6, 0x01, 0x00, 0x00, // IVT 71h segment
        0xc7, 0x06, 0x00, 0x05, 0x00, 0x00, // request count and first code
        0xba, 0x31, 0x03, // DX = MPU command port
        0xb0, 0xec, 0xee, 0x4a, 0xec, // active-track command and ACK
        0xb0, 0x01, 0xee, // track 0 enabled
        0x42, 0xb0, 0xb8, 0xee, 0x4a, 0xec, // clear counters and ACK
        0x42, 0xb0, 0x08, 0xee, 0x4a, 0xec, // start playback and ACK
        0xb0, 0x11, 0xe6, 0x20, 0xe6, 0xa0, // start both PIC initializations
        0xb0, 0x08, 0xe6, 0x21, // master vector base
        0xb0, 0x70, 0xe6, 0xa1, // slave vector base
        0xb0, 0x04, 0xe6, 0x21, // slave on master IRQ2
        0xb0, 0x02, 0xe6, 0xa1, // slave identity 2
        0xb0, 0x01, 0xe6, 0x21, 0xe6, 0xa1, // 8086 mode
        0xb0, 0xfb, 0xe6, 0x21, // unmask cascade
        0xb0, 0xfd, 0xe6, 0xa1, // unmask IRQ9
        0xfb, 0xf4, 0xf4, 0xf4, 0xfa, 0xf4, // service three requests, then halt
    ];
    let handler = (0x7c00 + code.len()) as u16;
    code[11..13].copy_from_slice(&handler.to_le_bytes());

    let mut irq = vec![
        0x50, 0x53, 0x52, // push AX, BX, DX
        0xba, 0x30, 0x03, 0xec, // read the MPU request byte
        0x8a, 0x1e, 0x00, 0x05, 0xb7, 0x00, // BX = request count
        0x88, 0x87, 0x01, 0x05, // save request at 0501h + BX
        0xfe, 0x06, 0x00, 0x05, // increment request count
        0x80, 0xfb, 0x00, 0x74, 0x00, // first request, patched below
        0x80, 0xfb, 0x01, 0x74, 0x00, // second request, patched below
        0xeb, 0x00, // otherwise EOI
    ];
    let first = irq.len();
    irq.extend_from_slice(&[
        0x30, 0xc0, 0xee, // zero timing byte
        0xb0, 0x90, 0xee, 0xb0, 0x3c, 0xee, 0xb0, 0x64, 0xee, // middle C on
        0xeb, 0x00, // EOI, patched below
    ]);
    let second = irq.len();
    irq.extend_from_slice(&[
        0x30, 0xc0, 0xee, 0xb0, 0xfc, 0xee, // zero timing, then end track
    ]);
    let eoi = irq.len();
    irq.extend_from_slice(&[
        0xb0, 0x20, 0xe6, 0xa0, 0xe6, 0x20, // slave and master EOI
        0x5a, 0x5b, 0x58, 0xcf, // restore registers and IRET
    ]);
    irq[25] = (first as isize - 26) as i8 as u8;
    irq[30] = (second as isize - 31) as i8 as u8;
    irq[32] = (eoi as isize - 33) as i8 as u8;
    irq[first + 13] = (eoi as isize - (first + 14) as isize) as i8 as u8;
    code.extend_from_slice(&irq);

    let mut machine = Machine::new_boot_image(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        boot_image_with(&code),
    )
    .unwrap();
    let reason = machine.run_until_halt_or_cycles(2_000_000).unwrap();

    assert_eq!(reason, StopReason::Halted);
    assert_eq!(
        &machine.memory().as_slice()[0x0500..0x0504],
        &[3, 0xf0, 0xf0, 0xfc]
    );
    let note = machine.take_midi_message().expect("guest note event");
    let end = machine.take_midi_message().expect("guest end event");
    assert_eq!(note.bytes, [0x90, 0x3c, 0x64]);
    assert_eq!(end.bytes, [0xfc]);
    assert!(end.guest_tick > note.guest_tick);
    assert!(machine.take_midi_message().is_none());
}
