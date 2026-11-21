// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

// Denied, not forbidden: the Windows XInput loader makes one audited FFI call.
#![deny(unsafe_code)]

mod controller;
#[cfg(windows)]
mod controller_windows;
mod joystick;
mod keyboard;
#[cfg(windows)]
mod xinput;
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
