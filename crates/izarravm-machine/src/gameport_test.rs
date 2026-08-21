// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn rc_deadlines_are_absolute_and_expire_at_the_boundary() {
    let mut port = GamePort::default();
    port.set_state(Some(JoystickState::joystick_a(0, u8::MAX, 0)), 0);
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
    port.set_state(Some(JoystickState::joystick_a(128, 128, 0x01)), 0);
    assert_eq!(port.read(0) & 0xf0, 0xe0);
    port.set_state(Some(JoystickState::joystick_a(128, 128, 0x02)), 0);
    assert_eq!(port.read(0) & 0xf0, 0xd0);
}

#[test]
fn disconnect_resets_stale_charge_state() {
    let mut port = GamePort::default();
    port.set_state(Some(JoystickState::joystick_a(u8::MAX, u8::MAX, 3)), 0);
    port.charge(1_000);
    port.set_state(None, 1_000);
    assert_eq!(port.read(1_000), 0xff);
    assert_eq!(port.discharge_deadlines, [0; 4]);
}

/// An empty connector is an open circuit: every line floats high, so the axis
/// one-shots never appear to fire and the buttons never appear pressed.
#[test]
fn an_absent_stick_reads_open_on_every_line() {
    let mut port = GamePort::default();
    assert_eq!(port.read(0), 0xff);
    assert_eq!(port.read(u64::MAX), 0xff);
    // The BIOS switch service answers the same question and must not contradict
    // the port it reports on, and the axis service reports the timeout its
    // one-shot never ends rather than a centred stick at zero.
    assert_eq!(port.bios_switches(0), 0xff);
    assert_eq!(
        port.bios_axes(),
        [BIOS_AXIS_TIMEOUT, BIOS_AXIS_TIMEOUT, 0, 0],
        "an absent axis must read as a timed-out count, not as centre"
    );
    // An attached stick still reports its own state, so the arm above is not
    // simply "always 0xFF".
    port.set_state(Some(JoystickState::joystick_a(0, 0, 0x03)), 0);
    assert_eq!(port.read(0), 0xcc);
    assert_eq!(port.bios_switches(0), 0xc0);
    assert_eq!(
        port.bios_axes(),
        [0, 0, 0, 0],
        "an attached stick at the origin reads zero -- the timeout above is not a constant"
    );
}

