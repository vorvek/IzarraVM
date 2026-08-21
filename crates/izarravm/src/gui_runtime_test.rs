// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use winit::keyboard::KeyCode;

fn binding(ctrl: bool, key: &str) -> KeyBinding {
    KeyBinding::new(ctrl, false, false, false, key)
}

fn super_binding(key: &str) -> KeyBinding {
    KeyBinding::new(false, false, false, true, key)
}

fn context<'a>(
    capturing_bind: bool,
    input_captured: bool,
    input_release: &'a KeyBinding,
    fullscreen: &'a KeyBinding,
) -> KeyRouteContext<'a> {
    KeyRouteContext {
        capturing_bind,
        capture_modifier_key: false,
        input_captured,
        input_release,
        fullscreen,
        host_super_down: false,
    }
}

fn controller_capture_context<'a>(
    input_release: &'a KeyBinding,
    fullscreen: &'a KeyBinding,
) -> KeyRouteContext<'a> {
    KeyRouteContext {
        capture_modifier_key: true,
        ..context(true, false, input_release, fullscreen)
    }
}

/// A context whose Super state comes from the host hook, not from winit.
fn hook_context<'a>(
    input_captured: bool,
    input_release: &'a KeyBinding,
    fullscreen: &'a KeyBinding,
) -> KeyRouteContext<'a> {
    KeyRouteContext {
        host_super_down: true,
        ..context(false, input_captured, input_release, fullscreen)
    }
}

