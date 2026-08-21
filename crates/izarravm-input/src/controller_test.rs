// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn device() -> ControllerDeviceMatcher {
    ControllerDeviceMatcher {
        backend: "gilrs-wgi".into(),
        platform: "windows".into(),
        guid: "controller-guid".into(),
        vendor_id: Some(0x1234),
        product_id: Some(0x5678),
        name: "Test Controller".into(),
        occurrence: 0,
    }
}

fn no_deadzone(span: AxisSpan, inverted: bool) -> AxisTransform {
    AxisTransform {
        span,
        inverted,
        calibration: AxisCalibration {
            deadzone: 0.0,
            ..AxisCalibration::default()
        },
    }
}

fn batch(
    events: Vec<HostControlValue>,
    final_values: Vec<HostControlValue>,
) -> HostControllerBatch {
    HostControllerBatch {
        connected: true,
        reset: false,
        events,
        final_values,
    }
}

fn value(control: HostControlId, value: f32) -> HostControlValue {
    HostControlValue { control, value }
}

#[test]
fn keyboard_profile_covers_the_common_physical_controls_once() {
    let controls = keyboard_controls();
    assert_eq!(controls.len(), 24);
    for (index, control) in controls.iter().enumerate() {
        assert!(
            !controls[..index]
                .iter()
                .any(|earlier| earlier.host == control.host),
            "duplicate keyboard control {}",
            control.label
        );
    }

    let config = ControllerConfig::default_keyboard(device());
    assert_eq!(config.profile, GuestControllerProfile::KeyboardOnly);
    assert_eq!(config.keys.len(), controls.len());
    assert!(config.buttons.is_empty());
    assert!(
        config
            .keys
            .iter()
            .all(|binding| controls.iter().any(|control| control.host == binding.host))
    );
}

#[test]
fn profile_changes_replace_target_specific_defaults() {
    let mut config = ControllerConfig::default_keyboard(device());
    config.apply_profile_defaults(GuestControllerProfile::Standard);
    assert_eq!(config.buttons.len(), 2);
    assert!(config.keys.is_empty());

    config.apply_profile_defaults(GuestControllerProfile::KeyboardOnly);
    assert!(config.buttons.is_empty());
    assert_eq!(config.keys.len(), keyboard_controls().len());

    config.buttons = default_button_bindings(GuestControllerProfile::Gravis {
        mode: GravisMode::FourButton,
        handedness: GravisHandedness::RightHanded,
    });
    config.profile = GuestControllerProfile::Standard;
    config.normalize_profile_bindings();
    assert_eq!(config.buttons.len(), 2);
    assert!(config.buttons.iter().all(|binding| binding.action < 2));
}

#[test]
fn trigger_axis_bindings_accept_digital_trigger_events() {
    let left_axis = HostControlId::semantic_axis(JoystickAxis::LeftZ);
    let left_button = HostControlId::semantic_button(JoystickButton::LeftTrigger2);
    let right_axis = HostControlId::semantic_axis(JoystickAxis::RightZ);
    let right_button = HostControlId::semantic_button(JoystickButton::RightTrigger2);
    assert!(!left_axis.matches(left_button));
    assert!(!left_button.matches(left_axis));
    assert!(!right_axis.matches(right_button));
    assert!(!right_button.matches(right_axis));
    assert!(!left_axis.matches(right_button));
    assert_eq!(
        resolve_control_value(&[value(left_button, 1.0)], left_axis),
        Some(1.0)
    );

    let space = GuestKey::from_key_code(winit::keyboard::KeyCode::Space).unwrap();
    let mut config = ControllerConfig::default_keyboard(device());
    let trigger = config
        .keys
        .iter_mut()
        .find(|binding| binding.host.host == left_axis)
        .expect("left trigger row");
    trigger.guest = space.into();
    let mut mapper = ControllerMapper::new(config);
    let delta = mapper.apply(batch(
        vec![value(left_button, 1.0)],
        vec![value(left_button, 1.0)],
    ));
    assert_eq!(delta.keys.len(), 1);
    assert_eq!(delta.keys[0].transition.key, space);
    assert!(delta.keys[0].transition.pressed);
    assert!(delta.gameport.is_none());
    assert!(!delta.reset_gameport);
}

