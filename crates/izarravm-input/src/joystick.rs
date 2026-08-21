// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use gilrs::{Axis, Button};
use serde::{Deserialize, Serialize};

/// Semantic axes used by the retired two-axis preference format and as a
/// portable fallback when a backend-specific raw control code changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoystickAxis {
    LeftStickX,
    LeftStickY,
    LeftZ,
    RightStickX,
    RightStickY,
    RightZ,
    DPadX,
    DPadY,
}

impl JoystickAxis {
    pub(crate) fn gilrs(self) -> Axis {
        match self {
            Self::LeftStickX => Axis::LeftStickX,
            Self::LeftStickY => Axis::LeftStickY,
            Self::LeftZ => Axis::LeftZ,
            Self::RightStickX => Axis::RightStickX,
            Self::RightStickY => Axis::RightStickY,
            Self::RightZ => Axis::RightZ,
            Self::DPadX => Axis::DPadX,
            Self::DPadY => Axis::DPadY,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JoystickPolarity {
    Positive,
    Negative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoystickAxisBinding {
    pub control: JoystickAxis,
    pub polarity: JoystickPolarity,
}

/// Semantic buttons retained for legacy preference migration and portable
/// fallback matching in the current controller model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoystickButton {
    South,
    East,
    North,
    West,
    C,
    Z,
    LeftTrigger,
    LeftTrigger2,
    RightTrigger,
    RightTrigger2,
    Select,
    Start,
    Mode,
    LeftThumb,
    RightThumb,
    DPadUp,
    DPadDown,
    DPadLeft,
    DPadRight,
}

impl JoystickButton {
    pub(crate) fn gilrs(self) -> Button {
        match self {
            Self::South => Button::South,
            Self::East => Button::East,
            Self::North => Button::North,
            Self::West => Button::West,
            Self::C => Button::C,
            Self::Z => Button::Z,
            Self::LeftTrigger => Button::LeftTrigger,
            Self::LeftTrigger2 => Button::LeftTrigger2,
            Self::RightTrigger => Button::RightTrigger,
            Self::RightTrigger2 => Button::RightTrigger2,
            Self::Select => Button::Select,
            Self::Start => Button::Start,
            Self::Mode => Button::Mode,
            Self::LeftThumb => Button::LeftThumb,
            Self::RightThumb => Button::RightThumb,
            Self::DPadUp => Button::DPadUp,
            Self::DPadDown => Button::DPadDown,
            Self::DPadLeft => Button::DPadLeft,
            Self::DPadRight => Button::DPadRight,
        }
    }
}

/// Retired preference payload. It is deserialize-only in the GUI and migrates
/// into `ControllerConfig` before the next save.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoystickBinding {
    pub controller_uuid: String,
    pub controller_name: String,
    pub x: JoystickAxisBinding,
    pub y: JoystickAxisBinding,
    pub button_1: JoystickButton,
    pub button_2: JoystickButton,
}
