// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_input::HostKeyboard;
use winit::keyboard::KeyCode;

fn policy(keyboard: bool, mouse: bool, joystick: bool) -> HostInputPolicy {
    let mut config = InputConfig::default();
    config.keyboard = keyboard;
    config.mouse = mouse;
    config.joystick = joystick;
    HostInputPolicy::from_config(&config)
}

#[test]
fn keyboard_mouse_matrix_gates_guest_paths_independently() {
    for keyboard_enabled in [false, true] {
        for mouse_enabled in [false, true] {
            let policy = policy(keyboard_enabled, mouse_enabled, true);
            let mut keyboard = HostKeyboard::default();

            let codes = policy.key_scancodes(&mut keyboard, KeyCode::KeyA, true, false);

            assert_eq!(codes, if keyboard_enabled { vec![0x1e] } else { vec![] });
            assert_eq!(keyboard.is_held(KeyCode::KeyA), keyboard_enabled);
            assert_eq!(policy.mouse_capture_requested(true, false), mouse_enabled);
            assert_eq!(policy.mouse_active(true), mouse_enabled);
            assert!(!policy.mouse_active(false));
            assert!(policy.joystick_enabled());
        }
    }
}

#[test]
fn disabled_keyboard_clears_preheld_keys_without_emitting_scancodes() {
    let mut keyboard = HostKeyboard::default();
    assert_eq!(keyboard.key(KeyCode::KeyA, true), vec![0x1e]);

    let codes = policy(false, true, false).key_scancodes(&mut keyboard, KeyCode::KeyB, true, false);

    assert!(codes.is_empty());
    assert!(!keyboard.is_held(KeyCode::KeyA));
    assert!(!keyboard.is_held(KeyCode::KeyB));
}

#[test]
fn focus_release_returns_breaks_only_when_keyboard_is_enabled() {
    let mut enabled_keyboard = HostKeyboard::default();
    enabled_keyboard.key(KeyCode::KeyA, true);
    assert_eq!(
        policy(true, true, false).release_scancodes(&mut enabled_keyboard),
        vec![0x9e]
    );
    assert!(enabled_keyboard.release_all().is_empty());

    let mut disabled_keyboard = HostKeyboard::default();
    disabled_keyboard.key(KeyCode::KeyA, true);
    assert!(
        policy(false, true, false)
            .release_scancodes(&mut disabled_keyboard)
            .is_empty()
    );
    assert!(disabled_keyboard.release_all().is_empty());
}

#[test]
fn capture_requires_an_enabled_mouse_click_outside_capture() {
    let enabled = policy(true, true, false);
    assert!(enabled.mouse_capture_requested(true, false));
    assert!(!enabled.mouse_capture_requested(false, false));
    assert!(!enabled.mouse_capture_requested(true, true));

    let disabled = policy(true, false, false);
    assert!(!disabled.mouse_capture_requested(true, false));
}
