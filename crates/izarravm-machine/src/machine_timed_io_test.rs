// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const COM1_THR: u16 = 0x03f8;
const COM1_IER: u16 = 0x03f9;
const COM1_FCR: u16 = 0x03fa;
const COM1_LCR: u16 = 0x03fb;
const COM1_MCR: u16 = 0x03fc;
const LPT1_DATA: u16 = 0x0378;
const LPT1_CONTROL: u16 = 0x037a;

const ALL_MODES: [GswMode; 4] = [
    GswMode::Gsw386Slow,
    GswMode::Gsw386,
    GswMode::Gsw486,
    GswMode::Gsw586,
];

fn out(machine: &mut Machine, port: u16, value: u8) {
    {
        let mut bus = machine.make_construction_bus();
        bus.write_io(port, BusWidth::Byte, u32::from(value), false)
            .unwrap();
    }
}

fn initialize_pic(machine: &mut Machine, master_mask: u8, slave_mask: u8) {
    for (port, value) in [
        (0x20, 0x11),
        (0x21, 0x08),
        (0x21, 0x04),
        (0x21, 0x01),
        (0xa0, 0x11),
        (0xa1, 0x70),
        (0xa1, 0x02),
        (0xa1, 0x01),
        (0x21, master_mask),
        (0xa1, slave_mask),
    ] {
        out(machine, port, value);
    }
}

fn initialize_level_pic(machine: &mut Machine, master_mask: u8, slave_mask: u8) {
    for (port, value) in [
        (0x20, 0x19),
        (0x21, 0x08),
        (0x21, 0x04),
        (0x21, 0x01),
        (0xa0, 0x19),
        (0xa1, 0x70),
        (0xa1, 0x02),
        (0xa1, 0x01),
        (0x21, master_mask),
        (0xa1, slave_mask),
    ] {
        out(machine, port, value);
    }
}

fn enable_halt_wake(machine: &mut Machine, master_mask: u8, slave_mask: u8) {
    machine.cpu.registers.eflags |= 0x0200;
    initialize_pic(machine, master_mask, slave_mask);
}

fn program_rtc_periodic(machine: &mut Machine) {
    out(machine, 0x70, 0x0b);
    out(machine, 0x71, 0x40); // PIE, with binary and 24-hour bits forced by RTC
}

fn program_uart_fifo_timeout(machine: &mut Machine) {
    out(machine, COM1_LCR, 0x80);
    out(machine, COM1_THR, 1); // divisor 1
    out(machine, COM1_IER, 0);
    out(machine, COM1_LCR, 0x03); // 8N1
    out(machine, COM1_FCR, 0x41); // FIFO on, four-byte RX trigger
    out(machine, COM1_IER, 0x01); // received-data interrupts
    out(machine, COM1_MCR, 0x18); // loopback and OUT2 interrupt gate
}

#[test]
fn rtc_default_periodic_edge_is_exact_in_every_cpu_mode() {
    for mode in ALL_MODES {
        let mut machine = int15_machine(4);
        machine.set_mode(mode);
        program_rtc_periodic(&mut machine);
        let deadline = machine.rtc.ticks_until_periodic_irq().unwrap();

        machine.advance_devices_ticks(deadline - 1);
        assert!(!machine.pic.irr_bit(8), "early IRQ8 in {mode:?}");
        let expected_cap = machine.timeline.cpu_clocks_for_master_ticks_ceil(1).max(1);
        assert_eq!(machine.event_batch_cap(u64::MAX), expected_cap, "{mode:?}");
        machine.advance_devices_ticks(1);
        assert!(machine.pic.irr_bit(8), "missing IRQ8 in {mode:?}");

        out(&mut machine, 0x70, 0x0c);
        let status = with_bus(&mut machine, |bus| {
            bus.read_io(0x71, BusWidth::Byte, 0, false).unwrap() as u8
        });
        assert_eq!(status & 0xc0, 0xc0, "PF and IRQF in {mode:?}");
    }
}