fn guest_bytes(route: &KeyRoute, router: &mut HostKeyRouter, policy: HostInputPolicy) -> Vec<u8> {
    match *route {
        KeyRoute::Guest {
            code,
            pressed,
            repeat,
        } => policy
            .key_transition(router.keyboard_mut(), code, pressed, repeat)
            .map(|transition| transition.key.scancodes(transition.pressed))
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

#[test]
fn rebind_press_repeat_and_release_never_leak_to_the_guest() {
    let mut router = HostKeyRouter::default();
    let policy = HostInputPolicy::new(true, true, true);
    let input_release = binding(true, "F2");
    let fullscreen = binding(true, "F11");

    let route = router.route(
        KeyCode::KeyA,
        true,
        false,
        context(true, false, &input_release, &fullscreen),
    );
    assert!(guest_bytes(&route, &mut router, policy).is_empty());
    assert_eq!(
        route,
        KeyRoute::Rebind {
            code: KeyCode::KeyA,
            ctrl: false,
            shift: false,
            alt: false,
            super_key: false,
        }
    );

    let repeat = router.route(
        KeyCode::KeyA,
        true,
        true,
        context(false, false, &input_release, &fullscreen),
    );
    assert!(guest_bytes(&repeat, &mut router, policy).is_empty());
    assert_eq!(repeat, KeyRoute::Swallowed);

    let release = router.route(
        KeyCode::KeyA,
        false,
        false,
        context(false, false, &input_release, &fullscreen),
    );
    assert!(guest_bytes(&release, &mut router, policy).is_empty());
    assert_eq!(release, KeyRoute::Swallowed);
    assert!(
        policy
            .release_key_transitions(router.keyboard_mut())
            .is_empty()
    );
}

#[test]
fn controller_chord_capture_swallows_modifiers_and_can_capture_one_alone() {
    let mut router = HostKeyRouter::default();
    let policy = HostInputPolicy::new(true, true, true);
    let input_release = binding(true, "F2");
    let fullscreen = binding(true, "F11");

    let shift = router.route(
        KeyCode::ShiftLeft,
        true,
        false,
        controller_capture_context(&input_release, &fullscreen),
    );
    assert_eq!(shift, KeyRoute::Swallowed);
    assert!(guest_bytes(&shift, &mut router, policy).is_empty());
    assert_eq!(
        router.route(
            KeyCode::KeyA,
            true,
            false,
            controller_capture_context(&input_release, &fullscreen),
        ),
        KeyRoute::Rebind {
            code: KeyCode::KeyA,
            ctrl: false,
            shift: true,
            alt: false,
            super_key: false,
        }
    );
    assert_eq!(
        router.route(
            KeyCode::KeyA,
            false,
            false,
            context(false, false, &input_release, &fullscreen),
        ),
        KeyRoute::Swallowed
    );
    assert_eq!(
        router.route(
            KeyCode::ShiftLeft,
            false,
            false,
            context(false, false, &input_release, &fullscreen),
        ),
        KeyRoute::Swallowed
    );

    assert_eq!(
        router.route(
            KeyCode::AltRight,
            true,
            false,
            controller_capture_context(&input_release, &fullscreen),
        ),
        KeyRoute::Swallowed
    );
    assert_eq!(
        router.route(
            KeyCode::AltRight,
            false,
            false,
            controller_capture_context(&input_release, &fullscreen),
        ),
        KeyRoute::Rebind {
            code: KeyCode::AltRight,
            ctrl: false,
            shift: false,
            alt: false,
            super_key: false,
        }
    );
    assert!(
        policy
            .release_key_transitions(router.keyboard_mut())
            .is_empty()
    );
}

#[test]
fn held_guest_key_repeat_cannot_become_a_rebind_trigger() {
    let mut router = HostKeyRouter::default();
    let policy = HostInputPolicy::new(true, true, true);
    let input_release = binding(true, "F2");
    let fullscreen = binding(true, "F11");

    let press = router.route(
        KeyCode::KeyA,
        true,
        false,
        context(false, false, &input_release, &fullscreen),
    );
    assert_eq!(guest_bytes(&press, &mut router, policy), vec![0x1e]);

    let repeat = router.route(
        KeyCode::KeyA,
        true,
        true,
        context(true, false, &input_release, &fullscreen),
    );
    assert!(matches!(repeat, KeyRoute::Guest { repeat: true, .. }));
    assert_eq!(guest_bytes(&repeat, &mut router, policy), vec![0x1e]);

    let release = router.route(
        KeyCode::KeyA,
        false,
        false,
        context(false, false, &input_release, &fullscreen),
    );
    assert!(matches!(release, KeyRoute::Guest { pressed: false, .. }));
    assert_eq!(guest_bytes(&release, &mut router, policy), vec![0x9e]);
    assert!(
        policy
            .release_key_transitions(router.keyboard_mut())
            .is_empty()
    );
}

#[test]
fn keyboard_off_keeps_fullscreen_rebind_and_input_release_host_side() {
    let mut router = HostKeyRouter::default();
    let policy = HostInputPolicy::new(false, true, true);
    let input_release = binding(true, "F2");
    let fullscreen = binding(true, "F11");

    let ctrl = router.route(
        KeyCode::ControlLeft,
        true,
        false,
        context(false, true, &input_release, &fullscreen),
    );
    assert!(guest_bytes(&ctrl, &mut router, policy).is_empty());

    let fullscreen_route = router.route(
        KeyCode::F11,
        true,
        false,
        context(false, true, &input_release, &fullscreen),
    );
    assert_eq!(fullscreen_route, KeyRoute::ToggleFullscreen);
    assert!(guest_bytes(&fullscreen_route, &mut router, policy).is_empty());
    assert_eq!(
        router.route(
            KeyCode::F11,
            false,
            false,
            context(false, true, &input_release, &fullscreen),
        ),
        KeyRoute::Swallowed
    );

    let release_route = router.route(
        KeyCode::F2,
        true,
        false,
        context(false, true, &input_release, &fullscreen),
    );
    assert_eq!(release_route, KeyRoute::ReleaseCapture);
    assert_eq!(
        router.route(
            KeyCode::F2,
            false,
            false,
            context(false, false, &input_release, &fullscreen),
        ),
        KeyRoute::Swallowed
    );

    let rebind = router.route(
        KeyCode::KeyB,
        true,
        false,
        context(true, false, &input_release, &fullscreen),
    );
    assert_eq!(
        rebind,
        KeyRoute::Rebind {
            code: KeyCode::KeyB,
            ctrl: true,
            shift: false,
            alt: false,
            super_key: false,
        }
    );
    assert!(guest_bytes(&rebind, &mut router, policy).is_empty());
}

#[test]
fn focus_loss_releases_enabled_keys_and_silently_clears_disabled_keys() {
    let input_release = binding(true, "F2");
    let fullscreen = binding(true, "F11");

    let mut enabled = HostKeyRouter::default();
    let enabled_policy = HostInputPolicy::new(true, true, true);
    let ctrl = enabled.route(
        KeyCode::ControlLeft,
        true,
        false,
        context(false, false, &input_release, &fullscreen),
    );
    assert_eq!(guest_bytes(&ctrl, &mut enabled, enabled_policy), vec![0x1d]);
    assert_eq!(
        enabled.route(
            KeyCode::F11,
            true,
            false,
            context(false, false, &input_release, &fullscreen),
        ),
        KeyRoute::ToggleFullscreen
    );
    assert!(enabled.ctrl_down);
    assert!(enabled.swallowed.contains(&KeyCode::F11));
    assert_eq!(
        enabled
            .focus_lost(enabled_policy)
            .into_iter()
            .flat_map(|transition| transition.key.scancodes(transition.pressed))
            .collect::<Vec<_>>(),
        vec![0x9d]
    );
    assert!(!enabled.ctrl_down);
    assert!(enabled.swallowed.is_empty());
    assert!(!enabled.is_pressed(KeyCode::ControlLeft));
    assert!(!enabled.is_pressed(KeyCode::F11));
    assert!(enabled.focus_lost(enabled_policy).is_empty());

    let mut disabled = HostKeyRouter::default();
    let disabled_policy = HostInputPolicy::new(false, true, true);
    let ctrl = disabled.route(
        KeyCode::ControlLeft,
        true,
        false,
        context(false, false, &input_release, &fullscreen),
    );
    assert!(guest_bytes(&ctrl, &mut disabled, disabled_policy).is_empty());
    assert!(matches!(
        disabled.route(
            KeyCode::KeyC,
            true,
            false,
            context(true, false, &input_release, &fullscreen),
        ),
        KeyRoute::Rebind { .. }
    ));
    assert!(disabled.ctrl_down);
    assert!(disabled.swallowed.contains(&KeyCode::KeyC));
    assert!(disabled.focus_lost(disabled_policy).is_empty());
    assert!(!disabled.ctrl_down);
    assert!(disabled.swallowed.is_empty());
    assert!(!disabled.is_pressed(KeyCode::ControlLeft));
    assert!(!disabled.is_pressed(KeyCode::KeyC));
}

#[test]
fn rebind_precedes_release_and_release_precedes_fullscreen() {
    let mut router = HostKeyRouter::default();
    let same = binding(false, "F11");
    assert!(matches!(
        router.route(KeyCode::F11, true, false, context(true, true, &same, &same),),
        KeyRoute::Rebind { .. }
    ));
    assert_eq!(
        router.route(
            KeyCode::F11,
            false,
            false,
            context(false, true, &same, &same),
        ),
        KeyRoute::Swallowed
    );
    assert_eq!(
        router.route(
            KeyCode::F11,
            true,
            false,
            context(false, true, &same, &same),
        ),
        KeyRoute::ReleaseCapture
    );
}

#[test]
fn super_is_a_modifier_and_never_reaches_the_guest_or_a_rebind() {
    let mut router = HostKeyRouter::default();
    let policy = HostInputPolicy::new(true, true, true);
    let input_release = super_binding("F2");
    let fullscreen = super_binding("F4");

    // A Super press while the modal waits for a hotkey stays a modifier: it
    // does not close the capture, and the AT keyboard has no code for it.
    let held = router.route(
        KeyCode::SuperLeft,
        true,
        false,
        context(true, true, &input_release, &fullscreen),
    );
    assert_eq!(held, KeyRoute::Swallowed);
    assert!(guest_bytes(&held, &mut router, policy).is_empty());
    assert!(router.super_down);

    assert_eq!(
        router.route(
            KeyCode::KeyA,
            true,
            false,
            context(true, true, &input_release, &fullscreen),
        ),
        KeyRoute::Rebind {
            code: KeyCode::KeyA,
            ctrl: false,
            shift: false,
            alt: false,
            super_key: true,
        }
    );

    assert_eq!(
        router.route(
            KeyCode::F2,
            true,
            false,
            context(false, true, &input_release, &fullscreen),
        ),
        KeyRoute::ReleaseCapture
    );
    assert_eq!(
        router.route(
            KeyCode::F4,
            true,
            false,
            context(false, true, &input_release, &fullscreen),
        ),
        KeyRoute::ToggleFullscreen
    );

    // The right Super key is the same modifier, and focus loss clears it.
    assert_eq!(
        router.route(
            KeyCode::SuperLeft,
            false,
            false,
            context(false, true, &input_release, &fullscreen),
        ),
        KeyRoute::Swallowed
    );
    assert!(!router.super_down);
    let _ = router.route(
        KeyCode::SuperRight,
        true,
        false,
        context(false, true, &input_release, &fullscreen),
    );
    assert!(router.super_down);
    assert!(router.focus_lost(policy).is_empty());
    assert!(!router.super_down);
}

#[test]
fn a_super_key_the_host_hook_swallowed_still_arms_the_hotkey() {
    let mut router = HostKeyRouter::default();
    let input_release = super_binding("F2");
    let fullscreen = super_binding("F4");

    // The hook discarded the Super press, so the router never saw it. The
    // hotkeys must still fire, or capture could not be released at all.
    assert!(!router.super_down);
    assert_eq!(
        router.route(
            KeyCode::F4,
            true,
            false,
            hook_context(true, &input_release, &fullscreen),
        ),
        KeyRoute::ToggleFullscreen
    );
    assert_eq!(
        router.route(
            KeyCode::F2,
            true,
            false,
            hook_context(true, &input_release, &fullscreen),
        ),
        KeyRoute::ReleaseCapture
    );

    // Without the hook's Super state the same key is plain guest input.
    let mut plain = HostKeyRouter::default();
    assert!(matches!(
        plain.route(
            KeyCode::F2,
            true,
            false,
            context(false, true, &input_release, &fullscreen),
        ),
        KeyRoute::Guest { .. }
    ));
}
