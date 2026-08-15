// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn rc_deadlines_are_absolute_and_expire_at_the_boundary() {
    let mut port = GamePort::default();
    port.set_state(Some(JoystickState {
        x: 0,
        y: u8::MAX,
        buttons: 0,
    }));
    port.charge(10_000);
    let x_deadline = 10_000 + axis_ticks(0);
    let y_deadline = 10_000 + axis_ticks(u8::MAX);
    assert_eq!(port.read(x_deadline - 1) & 0x03, 0x03);
    assert_eq!(port.read(x_deadline) & 0x03, 0x02);
    assert_eq!(port.read(y_deadline - 1) & 0x03, 0x02);
    assert_eq!(port.read(y_deadline) & 0x03, 0x00);
}

#[test]
fn buttons_are_active_low_and_joystick_b_is_unpopulated() {
    let mut port = GamePort::default();
    port.set_state(Some(JoystickState {
        x: 128,
        y: 128,
        buttons: 0x01,
    }));
    assert_eq!(port.read(0) & 0xf0, 0xe0);
    port.set_state(Some(JoystickState {
        x: 128,
        y: 128,
        buttons: 0x02,
    }));
    assert_eq!(port.read(0) & 0xf0, 0xd0);
}

#[test]
fn disconnect_resets_stale_charge_state() {
    let mut port = GamePort::default();
    port.set_state(Some(JoystickState {
        x: u8::MAX,
        y: u8::MAX,
        buttons: 3,
    }));
    port.charge(1_000);
    port.set_state(None);
    assert_eq!(port.read(1_000), 0xff);
    assert_eq!(port.discharge_deadlines, [0; 2]);
}

/// An empty connector is an open circuit: every line floats high, so the axis
/// one-shots never appear to fire and the buttons never appear pressed.
#[test]
fn an_absent_stick_reads_open_on_every_line() {
    let port = GamePort::default();
    assert_eq!(port.read(0), 0xff);
    assert_eq!(port.read(u64::MAX), 0xff);
    // The BIOS switch service answers the same question and must not contradict
    // the port it reports on.
    assert_eq!(port.bios_switches(), 0xff);
    // An attached stick still reports its own state, so the arm above is not
    // simply "always 0xFF".
    let mut port = port;
    port.set_state(Some(JoystickState {
        x: 0,
        y: 0,
        buttons: 0x03,
    }));
    assert_eq!(port.read(0), 0xc0);
    assert_eq!(port.bios_switches(), 0xc0);
}

/// 86Box's compatibility behaviour: while an axis one-shot is discharging the
/// button lines read released, and they become visible again once both have
/// expired.
#[test]
fn buttons_read_released_while_an_axis_is_still_discharging() {
    let mut port = GamePort::default();
    port.set_state(Some(JoystickState {
        x: 128,
        y: 200,
        buttons: 0x03,
    }));
    port.charge(10_000);
    let x_deadline = 10_000 + axis_ticks(128);
    let y_deadline = 10_000 + axis_ticks(200);
    assert!(x_deadline < y_deadline, "the test needs staggered deadlines");

    // Both discharging: axis bits set, buttons masked out of the answer.
    assert_eq!(port.read(10_000), 0xf3);
    // Only the slower axis discharging: still masked, so the guard is keyed on
    // "any axis", not on "the first axis".
    assert_eq!(port.read(x_deadline), 0xf2);
    // Past both deadlines the pressed buttons appear.
    assert_eq!(port.read(y_deadline), 0xc0);

    // The masking arm is what hid them: with no buttons pressed the mid-pulse
    // read is identical, so the assertion above is not reading a constant.
    let mut released = port;
    released.set_state(Some(JoystickState {
        x: 128,
        y: 200,
        buttons: 0x00,
    }));
    released.charge(10_000);
    assert_eq!(released.read(10_000), 0xf3);
    assert_eq!(released.read(y_deadline), 0xf0);
}

/// The 555/558 timing law, asserted against the formula rather than against a
/// recorded constant: 24.2 us of base plus 1,100 us of pot span.
#[test]
fn axis_pulse_follows_the_555_formula_over_a_100k_pot() {
    let expected = |axis: u8| -> u64 {
        let ns = 24_200_u64 + 1_100_000_u64 * u64::from(axis) / 255;
        (u128::from(ns) * u128::from(MASTER_CLOCK_HZ)).div_ceil(1_000_000_000) as u64
    };
    for axis in [0_u8, 1, 64, 128, 200, 254, u8::MAX] {
        assert_eq!(axis_ticks(axis), expected(axis), "axis {axis}");
    }

    // The headline numbers of the 86Box comparison: a centred stick is ~0.57 ms
    // and full deflection is ~1.12 ms. Checked in ticks so a wrong RC_SPAN_NS
    // (the old 2,750 us implied 1.40 ms and 2.77 ms) cannot pass.
    let ns_per_tick_num = 1_000_000_000_u128;
    let to_ns = |ticks: u64| -> u64 {
        (u128::from(ticks) * ns_per_tick_num / u128::from(MASTER_CLOCK_HZ)) as u64
    };
    let centred = to_ns(axis_ticks(128));
    assert!(
        (570_000..=580_000).contains(&centred),
        "centred pulse {centred} ns is not ~0.57 ms"
    );
    let full = to_ns(axis_ticks(u8::MAX));
    assert!(
        (1_120_000..=1_130_000).contains(&full),
        "full-deflection pulse {full} ns is not ~1.12 ms"
    );
}