#[test]
fn rtc_phase_and_halt_wake_survive_a_live_mode_switch() {
    let mut machine = int15_machine(4);
    machine.set_mode(GswMode::Gsw586);
    enable_halt_wake(&mut machine, 0xfb, 0xfe); // cascade plus slave IRQ8
    program_rtc_periodic(&mut machine);
    let deadline = machine.rtc.ticks_until_periodic_irq().unwrap();
    machine.advance_devices_ticks(deadline / 3);
    let remaining = machine.rtc.ticks_until_periodic_irq().unwrap();

    machine.set_mode(GswMode::Gsw386Slow);
    assert_eq!(machine.rtc.ticks_until_periodic_irq(), Some(remaining));
    let expected = machine
        .timeline
        .cpu_clocks_for_master_ticks_ceil(remaining)
        .max(1);
    assert_eq!(
        machine.next_timer_wake(machine.master_ticks() + remaining),
        Some(expected)
    );
    machine.advance_devices_ticks(remaining - 1);
    assert!(!machine.pic.irr_bit(8));
    machine.advance_devices_ticks(1);
    assert!(machine.pic.irr_bit(8));
}

#[test]
fn uart_fifo_timeout_is_an_exact_halt_wake_in_every_mode() {
    for mode in ALL_MODES {
        let mut machine = int15_machine(4);
        machine.set_mode(mode);
        enable_halt_wake(&mut machine, 0xef, 0xff); // master IRQ4 only
        program_uart_fifo_timeout(&mut machine);
        out(&mut machine, COM1_THR, b'U');
        let deadline = machine.serial.ticks_until_irq().unwrap();
        let expected = machine
            .timeline
            .cpu_clocks_for_master_ticks_ceil(deadline)
            .max(1);
        assert_eq!(
            machine.next_timer_wake(machine.master_ticks() + deadline),
            Some(expected),
            "{mode:?}"
        );

        machine.advance_devices_ticks(deadline - 1);
        assert!(!machine.pic.irr_bit(4), "early IRQ4 in {mode:?}");
        machine.advance_devices_ticks(1);
        assert!(machine.pic.irr_bit(4), "missing IRQ4 in {mode:?}");
        let iir = with_bus(&mut machine, |bus| {
            bus.read_io(COM1_FCR, BusWidth::Byte, 0, false).unwrap() as u8
        });
        assert_eq!(iir & 0x0f, 0x0c, "character timeout in {mode:?}");
    }
}

#[test]
fn uart_transmit_deadline_uses_master_time_across_mode_switches() {
    let mut machine = int15_machine(4);
    machine.set_mode(GswMode::Gsw586);
    out(&mut machine, COM1_LCR, 0x80);
    out(&mut machine, COM1_THR, 12); // 9600 baud
    out(&mut machine, COM1_IER, 0);
    out(&mut machine, COM1_LCR, 0x03);
    out(&mut machine, COM1_THR, b'A');
    let deadline = machine.serial.ticks_until_idle();
    machine.advance_devices_ticks(deadline / 2);
    let remaining = machine.serial.ticks_until_idle();
    machine.set_mode(GswMode::Gsw386Slow);
    assert_eq!(machine.serial.ticks_until_idle(), remaining);
    assert!(machine.serial_output().is_empty());

    machine.advance_devices_ticks(remaining - 1);
    assert!(machine.serial_output().is_empty());
    assert_eq!(
        machine.event_batch_cap(u64::MAX),
        machine.timeline.cpu_clocks_for_master_ticks_ceil(1).max(1)
    );
    machine.advance_devices_ticks(1);
    assert_eq!(machine.serial_output(), b"A");
}

