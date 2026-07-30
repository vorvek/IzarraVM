// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn accept(wizard: &mut JoystickWizard, event: WizardEvent) {
    wizard.accept("controller-a".into(), "Test Pad".into(), event);
}

#[test]
fn wizard_captures_sequence_and_polarity() {
    let mut wizard = JoystickWizard::default();
    accept(&mut wizard, WizardEvent::Centered);
    accept(
        &mut wizard,
        WizardEvent::Axis(JoystickAxis::LeftStickX, -0.8),
    );
    accept(&mut wizard, WizardEvent::Centered);
    accept(
        &mut wizard,
        WizardEvent::Axis(JoystickAxis::LeftStickY, 0.9),
    );
    accept(&mut wizard, WizardEvent::Button(JoystickButton::South));
    accept(&mut wizard, WizardEvent::Button(JoystickButton::East));

    let binding = wizard.binding().unwrap();
    assert_eq!(binding.controller_uuid, "controller-a");
    assert_eq!(binding.controller_name, "Test Pad");
    assert_eq!(binding.x.control, JoystickAxis::LeftStickX);
    assert_eq!(binding.x.polarity, JoystickPolarity::Negative);
    assert_eq!(binding.y.control, JoystickAxis::LeftStickY);
    assert_eq!(binding.y.polarity, JoystickPolarity::Positive);
    assert_eq!(binding.button_1, JoystickButton::South);
    assert_eq!(binding.button_2, JoystickButton::East);
}

#[test]
fn wizard_rejects_duplicate_axes_and_buttons() {
    let mut wizard = JoystickWizard::default();
    accept(&mut wizard, WizardEvent::Centered);
    accept(
        &mut wizard,
        WizardEvent::Axis(JoystickAxis::LeftStickX, 1.0),
    );
    accept(&mut wizard, WizardEvent::Centered);
    accept(
        &mut wizard,
        WizardEvent::Axis(JoystickAxis::LeftStickX, -1.0),
    );
    assert_eq!(wizard.step(), JoystickWizardStep::YDown);
    assert!(wizard.error().unwrap().contains("different axis"));
    accept(
        &mut wizard,
        WizardEvent::Axis(JoystickAxis::LeftStickY, 1.0),
    );
    accept(&mut wizard, WizardEvent::Button(JoystickButton::South));
    accept(&mut wizard, WizardEvent::Button(JoystickButton::South));
    assert_eq!(wizard.step(), JoystickWizardStep::Button2);
    assert!(wizard.error().unwrap().contains("different control"));
}

#[test]
fn wizard_enforces_neutral_and_movement_thresholds() {
    assert!(axes_centered(&mut [0.20, -0.20].into_iter()));
    assert!(!axes_centered(&mut [0.21, 0.0].into_iter()));
    assert!(!axes_centered(&mut [].into_iter()));

    let mut wizard = JoystickWizard::default();
    accept(&mut wizard, WizardEvent::Centered);
    accept(
        &mut wizard,
        WizardEvent::Axis(JoystickAxis::LeftStickX, 0.74),
    );
    assert_eq!(wizard.step(), JoystickWizardStep::XRight);
    accept(
        &mut wizard,
        WizardEvent::Axis(JoystickAxis::LeftStickX, 0.75),
    );
    assert_eq!(wizard.step(), JoystickWizardStep::Recenter);
}

#[test]
fn wizard_ignores_other_controllers_after_center_capture() {
    let mut wizard = JoystickWizard::default();
    accept(&mut wizard, WizardEvent::Centered);
    wizard.accept(
        "controller-b".into(),
        "Other Pad".into(),
        WizardEvent::Axis(JoystickAxis::RightStickX, 1.0),
    );
    assert_eq!(wizard.step(), JoystickWizardStep::XRight);
}

#[test]
fn runtime_deadzone_is_rescaled_and_quantized() {
    assert_eq!(quantize_axis(-1.0), 0);
    assert_eq!(quantize_axis(-RUNTIME_DEADZONE), 128);
    assert_eq!(quantize_axis(0.0), 128);
    assert_eq!(quantize_axis(RUNTIME_DEADZONE), 128);
    assert_eq!(quantize_axis(1.0), 255);
    assert!(quantize_axis(0.575) > 190);
}

#[test]
fn uuid_format_is_stable() {
    assert_eq!(
        format_uuid([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]),
        "00010203-0405-0607-0809-0a0b0c0d0e0f"
    );
}