#[test]
fn control_resolution_uses_one_representation_in_priority_order() {
    let left_axis = HostControlId::semantic_axis(JoystickAxis::LeftZ);
    let left_button = HostControlId::semantic_button(JoystickButton::LeftTrigger2);
    let coexist = [value(left_axis, -1.0), value(left_button, 1.0)];
    assert_eq!(resolve_control_value(&coexist, left_axis), Some(-1.0));
    let negative = HostDigitalBinding {
        host: left_axis,
        direction: DigitalDirection::Negative,
    };
    assert!(negative.update(resolve_control_value(&coexist, left_axis).unwrap(), false));

    let persisted = HostControlId {
        kind: HostControlKind::Axis,
        raw_code: Some(7),
        semantic: Some(HostSemanticControl::Axis(JoystickAxis::LeftStickX)),
    };
    let repurposed_raw = HostControlId {
        kind: HostControlKind::Axis,
        raw_code: Some(7),
        semantic: Some(HostSemanticControl::Axis(JoystickAxis::RightStickX)),
    };
    let semantic_fallback = HostControlId {
        kind: HostControlKind::Axis,
        raw_code: Some(9),
        semantic: Some(HostSemanticControl::Axis(JoystickAxis::LeftStickX)),
    };
    assert_eq!(
        resolve_control_value(
            &[value(repurposed_raw, 0.75), value(semantic_fallback, -0.5),],
            persisted,
        ),
        Some(-0.5)
    );
}

#[test]
fn raw_only_code_zero_is_a_real_control_identity() {
    let raw_zero = HostControlId {
        kind: HostControlKind::Button,
        raw_code: Some(0),
        semantic: None,
    };
    assert!(raw_zero.matches(raw_zero));
    assert_eq!(
        resolve_control_value(&[value(raw_zero, 0.8)], raw_zero),
        Some(0.8)
    );
    let semantic_zero = HostControlId {
        kind: HostControlKind::Button,
        raw_code: Some(0),
        semantic: Some(HostSemanticControl::Button(JoystickButton::West)),
    };
    assert_eq!(
        resolve_control_value(&[value(semantic_zero, 0.6)], raw_zero),
        Some(0.6),
        "a raw binding survives when that raw code later gains a semantic name"
    );
    assert_eq!(
        HostControlId::semantic_button(JoystickButton::West).raw_code,
        None,
        "a semantic-only default must not claim raw code zero"
    );
}

#[test]
fn strong_device_identity_never_falls_back_to_a_shared_name() {
    let expected = device();
    let mut wrong_guid = expected.clone();
    wrong_guid.guid = "other-guid".into();
    assert!(!device_matches(&expected, &wrong_guid));
    let mut wrong_ids = expected.clone();
    wrong_ids.vendor_id = Some(0xabcd);
    assert!(!device_matches(&expected, &wrong_ids));

    let mut expected_usb = expected.clone();
    expected_usb.guid.clear();
    let mut wrong_usb = expected_usb.clone();
    wrong_usb.vendor_id = Some(0xabcd);
    wrong_usb.product_id = Some(0xef01);
    assert!(!device_matches(&expected_usb, &wrong_usb));

    let mut name_only = expected.clone();
    name_only.guid.clear();
    name_only.vendor_id = None;
    name_only.product_id = None;
    assert!(device_matches(&name_only, &expected));
    let mut other_occurrence = expected.clone();
    other_occurrence.occurrence = 1;
    assert!(!device_matches(&expected, &other_occurrence));
}