#[test]
fn lpt_busy_ack_and_irq_deadlines_survive_a_mode_switch() {
    let mut machine = int15_machine(4);
    machine.set_mode(GswMode::Gsw586);
    enable_halt_wake(&mut machine, 0x7f, 0xff); // master IRQ7 only
    out(&mut machine, LPT1_DATA, b'P');
    out(&mut machine, LPT1_CONTROL, 0x11);
    out(&mut machine, LPT1_CONTROL, 0x10);
    let irq_deadline = machine.lpt.ticks_until_irq().unwrap();
    machine.advance_devices_ticks(irq_deadline / 2);
    let remaining = machine.lpt.ticks_until_irq().unwrap();
    machine.set_mode(GswMode::Gsw386);
    assert_eq!(machine.lpt.ticks_until_irq(), Some(remaining));
    let expected = machine
        .timeline
        .cpu_clocks_for_master_ticks_ceil(remaining)
        .max(1);
    assert_eq!(
        machine.next_timer_wake(machine.master_ticks() + remaining),
        Some(expected)
    );

    machine.advance_devices_ticks(remaining - 1);
    assert!(machine.lpt_output().is_empty());
    assert!(!machine.pic.irr_bit(7));
    machine.advance_devices_ticks(1);
    assert_eq!(machine.lpt_output(), b"P");
    assert!(machine.pic.irr_bit(7));

    let ack = machine.lpt.ticks_until_event().unwrap();
    machine.advance_devices_ticks(ack - 1);
    assert_eq!(
        machine.event_batch_cap(u64::MAX),
        machine.timeline.cpu_clocks_for_master_ticks_ceil(1).max(1)
    );
    machine.advance_devices_ticks(1);
    assert_eq!(machine.lpt.ticks_until_event(), None);
}

#[test]
fn halted_cpu_finishes_non_interrupting_uart_and_lpt_transfers() {
    // COM1 'S'; LPT1 'P' + strobe pulse; CLI; HLT.
    let code = [
        0xba, 0xf8, 0x03, 0xb0, b'S', 0xee, 0xba, 0x78, 0x03, 0xb0, b'P', 0xee, 0xba, 0x7a, 0x03,
        0xb0, 0x01, 0xee, 0x30, 0xc0, 0xee, 0xfa, 0xf4,
    ];
    let mut machine = Machine::new(
        MachineProfile::gsw_386(4, VideoCard::Vega),
        rom_with_code(&code),
    )
    .unwrap();

    let stop = machine.run_until_halt_or_cycles(100_000).unwrap();
    assert_eq!(stop, StopReason::Halted);
    assert_eq!(machine.serial_output(), b"S");
    assert_eq!(machine.lpt_output(), b"P");
    assert_eq!(machine.serial.ticks_until_event(), None);
    assert_eq!(machine.lpt.ticks_until_event(), None);
}

#[test]
fn keyboard_deadline_is_exact_and_wakes_hlt_in_every_mode() {
    for mode in ALL_MODES {
        let mut machine = int15_machine(4);
        machine.set_mode(mode);
        enable_halt_wake(&mut machine, 0xfd, 0xff); // master IRQ1 only
        machine.inject_key_scancodes(&[0x1e]);
        let deadline = machine.keyboard.ticks_until_event().unwrap();
        let expected = machine
            .timeline
            .cpu_clocks_for_master_ticks_ceil(deadline)
            .max(1);
        assert_eq!(
            machine.next_timer_wake(machine.master_ticks() + deadline),
            Some(expected),
            "{mode:?}"
        );

        machine.advance_devices_ticks(deadline - 1);
        assert!(!machine.pic.irr_bit(1), "early IRQ1 in {mode:?}");
        assert_eq!(machine.read_io_port_u8(0x64) & 0x01, 0, "{mode:?}");
        assert_eq!(
            machine.event_batch_cap(u64::MAX),
            machine.timeline.cpu_clocks_for_master_ticks_ceil(1).max(1),
            "{mode:?}"
        );
        machine.advance_devices_ticks(1);
        assert!(machine.pic.irr_bit(1), "missing IRQ1 in {mode:?}");
        assert_eq!(machine.read_io_port_u8(0x60), 0x1e, "{mode:?}");
    }
}

