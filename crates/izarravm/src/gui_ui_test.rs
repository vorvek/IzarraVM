// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use winit::keyboard::KeyCode;

fn visual_value(control: HostControlId, value: f32) -> HostControlValue {
    HostControlValue { control, value }
}

#[test]
fn capture_builds_modifiers_before_the_typed_key() {
    let chord = guest_chord_from_capture(KeyCode::KeyA, true, true, true).unwrap();
    assert_eq!(
        chord.keys(),
        [
            GuestKey::from_key_code(KeyCode::ControlLeft).unwrap(),
            GuestKey::from_key_code(KeyCode::ShiftLeft).unwrap(),
            GuestKey::from_key_code(KeyCode::AltLeft).unwrap(),
            GuestKey::from_key_code(KeyCode::KeyA).unwrap(),
        ]
    );
}

#[test]
fn repeated_controller_names_receive_stable_ordinals() {
    let matcher = |occurrence| ControllerDeviceMatcher {
        backend: "gilrs".to_owned(),
        platform: "windows".to_owned(),
        guid: "same-guid".to_owned(),
        vendor_id: Some(1),
        product_id: Some(2),
        name: "Xbox controller".to_owned(),
        occurrence,
    };
    let devices = [
        ControllerDevice {
            runtime_id: 4,
            matcher: matcher(0),
        },
        ControllerDevice {
            runtime_id: 9,
            matcher: matcher(1),
        },
    ];
    assert_eq!(
        controller_device_display_name(&devices, &devices[0].matcher),
        "Xbox controller (1)"
    );
    assert_eq!(
        controller_device_display_name(&devices, &devices[1].matcher),
        "Xbox controller (2)"
    );
}

#[test]
fn windows_controller_names_use_backend_scoped_suffixes() {
    let matcher = |backend: &str, occurrence| ControllerDeviceMatcher {
        backend: backend.to_owned(),
        platform: "windows".to_owned(),
        guid: format!("{backend}-{occurrence}"),
        vendor_id: None,
        product_id: None,
        name: "Controller".to_owned(),
        occurrence,
    };
    let devices = [
        ControllerDevice {
            runtime_id: 0,
            matcher: matcher("gilrs-wgi", 0),
        },
        ControllerDevice {
            runtime_id: 1,
            matcher: matcher("gilrs-wgi", 1),
        },
        ControllerDevice {
            runtime_id: 0,
            matcher: matcher("xinput", 0),
        },
    ];
    assert_eq!(
        controller_device_display_name(&devices, &devices[0].matcher),
        "Controller (1) (WGI)"
    );
    assert_eq!(
        controller_device_display_name(&devices, &devices[1].matcher),
        "Controller (2) (WGI)"
    );
    assert_eq!(
        controller_device_display_name(&devices, &devices[2].matcher),
        "Controller (XInput)"
    );
}

#[test]
fn hardware_name_upgrade_updates_selected_and_staged_matchers() {
    let live = ControllerDeviceMatcher {
        backend: "gilrs-wgi".into(),
        platform: "windows".into(),
        guid: "00000000-0000-0000-0000-000000000000".into(),
        vendor_id: Some(0x3434),
        product_id: Some(0x1061),
        name: "Xbox 360 Controller for Windows".into(),
        occurrence: 0,
    };
    let mut resolved = live.clone();
    resolved.name = "Keychron Q6 HE 8K".into();
    let mut selected = live.clone();
    let mut staged = live.clone();
    assert!(upgrade_controller_name(
        &mut selected,
        &live,
        Some(&resolved.name)
    ));
    assert!(upgrade_controller_name(
        &mut staged,
        &live,
        Some(&resolved.name)
    ));
    assert_eq!(selected.name, "Keychron Q6 HE 8K");
    assert_eq!(staged.name, "Keychron Q6 HE 8K");
    assert!(selected.strongly_matches(&live));
}

#[test]
fn name_only_legacy_matcher_is_not_rewritten() {
    let live = ControllerDeviceMatcher {
        backend: "gilrs-wgi".into(),
        platform: "windows".into(),
        guid: String::new(),
        vendor_id: None,
        product_id: None,
        name: "Generic pad".into(),
        occurrence: 0,
    };
    let mut resolved = live.clone();
    resolved.name = "Guessed hardware".into();
    let mut target = live.clone();
    assert!(!upgrade_controller_name(
        &mut target,
        &live,
        Some(&resolved.name)
    ));
    assert_eq!(target.name, "Generic pad");
}

