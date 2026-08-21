// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

#![forbid(unsafe_code)]

mod controller;
mod joystick;
mod keyboard;
pub use controller::{
    AxisCalibration, AxisSpan, AxisTransform, ControllerAxisBinding, ControllerButtonBinding,
    ControllerButtonState, ControllerConfig, ControllerDevice, ControllerDeviceMatcher,
    ControllerGamePortState, ControllerGuestDelta, ControllerKeyBinding, ControllerKeyTransition,
    ControllerManager, ControllerMapper, DigitalDirection, GravisHandedness, GravisMode,
    GuestControllerProfile, HostControlId, HostControlKind, HostControlValue, HostControllerBatch,
    HostDigitalBinding, HostSemanticControl, KeyboardControlSpec, keyboard_controls,
    resolve_control_value,
};
pub use joystick::{
    JoystickAxis, JoystickAxisBinding, JoystickBinding, JoystickButton, JoystickPolarity,
};
pub use keyboard::{
    GuestKey, GuestKeyChord, GuestKeyRouter, GuestKeySource, GuestKeyTransition, HostKeyboard,
};