/// 86Box's compatibility behaviour: while an axis one-shot is discharging the
/// button lines read released, and they become visible again once both have
/// expired.
#[test]
fn buttons_read_released_while_an_axis_is_still_discharging() {
    let mut port = GamePort::default();
    port.set_state(Some(JoystickState::joystick_a(128, 200, 0x03)), 0);
    port.charge(10_000);
    let x_deadline = 10_000 + axis_ticks(128);
    let y_deadline = 10_000 + axis_ticks(200);
    assert!(
        x_deadline < y_deadline,
        "the test needs staggered deadlines"
    );

    // Both discharging: axis bits set, buttons masked out of the answer.
    assert_eq!(port.read(10_000), 0xff);
    // Only the slower axis discharging: still masked, so the guard is keyed on
    // "any axis", not on "the first axis".
    assert_eq!(port.read(x_deadline), 0xfe);
    // Past both deadlines the pressed buttons appear.
    assert_eq!(port.read(y_deadline), 0xcc);

    // The masking arm is what hid them: with no buttons pressed the mid-pulse
    // read is identical, so the assertion above is not reading a constant.
    let mut released = port;
    released.set_state(Some(JoystickState::joystick_a(128, 200, 0x00)), 0);
    released.charge(10_000);
    assert_eq!(released.read(10_000), 0xff);
    assert_eq!(released.read(y_deadline), 0xfc);
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

fn attached_state(buttons: u8, turbo_buttons: u8) -> JoystickState {
    JoystickState {
        axes: [128, 128, 0, 0],
        axis_present: 0x03,
        buttons,
        turbo_buttons,
    }
}

#[test]
fn all_four_axes_and_buttons_are_visible_at_the_port_and_bios() {
    let mut port = GamePort::default();
    port.set_state(
        Some(JoystickState {
            axes: [10, 20, 30, 40],
            axis_present: 0x0f,
            buttons: 0x0f,
            turbo_buttons: 0,
        }),
        0,
    );
    assert_eq!(port.read(0), 0x00);
    assert_eq!(port.bios_switches(0), 0x00);
    assert_eq!(port.bios_axes(), [10, 20, 30, 40]);
    port.charge(100);
    assert_eq!(port.read(100), 0xff, "charged axes mask all button lines");
}

#[test]
fn absent_b_axes_stay_open_without_hiding_gravis_c_and_d() {
    let mut port = GamePort::default();
    port.set_state(Some(attached_state(0x0c, 0)), 0);
    assert_eq!(port.read(0), 0x3c);
    port.charge(100);
    let after_a = 100 + axis_ticks(128);
    assert_eq!(port.read(after_a), 0x3c);
}

#[test]
fn replay_keeps_each_asserted_and_released_level_for_guest_time() {
    let mut port = GamePort::default();
    port.apply_update(
        GamePortUpdate {
            state: Some(attached_state(0, 0)),
            button_transitions: [true, false, true, false]
                .into_iter()
                .map(|normal_held| GamePortButtonTransition {
                    line: 0,
                    normal_held,
                    turbo_held: false,
                })
                .collect(),
            reset_replay: false,
        },
        0,
    );
    assert_eq!(port.read(0) & 0x10, 0);
    assert_eq!(port.read(BUTTON_MIN_DWELL_TICKS - 1) & 0x10, 0);
    assert_ne!(port.read(BUTTON_MIN_DWELL_TICKS) & 0x10, 0);
    assert_eq!(port.read(BUTTON_MIN_DWELL_TICKS * 2) & 0x10, 0);
    assert_ne!(port.read(BUTTON_MIN_DWELL_TICKS * 3) & 0x10, 0);
    assert_eq!(port.button_lines[0].len, 0);
}

#[test]
fn replay_overflow_discards_history_and_converges_to_the_final_target() {
    let mut port = GamePort::default();
    let transitions = (0..BUTTON_REPLAY_CAPACITY + 4)
        .map(|index| GamePortButtonTransition {
            line: 0,
            normal_held: index & 1 == 0,
            turbo_held: false,
        })
        .collect();
    port.apply_update(
        GamePortUpdate {
            state: Some(attached_state(0, 0)),
            button_transitions: transitions,
            reset_replay: false,
        },
        0,
    );
    let settled = BUTTON_MIN_DWELL_TICKS * (BUTTON_REPLAY_CAPACITY as u64 + 8);
    assert_ne!(port.read(settled) & 0x10, 0);
    assert_eq!(port.button_lines[0].len, 0);
    assert_eq!(port.button_lines[0].current, ButtonDriveState::default());
}

#[test]
fn turbo_is_ten_hz_half_duty_starts_on_and_uses_guest_ticks() {
    let mut port = GamePort::default();
    let start = 123_456;
    port.set_state(Some(attached_state(0, 0x01)), start);
    assert_eq!(port.read(start) & 0x10, 0);
    assert_eq!(port.read(start + TURBO_HALF_PERIOD_TICKS - 1) & 0x10, 0);
    assert_ne!(port.read(start + TURBO_HALF_PERIOD_TICKS) & 0x10, 0);
    assert_ne!(port.read(start + TURBO_HALF_PERIOD_TICKS * 2 - 1) & 0x10, 0);
    assert_eq!(port.read(start + TURBO_HALF_PERIOD_TICKS * 2) & 0x10, 0);
    assert_eq!(port.read(start + TURBO_HALF_PERIOD_TICKS * 2) & 0x10, 0);
    assert!(!port.is_idle(start + TURBO_HALF_PERIOD_TICKS * 2));
}

#[test]
fn an_unchanged_aggregate_turbo_source_does_not_restart_phase() {
    let mut port = GamePort::default();
    let start = 77_000;
    port.set_state(Some(attached_state(0, 0x01)), start);
    let update_at = start + TURBO_HALF_PERIOD_TICKS + 10;
    port.apply_update(
        GamePortUpdate {
            state: Some(attached_state(0, 0x01)),
            button_transitions: Vec::new(),
            reset_replay: false,
        },
        update_at,
    );
    assert_eq!(port.button_lines[0].turbo_epoch, start);
    assert_ne!(
        port.read(update_at) & 0x10,
        0,
        "phase remains in its OFF half"
    );
}

#[test]
fn normal_drive_overrides_turbo_off_without_restarting_turbo_phase() {
    let mut port = GamePort::default();
    let start = 91_000;
    port.set_state(Some(attached_state(0x01, 0x01)), start);
    assert_eq!(port.read(start + TURBO_HALF_PERIOD_TICKS) & 0x10, 0);
    port.apply_update(
        GamePortUpdate {
            state: Some(attached_state(0, 0x01)),
            button_transitions: vec![GamePortButtonTransition {
                line: 0,
                normal_held: false,
                turbo_held: true,
            }],
            reset_replay: false,
        },
        start + TURBO_HALF_PERIOD_TICKS,
    );
    assert_ne!(
        port.read(start + TURBO_HALF_PERIOD_TICKS + BUTTON_MIN_DWELL_TICKS) & 0x10,
        0
    );
    assert_eq!(port.button_lines[0].turbo_epoch, start);
}
