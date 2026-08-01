// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

#![forbid(unsafe_code)]

mod joystick;
mod keyboard;
pub use joystick::{
    GamepadManager, JoystickAxis, JoystickAxisBinding, JoystickBinding, JoystickButton,
    JoystickPolarity, JoystickSample, JoystickWizard, JoystickWizardStep,
};
pub use keyboard::HostKeyboard;
