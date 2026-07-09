// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn maps_core_and_extended_keys() {
    assert_eq!(keycode_to_set1(KeyCode::Escape), Some((0x01, false)));
    assert_eq!(keycode_to_set1(KeyCode::KeyA), Some((0x1e, false)));
    assert_eq!(keycode_to_set1(KeyCode::ShiftLeft), Some((0x2a, false)));
    assert_eq!(keycode_to_set1(KeyCode::ShiftRight), Some((0x36, false)));
    assert_eq!(keycode_to_set1(KeyCode::IntlBackslash), Some((0x56, false)));
    assert_eq!(keycode_to_set1(KeyCode::Numpad8), Some((0x48, false)));
    assert_eq!(keycode_to_set1(KeyCode::ArrowUp), Some((0x48, true)));
    assert_eq!(keycode_to_set1(KeyCode::ArrowRight), Some((0x4d, true)));
    assert_eq!(keycode_to_set1(KeyCode::ControlRight), Some((0x1d, true)));
    assert_eq!(keycode_to_set1(KeyCode::AltRight), Some((0x38, true)));
    assert_eq!(keycode_to_set1(KeyCode::NumpadDivide), Some((0x35, true)));
    assert_eq!(keycode_to_set1(KeyCode::NumpadEnter), Some((0x1c, true)));
    assert_eq!(keycode_to_set1(KeyCode::Delete), Some((0x53, true)));
    assert_eq!(keycode_to_set1(KeyCode::F24), None);
}

#[test]
fn press_and_release_emit_make_then_break() {
    let mut kb = HostKeyboard::default();
    assert_eq!(kb.key(KeyCode::ShiftLeft, true), vec![0x2a]);
    assert_eq!(kb.key(KeyCode::KeyA, true), vec![0x1e]);
    assert_eq!(kb.key(KeyCode::KeyA, false), vec![0x9e]);
    assert_eq!(kb.key(KeyCode::ShiftLeft, false), vec![0xaa]);
}

#[test]
fn extended_key_carries_the_e0_prefix_both_ways() {
    let mut kb = HostKeyboard::default();
    assert_eq!(kb.key(KeyCode::ArrowRight, true), vec![0xe0, 0x4d]);
    assert_eq!(kb.key(KeyCode::ArrowRight, false), vec![0xe0, 0xcd]);
}

#[test]
fn duplicate_press_of_held_key_is_dropped() {
    let mut kb = HostKeyboard::default();
    assert_eq!(kb.key(KeyCode::KeyA, true), vec![0x1e]);
    assert!(kb.key(KeyCode::KeyA, true).is_empty()); // already held: auto-repeat make
    // The release still emits exactly one break.
    assert_eq!(kb.key(KeyCode::KeyA, false), vec![0x9e]);
    // After release a fresh press emits the make again.
    assert_eq!(kb.key(KeyCode::KeyA, true), vec![0x1e]);
}

#[test]
fn held_non_modifier_can_emit_typematic_make() {
    let mut kb = HostKeyboard::default();
    assert_eq!(kb.key_with_repeat(KeyCode::KeyS, true, false), vec![0x1f]);
    assert_eq!(kb.key_with_repeat(KeyCode::KeyS, true, true), vec![0x1f]);
    assert_eq!(kb.key_with_repeat(KeyCode::KeyS, false, false), vec![0x9f]);
}

#[test]
fn held_modifier_does_not_repeat() {
    let mut kb = HostKeyboard::default();
    assert_eq!(
        kb.key_with_repeat(KeyCode::ControlLeft, true, false),
        vec![0x1d]
    );
    assert!(
        kb.key_with_repeat(KeyCode::ControlLeft, true, true)
            .is_empty()
    );
}

#[test]
fn reports_whether_key_is_held() {
    let mut kb = HostKeyboard::default();
    assert!(!kb.is_held(KeyCode::KeyS));
    kb.key(KeyCode::KeyS, true);
    assert!(kb.is_held(KeyCode::KeyS));
    kb.key(KeyCode::KeyS, false);
    assert!(!kb.is_held(KeyCode::KeyS));
}

#[test]
fn release_all_breaks_every_held_key_then_forgets_them() {
    let mut kb = HostKeyboard::default();
    kb.key(KeyCode::ShiftLeft, true);
    kb.key(KeyCode::ArrowUp, true);
    let mut codes = kb.release_all();
    codes.sort_unstable();
    // 0xaa (shift break) and 0xe0,0xc8 (arrow-up break), order-independent.
    assert_eq!(codes, vec![0xaa, 0xc8, 0xe0]);
    assert!(kb.release_all().is_empty());
}

#[test]
fn unmapped_key_emits_nothing_and_is_not_tracked() {
    let mut kb = HostKeyboard::default();
    assert!(kb.key(KeyCode::F24, true).is_empty());
    assert!(kb.release_all().is_empty());
}