#[test]
fn identical_devices_keep_their_occurrences_across_hot_plug() {
    let device_at = |runtime_id, occurrence| ControllerDevice {
        runtime_id,
        matcher: ControllerDeviceMatcher {
            occurrence,
            ..device()
        },
    };
    let previous = [device_at(10, 0), device_at(20, 1)];
    let mut after_unplug = [device_at(20, 0)];
    assign_device_occurrences(&previous, &mut after_unplug);
    assert_eq!(after_unplug[0].matcher.occurrence, 1);

    let retained = after_unplug.to_vec();
    let mut after_replug = [device_at(20, 0), device_at(30, 0)];
    assign_device_occurrences(&retained, &mut after_replug);
    assert_eq!(after_replug[0].matcher.occurrence, 1);
    assert_eq!(after_replug[1].matcher.occurrence, 0);
}

#[test]
fn keyboard_chords_press_modifiers_first_and_release_them_last() {
    let shift = GuestKey::from_key_code(winit::keyboard::KeyCode::ShiftLeft).unwrap();
    let letter = GuestKey::from_key_code(winit::keyboard::KeyCode::KeyA).unwrap();
    let south = HostControlId::semantic_button(JoystickButton::South);
    let mut config = ControllerConfig::default_keyboard(device());
    config
        .keys
        .iter_mut()
        .find(|binding| binding.host.host == south)
        .expect("face A row")
        .guest = GuestKeyChord::new([shift, letter]);
    let mut mapper = ControllerMapper::new(config);

    let press = mapper.apply(batch(vec![value(south, 1.0)], vec![value(south, 1.0)]));
    assert_eq!(
        press
            .keys
            .iter()
            .map(|change| (change.transition.key, change.transition.pressed))
            .collect::<Vec<_>>(),
        [(shift, true), (letter, true)]
    );
    assert!(press.gameport.is_none());
    assert!(!press.reset_gameport);

    let release = mapper.apply(batch(vec![value(south, 0.0)], vec![value(south, 0.0)]));
    assert_eq!(
        release
            .keys
            .iter()
            .map(|change| (change.transition.key, change.transition.pressed))
            .collect::<Vec<_>>(),
        [(letter, false), (shift, false)]
    );
}

#[test]
fn reconnect_latch_preserves_reset_and_waits_for_neutral() {
    let mut latch = ControllerConnectionLatch::default();
    assert_eq!(
        latch.update(Some(7), false, true),
        ControllerConnectionDecision {
            connected: true,
            reset: true,
        }
    );
    assert_eq!(
        latch.update(Some(7), true, true),
        ControllerConnectionDecision {
            connected: true,
            reset: true,
        },
        "same-poll disconnect and reconnect must retain the reset edge"
    );
    assert_eq!(
        latch.update(Some(7), true, false),
        ControllerConnectionDecision {
            connected: false,
            reset: true,
        }
    );
    assert_eq!(
        latch.update(Some(7), false, false),
        ControllerConnectionDecision {
            connected: false,
            reset: true,
        }
    );
    assert_eq!(
        latch.update(Some(7), false, true),
        ControllerConnectionDecision {
            connected: true,
            reset: false,
        }
    );
}

#[test]
fn reconnect_neutrality_respects_full_and_half_axis_spans() {
    let mut config = ControllerConfig::default_keyboard(device());
    config.apply_profile_defaults(GuestControllerProfile::WheelPedals);
    let axis_values = |left_x, left_z, right_z, right_y| {
        vec![
            value(
                HostControlId::semantic_axis(JoystickAxis::LeftStickX),
                left_x,
            ),
            value(HostControlId::semantic_axis(JoystickAxis::LeftZ), left_z),
            value(HostControlId::semantic_axis(JoystickAxis::RightZ), right_z),
            value(
                HostControlId::semantic_axis(JoystickAxis::RightStickY),
                right_y,
            ),
        ]
    };

    assert!(controls_neutral(
        &config,
        &axis_values(0.0, -1.0, -1.0, -1.0)
    ));
    assert!(!controls_neutral(
        &config,
        &axis_values(0.21, -1.0, -1.0, -1.0)
    ));
    assert!(!controls_neutral(
        &config,
        &axis_values(0.0, 0.21, -1.0, -1.0)
    ));

    config.axes[1].transform.span = AxisSpan::NegativeHalf;
    assert!(controls_neutral(
        &config,
        &axis_values(0.0, 1.0, -1.0, -1.0)
    ));
    assert!(!controls_neutral(
        &config,
        &axis_values(0.0, -0.21, -1.0, -1.0)
    ));
}

