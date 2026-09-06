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
fn guest_key_choices_cover_every_named_set_one_key_once() {
    let choices = GuestKey::choices().collect::<Vec<_>>();
    assert_eq!(choices.len(), GUEST_KEY_CHOICES.len());
    let unique = choices.iter().copied().collect::<BTreeSet<_>>();
    assert_eq!(unique.len(), choices.len());
    assert!(
        choices
            .iter()
            .all(|key| !key.display().starts_with("Set 1"))
    );
}

#[test]
fn guest_key_chords_keep_order_and_remove_duplicates() {
    let shift = GuestKey::from_key_code(KeyCode::ShiftLeft).unwrap();
    let letter = GuestKey::from_key_code(KeyCode::KeyA).unwrap();
    let chord = GuestKeyChord::new([shift, letter, shift]);
    assert_eq!(chord.keys(), [shift, letter]);
    assert_eq!(chord.display(), "Left Shift+A");
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
    let codes = kb.release_all();
    assert!(
        matches!(codes.as_slice(), [0xaa, 0xe0, 0xc8] | [0xe0, 0xc8, 0xaa]),
        "break codes must keep each E0 prefix with its key: {codes:02x?}"
    );
    assert!(kb.release_all().is_empty());
}

#[test]
fn unmapped_key_emits_nothing_and_is_not_tracked() {
    let mut kb = HostKeyboard::default();
    assert!(kb.key(KeyCode::F24, true).is_empty());
    assert!(kb.release_all().is_empty());
}

#[test]
fn guest_key_router_breaks_only_after_the_last_source_releases() {
    let key = GuestKey::from_key_code(KeyCode::Space).unwrap();
    let press = GuestKeyTransition {
        key,
        pressed: true,
        repeat: false,
    };
    let release = GuestKeyTransition {
        pressed: false,
        ..press
    };
    let mut router = GuestKeyRouter::default();
    assert_eq!(router.apply(GuestKeySource::Physical, press), vec![0x39]);
    assert!(
        router
            .apply(GuestKeySource::Controller(7), press)
            .is_empty()
    );
    assert!(router.apply(GuestKeySource::Physical, release).is_empty());
    assert_eq!(
        router.apply(GuestKeySource::Controller(7), release),
        vec![0xb9]
    );
}

#[test]
fn releasing_physical_keys_does_not_release_controller_owned_keys() {
    let key = GuestKey::from_key_code(KeyCode::ArrowUp).unwrap();
    let press = GuestKeyTransition {
        key,
        pressed: true,
        repeat: false,
    };
    let mut router = GuestKeyRouter::default();
    router.apply(GuestKeySource::Controller(1), press);
    router.apply(GuestKeySource::Physical, press);
    assert!(router.release_source(GuestKeySource::Physical).is_empty());
    assert_eq!(
        router.release_source(GuestKeySource::Controller(1)),
        vec![0xe0, 0xc8]
    );
}

#[test]
fn physical_typematic_can_repeat_a_key_with_another_owner() {
    let key = GuestKey::from_key_code(KeyCode::KeyS).unwrap();
    let mut router = GuestKeyRouter::default();
    let press = GuestKeyTransition {
        key,
        pressed: true,
        repeat: false,
    };
    assert_eq!(
        router.apply(GuestKeySource::Controller(2), press),
        vec![0x1f]
    );
    assert!(router.apply(GuestKeySource::Physical, press).is_empty());
    assert_eq!(
        router.apply(
            GuestKeySource::Physical,
            GuestKeyTransition {
                repeat: true,
                ..press
            }
        ),
        vec![0x1f]
    );
}
