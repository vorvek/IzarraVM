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
    assert_eq!(port.read(1_000), 0xf0);
    assert_eq!(port.discharge_deadlines, [0; 2]);
}