#[test]
fn axis_spans_and_inversion_have_exact_endpoints() {
    let full = no_deadzone(AxisSpan::Full, false);
    assert_eq!(
        [full.apply(-1.0), full.apply(0.0), full.apply(1.0)],
        [0, 128, 255]
    );

    let positive = no_deadzone(AxisSpan::PositiveHalf, false);
    assert_eq!(
        [
            positive.apply(-1.0),
            positive.apply(0.0),
            positive.apply(0.5),
            positive.apply(1.0),
        ],
        [0, 0, 128, 255]
    );

    let negative = no_deadzone(AxisSpan::NegativeHalf, false);
    assert_eq!(
        [
            negative.apply(-1.0),
            negative.apply(-0.5),
            negative.apply(0.0),
            negative.apply(1.0),
        ],
        [255, 128, 0, 0]
    );
    let inverted = no_deadzone(AxisSpan::PositiveHalf, true);
    assert_eq!([inverted.apply(0.0), inverted.apply(1.0)], [255, 0]);
}

#[test]
fn legacy_axes_keep_deadzone_polarity_and_rounding() {
    let legacy = JoystickBinding {
        controller_uuid: "legacy".into(),
        controller_name: "Legacy".into(),
        x: JoystickAxisBinding {
            control: JoystickAxis::LeftStickX,
            polarity: JoystickPolarity::Positive,
        },
        y: JoystickAxisBinding {
            control: JoystickAxis::LeftStickY,
            polarity: JoystickPolarity::Negative,
        },
        button_1: JoystickButton::South,
        button_2: JoystickButton::East,
    };
    let migrated = ControllerConfig::from_legacy(legacy);
    let x = migrated.axes[0].transform;
    let y = migrated.axes[1].transform;
    let old = |raw: f32, negative: bool| {
        let raw = if negative { -raw } else { raw }.clamp(-1.0, 1.0);
        let scaled = if raw.abs() <= 0.15 {
            0.0
        } else {
            raw.signum() * (raw.abs() - 0.15) / 0.85
        };
        ((scaled + 1.0) * 127.5).round() as u8
    };
    for raw in [-1.0, -0.151, -0.15, 0.0, 0.15, 0.151, 1.0] {
        assert_eq!(x.apply(raw), old(raw, false), "positive {raw}");
        assert_eq!(y.apply(raw), old(raw, true), "negative {raw}");
    }
}

#[test]
fn ordered_button_edges_preserve_two_taps_in_one_batch() {
    let button = HostControlId::semantic_button(JoystickButton::South);
    let mut config = ControllerConfig::default_gravis(device());
    config.buttons.truncate(1);
    let mut mapper = ControllerMapper::new(config);
    let delta = mapper.apply(batch(
        vec![
            value(button, 1.0),
            value(button, 0.0),
            value(button, 1.0),
            value(button, 0.0),
        ],
        vec![value(button, 0.0)],
    ));
    assert_eq!(
        delta
            .button_transitions
            .iter()
            .map(|state| state.normal_held)
            .collect::<Vec<_>>(),
        [true, false, true, false]
    );
    assert_eq!(delta.gameport.unwrap().normal_buttons, 0);
}

