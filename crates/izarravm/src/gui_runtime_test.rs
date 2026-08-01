// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use winit::keyboard::KeyCode;

fn binding(ctrl: bool, key: &str) -> KeyBinding {
    KeyBinding::new(ctrl, false, false, key)
}

fn context<'a>(
    capturing_bind: bool,
    input_captured: bool,
    input_release: &'a KeyBinding,
    fullscreen: &'a KeyBinding,
) -> KeyRouteContext<'a> {
    KeyRouteContext {
        capturing_bind,
        input_captured,
        input_release,
        fullscreen,
    }
}

fn guest_bytes(route: &KeyRoute, router: &mut HostKeyRouter, policy: HostInputPolicy) -> Vec<u8> {
    match *route {
        KeyRoute::Guest {
            code,
            pressed,
            repeat,
        } => policy.key_scancodes(router.keyboard_mut(), code, pressed, repeat),
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
            key: "KeyA".into(),
            ctrl: false,
            shift: false,
            alt: false,
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
    assert!(policy.release_scancodes(router.keyboard_mut()).is_empty());
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
    assert!(policy.release_scancodes(router.keyboard_mut()).is_empty());
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
            key: "KeyB".into(),
            ctrl: true,
            shift: false,
            alt: false,
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
    assert_eq!(enabled.focus_lost(enabled_policy), vec![0x9d]);
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