#[test]
fn keyboard_deadline_keeps_master_time_across_a_live_mode_switch() {
    let mut machine = int15_machine(4);
    machine.set_mode(GswMode::Gsw586);
    enable_halt_wake(&mut machine, 0xfd, 0xff);
    machine.inject_key_scancodes(&[0x30]);
    let deadline = machine.keyboard.ticks_until_event().unwrap();
    machine.advance_devices_ticks(deadline / 3);
    let remaining = machine.keyboard.ticks_until_event().unwrap();

    machine.set_mode(GswMode::Gsw386Slow);
    assert_eq!(machine.keyboard.ticks_until_event(), Some(remaining));
    let expected = machine
        .timeline
        .cpu_clocks_for_master_ticks_ceil(remaining)
        .max(1);
    assert_eq!(
        machine.next_timer_wake(machine.master_ticks() + remaining),
        Some(expected)
    );
    machine.advance_devices_ticks(remaining - 1);
    assert!(!machine.pic.irr_bit(1));
    machine.advance_devices_ticks(1);
    assert!(machine.pic.irr_bit(1));
    assert_eq!(machine.read_io_port_u8(0x60), 0x30);
}

#[test]
fn keyboard_precedes_aux_and_each_byte_keeps_its_wire_deadline() {
    for mode in ALL_MODES {
        let mut machine = int15_machine(4);
        machine.set_mode(mode);
        initialize_pic(&mut machine, 0xf9, 0xef); // IRQ1, cascade, and IRQ12
        machine.keyboard.set_mouse_irq(true);
        machine.keyboard.set_mouse_reporting(true);
        machine.inject_mouse(4, -2, 0x01);
        machine.inject_key_scancodes(&[0x2e]);

        let keyboard_deadline = machine.keyboard.ticks_until_event().unwrap();
        machine.advance_devices_ticks(keyboard_deadline);
        assert!(machine.pic.irr_bit(1), "keyboard did not win in {mode:?}");
        assert!(!machine.pic.irr_bit(12), "early AUX byte in {mode:?}");
        assert_eq!(machine.read_io_port_u8(0x64) & 0x20, 0, "{mode:?}");
        assert_eq!(machine.read_io_port_u8(0x60), 0x2e, "{mode:?}");
        assert_eq!(machine.pic.acknowledge(), Some(0x09), "{mode:?}");
        out(&mut machine, 0x20, 0x20);

        let aux_deadline = machine.keyboard.ticks_until_event().unwrap();
        machine.advance_devices_ticks(aux_deadline - 1);
        assert!(!machine.pic.irr_bit(12), "early IRQ12 in {mode:?}");
        machine.advance_devices_ticks(1);
        assert!(machine.pic.irr_bit(12), "missing IRQ12 in {mode:?}");
        assert_eq!(machine.read_io_port_u8(0x64) & 0x20, 0x20, "{mode:?}");
    }
}

#[test]
fn ltim_keyboard_request_stays_asserted_until_the_guest_reads_obf() {
    let mut machine = int15_machine(4);
    initialize_level_pic(&mut machine, 0xfd, 0xff);
    machine.inject_key_scancodes(&[0x20]);
    let deadline = machine.keyboard.ticks_until_event().unwrap();
    machine.advance_devices_ticks(deadline);

    assert_eq!(machine.pic.acknowledge(), Some(0x09));
    out(&mut machine, 0x20, 0x20);
    assert!(
        machine.pic.irr_bit(1),
        "EOI reasserts while the 8042 output line remains high"
    );
    assert_eq!(machine.read_io_port_u8(0x60), 0x20);
    assert!(
        !machine.pic.irr_bit(1),
        "the port read deasserts the LTIM input in the same I/O cycle"
    );
}

// ---------------------------------------------------------------------------
// Slice 9A (`dev_docs/2026-09-05-device-timing-slice9-design.md`): the 8259A
// INTA instrument's PIT-edge-to-IRQ0-handler-entry histogram. Certifies that
// under TODAY's model (no INTA-turnaround charge -- that belongs to slice 8)
// a PIT edge immediately followed by the IRQ0 acknowledge records exactly one
// sample at zero extra delay, so slice 8 has a certifier to move the sample
// off the zero bucket against.
// ---------------------------------------------------------------------------