#[test]
fn source_counting_does_not_release_a_line_owned_by_another_binding() {
    let south = HostControlId::semantic_button(JoystickButton::South);
    let dpad_up = HostControlId::semantic_button(JoystickButton::DPadUp);
    let mut config = ControllerConfig::default_gravis(device());
    config.buttons = vec![
        ControllerButtonBinding {
            host: HostDigitalBinding {
                host: south,
                direction: DigitalDirection::Positive,
            },
            action: 0,
        },
        ControllerButtonBinding {
            host: HostDigitalBinding {
                host: dpad_up,
                direction: DigitalDirection::Positive,
            },
            action: 0,
        },
    ];
    let mut mapper = ControllerMapper::new(config);
    let first = mapper.apply(batch(
        vec![value(south, 1.0), value(dpad_up, 1.0), value(south, 0.0)],
        vec![value(south, 0.0), value(dpad_up, 1.0)],
    ));
    assert_eq!(first.button_transitions.len(), 1);
    assert!(first.button_transitions[0].normal_held);
    let last = mapper.apply(batch(
        vec![value(dpad_up, 0.0)],
        vec![value(south, 0.0), value(dpad_up, 0.0)],
    ));
    assert_eq!(last.button_transitions.len(), 1);
    assert!(!last.button_transitions[0].normal_held);
}

#[test]
fn gravis_two_button_mode_routes_c_and_d_to_turbo_a_and_b() {
    let west = HostControlId::semantic_button(JoystickButton::West);
    let north = HostControlId::semantic_button(JoystickButton::North);
    let mut config = ControllerConfig::default_gravis(device());
    config.profile = GuestControllerProfile::Gravis {
        mode: GravisMode::TwoButtonTurbo,
        handedness: GravisHandedness::RightHanded,
    };
    let mut mapper = ControllerMapper::new(config);
    let delta = mapper.apply(batch(
        vec![value(west, 1.0), value(north, 1.0)],
        vec![value(west, 1.0), value(north, 1.0)],
    ));
    let state = delta.gameport.unwrap();
    assert_eq!(state.normal_buttons, 0);
    assert_eq!(state.turbo_buttons, 0x03);
}

#[test]
fn analog_to_digital_mapping_uses_press_and_release_hysteresis() {
    let axis = HostControlId::semantic_axis(JoystickAxis::LeftStickX);
    let key = GuestKey::from_key_code(winit::keyboard::KeyCode::Space).unwrap();
    let mut config = ControllerConfig::default_gravis(device());
    config.keys.push(ControllerKeyBinding {
        host: HostDigitalBinding {
            host: axis,
            direction: DigitalDirection::Positive,
        },
        guest: key.into(),
    });
    let mut mapper = ControllerMapper::new(config);
    let press = mapper.apply(batch(vec![value(axis, 0.65)], vec![value(axis, 0.65)]));
    assert_eq!(press.keys.len(), 1);
    assert!(press.keys[0].transition.pressed);
    let held = mapper.apply(batch(vec![value(axis, 0.55)], vec![value(axis, 0.55)]));
    assert!(held.keys.is_empty());
    let release = mapper.apply(batch(vec![value(axis, 0.50)], vec![value(axis, 0.50)]));
    assert_eq!(release.keys.len(), 1);
    assert!(!release.keys[0].transition.pressed);
}

#[test]
fn disconnect_releases_controller_keys_and_neutralizes_gameport() {
    let button = HostControlId::semantic_button(JoystickButton::South);
    let key = GuestKey::from_key_code(winit::keyboard::KeyCode::Space).unwrap();
    let mut config = ControllerConfig::default_gravis(device());
    config.keys.push(ControllerKeyBinding {
        host: HostDigitalBinding {
            host: button,
            direction: DigitalDirection::Positive,
        },
        guest: key.into(),
    });
    let mut mapper = ControllerMapper::new(config);
    mapper.apply(batch(vec![value(button, 1.0)], vec![value(button, 1.0)]));
    let delta = mapper.apply(HostControllerBatch {
        connected: false,
        reset: true,
        events: Vec::new(),
        final_values: Vec::new(),
    });
    assert!(delta.reset_gameport);
    assert!(delta.gameport.is_none());
    assert_eq!(delta.keys.len(), 1);
    assert!(!delta.keys[0].transition.pressed);
}