#[test]
fn missing_hardware_name_never_downgrades_an_enriched_matcher() {
    let live = ControllerDeviceMatcher {
        backend: "gilrs-wgi".into(),
        platform: "windows".into(),
        guid: "00000000-0000-0000-0000-000000000000".into(),
        vendor_id: Some(0x3434),
        product_id: Some(0x1061),
        name: "Xbox 360 Controller for Windows".into(),
        occurrence: 0,
    };
    let mut target = live.clone();
    target.name = "Keychron Q6 HE 8K".into();
    assert!(!upgrade_controller_name(&mut target, &live, None));
    assert_eq!(target.name, "Keychron Q6 HE 8K");
}

#[test]
fn capture_keeps_a_modifier_typed_on_its_own() {
    let chord = guest_chord_from_capture(KeyCode::ShiftRight, false, false, false).unwrap();
    assert_eq!(
        chord.keys(),
        [GuestKey::from_key_code(KeyCode::ShiftRight).unwrap()]
    );
    assert!(guest_chord_from_capture(KeyCode::SuperLeft, false, false, false).is_none());
}

#[test]
fn overlay_coordinates_follow_the_exact_painted_rectangle() {
    let rect = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(480.0, 60.0));
    assert_eq!(
        controller_point(rect, FACE_VIEWBOX, [120.0, 60.0]),
        egui::pos2(250.0, 50.0)
    );
    assert_eq!(
        controller_rect(rect, FACE_VIEWBOX, [0.0, 0.0, 240.0, 120.0]),
        rect
    );
}

#[test]
fn stick_caps_clamp_axes_and_invert_screen_y() {
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(240.0, 120.0));
    assert_eq!(
        controller_stick_point(rect, FACE_VIEWBOX, [86.0, 82.0], [2.0, -2.0], 7.0,),
        egui::pos2(93.0, 89.0)
    );
    assert_eq!(
        controller_stick_point(rect, FACE_VIEWBOX, [86.0, 82.0], [-1.0, 1.0], 7.0,),
        egui::pos2(79.0, 75.0)
    );
}

#[test]
fn visual_state_accepts_button_and_axis_dpad_and_trigger_reports() {
    let values = [
        visual_value(HostControlId::semantic_button(JoystickButton::DPadUp), 1.0),
        visual_value(HostControlId::semantic_axis(JoystickAxis::DPadX), -1.0),
        visual_value(HostControlId::semantic_axis(JoystickAxis::LeftZ), -1.0),
        visual_value(
            HostControlId::semantic_button(JoystickButton::LeftTrigger2),
            1.0,
        ),
        visual_value(HostControlId::semantic_axis(JoystickAxis::RightZ), 1.0),
        visual_value(HostControlId::semantic_axis(JoystickAxis::LeftStickX), 1.5),
    ];
    let state = ControllerVisualState::from_values(&values);
    assert_eq!(state.dpad, [true, false, true, false]);
    assert_eq!(state.shoulders, [true, false, false, true]);
    assert_eq!(state.left_stick, [1.0, 0.0]);
}

#[test]
fn embedded_controller_svgs_render_without_external_assets() {
    let options = Default::default();
    assert!(egui_extras::image::load_svg_bytes(CONTROLLER_FACE_SVG, &options).is_ok());
    assert!(egui_extras::image::load_svg_bytes(CONTROLLER_SHOULDERS_SVG, &options).is_ok());
}

#[test]
fn input_test_keeps_trigger_axes_unipolar() {
    let trigger = visual_value(HostControlId::semantic_axis(JoystickAxis::LeftZ), 0.0);
    let stick = visual_value(HostControlId::semantic_axis(JoystickAxis::LeftStickX), 0.0);
    assert_eq!(input_test_progress(trigger), 0.0);
    assert_eq!(input_test_progress(stick), 0.5);
    assert_eq!(
        input_test_progress(visual_value(
            HostControlId::semantic_axis(JoystickAxis::RightZ),
            1.0,
        )),
        1.0
    );
}