#[test]
fn irq0_entry_diagnostics_record_a_zero_delay_sample_under_todays_model() {
    let mut machine = int15_machine(4);
    initialize_pic(&mut machine, 0xfe, 0xff); // unmask IRQ0 only, on the master
    // PIT channel 0, mode 0 (interrupt on terminal count), lobyte/hibyte, binary,
    // count 16 -> one OUT rising edge 16 PIT input clocks later.
    out(&mut machine, 0x43, 0x30);
    out(&mut machine, 0x40, 16);
    out(&mut machine, 0x40, 0);

    assert_eq!(
        machine.irq0_entry_samples(),
        0,
        "nothing recorded before the edge"
    );
    assert_eq!(machine.inta_acknowledge_count(), 0);

    // Advance one master tick at a time until the edge lands, so the
    // acknowledge below happens at EXACTLY the edge's own master tick: the
    // zero-extra-delay case.
    let mut fired = false;
    for _ in 0..500_000 {
        if machine.pic.irr_bit(0) {
            fired = true;
            break;
        }
        machine.advance_devices_ticks(1);
    }
    assert!(fired, "IRQ0 never asserted");

    let vector = with_bus(&mut machine, |bus| bus.acknowledge_interrupt());
    assert_eq!(vector, Some(0x08), "IRQ0 vector at ICW2 base 0x08");

    assert_eq!(machine.inta_acknowledge_count(), 1);
    assert_eq!(machine.irq0_entry_samples(), 1, "exactly one IRQ0 delivery");
    let histogram = machine.irq0_entry_histogram();
    assert_eq!(
        histogram[0], 1,
        "today's model charges no INTA turnaround: the sample lands in the zero bucket"
    );
    assert_eq!(
        histogram.iter().skip(1).sum::<u64>(),
        0,
        "no sample should land in any non-zero bucket under today's model"
    );
}

#[test]
fn irq0_entry_diagnostics_bucket_a_deliberately_delayed_acknowledge() {
    let mut machine = int15_machine(4);
    initialize_pic(&mut machine, 0xfe, 0xff);
    out(&mut machine, 0x43, 0x30);
    out(&mut machine, 0x40, 16);
    out(&mut machine, 0x40, 0);

    let mut fired = false;
    for _ in 0..500_000 {
        if machine.pic.irr_bit(0) {
            fired = true;
            break;
        }
        machine.advance_devices_ticks(1);
    }
    assert!(fired, "IRQ0 never asserted");

    // Let a handful of guest clocks pass BEFORE the acknowledge, so the sample
    // must NOT land in the zero bucket -- proving the histogram actually
    // measures elapsed time rather than always recording zero.
    let delay_ticks = machine.timeline.master_ticks_for_cpu_clocks(40);
    machine.advance_devices_ticks(delay_ticks);
    let vector = with_bus(&mut machine, |bus| bus.acknowledge_interrupt());
    assert_eq!(vector, Some(0x08));

    assert_eq!(machine.irq0_entry_samples(), 1);
    let histogram = machine.irq0_entry_histogram();
    assert_eq!(
        histogram[0], 0,
        "a real delay must not land in the zero bucket"
    );
    assert_eq!(histogram.iter().skip(1).sum::<u64>(), 1);
}

#[test]
fn irq0_entry_diagnostics_ignore_a_non_irq0_acknowledge() {
    let mut machine = int15_machine(4);
    initialize_pic(&mut machine, 0xfd, 0xff); // unmask IRQ1 only
    machine.inject_key_scancodes(&[0x1e]);
    let deadline = machine.keyboard.ticks_until_event().unwrap();
    machine.advance_devices_ticks(deadline);
    assert!(machine.pic.irr_bit(1), "IRQ1 never asserted");

    let vector = with_bus(&mut machine, |bus| bus.acknowledge_interrupt());
    assert_eq!(vector, Some(0x09), "IRQ1 vector");

    assert_eq!(
        machine.inta_acknowledge_count(),
        1,
        "the counter is not IRQ0-specific"
    );
    assert_eq!(
        machine.irq0_entry_samples(),
        0,
        "a non-IRQ0 acknowledge must not touch the IRQ0-specific histogram"
    );
}
