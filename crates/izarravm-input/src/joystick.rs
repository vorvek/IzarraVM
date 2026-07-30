// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use gilrs::{Axis, Button, EventType, Gilrs};
use serde::{Deserialize, Serialize};

const NEUTRAL_LIMIT: f32 = 0.20;
const MOVEMENT_LIMIT: f32 = 0.75;
const RUNTIME_DEADZONE: f32 = 0.15;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    const CENTERED_CONTROLS: [Self; 6] = [
        Self::LeftStickX,
        Self::LeftStickY,
        Self::RightStickX,
        Self::RightStickY,
        Self::DPadX,
        Self::DPadY,
    ];

    fn from_gilrs(axis: Axis) -> Option<Self> {
        match axis {
            Axis::LeftStickX => Some(Self::LeftStickX),
            Axis::LeftStickY => Some(Self::LeftStickY),
            Axis::LeftZ => Some(Self::LeftZ),
            Axis::RightStickX => Some(Self::RightStickX),
            Axis::RightStickY => Some(Self::RightStickY),
            Axis::RightZ => Some(Self::RightZ),
            Axis::DPadX => Some(Self::DPadX),
            Axis::DPadY => Some(Self::DPadY),
            _ => None,
        }
    }

    fn gilrs(self) -> Axis {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JoystickPolarity {
    Positive,
    Negative,
}

impl JoystickPolarity {
    fn apply(self, value: f32) -> f32 {
        match self {
            Self::Positive => value,
            Self::Negative => -value,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoystickAxisBinding {
    pub control: JoystickAxis,
    pub polarity: JoystickPolarity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    fn from_gilrs(button: Button) -> Option<Self> {
        match button {
            Button::South => Some(Self::South),
            Button::East => Some(Self::East),
            Button::North => Some(Self::North),
            Button::West => Some(Self::West),
            Button::C => Some(Self::C),
            Button::Z => Some(Self::Z),
            Button::LeftTrigger => Some(Self::LeftTrigger),
            Button::LeftTrigger2 => Some(Self::LeftTrigger2),
            Button::RightTrigger => Some(Self::RightTrigger),
            Button::RightTrigger2 => Some(Self::RightTrigger2),
            Button::Select => Some(Self::Select),
            Button::Start => Some(Self::Start),
            Button::Mode => Some(Self::Mode),
            Button::LeftThumb => Some(Self::LeftThumb),
            Button::RightThumb => Some(Self::RightThumb),
            Button::DPadUp => Some(Self::DPadUp),
            Button::DPadDown => Some(Self::DPadDown),
            Button::DPadLeft => Some(Self::DPadLeft),
            Button::DPadRight => Some(Self::DPadRight),
            _ => None,
        }
    }

    fn gilrs(self) -> Button {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoystickBinding {
    pub controller_uuid: String,
    pub controller_name: String,
    pub x: JoystickAxisBinding,
    pub y: JoystickAxisBinding,
    pub button_1: JoystickButton,
    pub button_2: JoystickButton,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JoystickSample {
    pub x: u8,
    pub y: u8,
    pub buttons: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoystickWizardStep {
    Center,
    XRight,
    Recenter,
    YDown,
    Button1,
    Button2,
    Complete,
}

impl JoystickWizardStep {
    pub fn instruction(self) -> &'static str {
        match self {
            Self::Center => "Center the stick.",
            Self::XRight => "Move the X axis fully right.",
            Self::Recenter => "Return the stick to center.",
            Self::YDown => "Move the Y axis fully down.",
            Self::Button1 => "Press Button 1.",
            Self::Button2 => "Press Button 2.",
            Self::Complete => "Joystick controls captured.",
        }
    }
}

#[derive(Debug, Clone)]
pub struct JoystickWizard {
    step: JoystickWizardStep,
    controller_uuid: Option<String>,
    controller_name: Option<String>,
    x: Option<JoystickAxisBinding>,
    y: Option<JoystickAxisBinding>,
    button_1: Option<JoystickButton>,
    button_2: Option<JoystickButton>,
    error: Option<&'static str>,
}

impl Default for JoystickWizard {
    fn default() -> Self {
        Self {
            step: JoystickWizardStep::Center,
            controller_uuid: None,
            controller_name: None,
            x: None,
            y: None,
            button_1: None,
            button_2: None,
            error: None,
        }
    }
}

impl JoystickWizard {
    pub fn step(&self) -> JoystickWizardStep {
        self.step
    }

    pub fn error(&self) -> Option<&'static str> {
        self.error
    }

    pub fn binding(&self) -> Option<JoystickBinding> {
        (self.step == JoystickWizardStep::Complete).then(|| JoystickBinding {
            controller_uuid: self.controller_uuid.clone().expect("complete UUID"),
            controller_name: self.controller_name.clone().expect("complete name"),
            x: self.x.expect("complete X axis"),
            y: self.y.expect("complete Y axis"),
            button_1: self.button_1.expect("complete button 1"),
            button_2: self.button_2.expect("complete button 2"),
        })
    }

    fn accepts_controller(&self, uuid: &str) -> bool {
        self.controller_uuid
            .as_deref()
            .is_none_or(|bound| bound == uuid)
    }

    fn accept(&mut self, uuid: String, name: String, event: WizardEvent) {
        if !self.accepts_controller(&uuid) {
            return;
        }
        self.error = None;
        match (self.step, event) {
            (JoystickWizardStep::Center, WizardEvent::Centered) => {
                self.controller_uuid = Some(uuid);
                self.controller_name = Some(name);
                self.step = JoystickWizardStep::XRight;
            }
            (JoystickWizardStep::XRight, WizardEvent::Axis(control, value))
                if value.abs() >= MOVEMENT_LIMIT =>
            {
                self.x = Some(axis_binding(control, value));
                self.step = JoystickWizardStep::Recenter;
            }
            (JoystickWizardStep::Recenter, WizardEvent::Centered) => {
                self.step = JoystickWizardStep::YDown;
            }
            (JoystickWizardStep::YDown, WizardEvent::Axis(control, value))
                if value.abs() >= MOVEMENT_LIMIT =>
            {
                if self.x.is_some_and(|x| x.control == control) {
                    self.error = Some("Choose a different axis for Y.");
                } else {
                    self.y = Some(axis_binding(control, value));
                    self.step = JoystickWizardStep::Button1;
                }
            }
            (JoystickWizardStep::Button1, WizardEvent::Button(control)) => {
                self.button_1 = Some(control);
                self.step = JoystickWizardStep::Button2;
            }
            (JoystickWizardStep::Button2, WizardEvent::Button(control)) => {
                if self.button_1 == Some(control) {
                    self.error = Some("Choose a different control for Button 2.");
                } else {
                    self.button_2 = Some(control);
                    self.step = JoystickWizardStep::Complete;
                }
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum WizardEvent {
    Centered,
    Axis(JoystickAxis, f32),
    Button(JoystickButton),
}

fn axis_binding(control: JoystickAxis, value: f32) -> JoystickAxisBinding {
    JoystickAxisBinding {
        control,
        polarity: if value >= 0.0 {
            JoystickPolarity::Positive
        } else {
            JoystickPolarity::Negative
        },
    }
}

pub struct GamepadManager {
    gilrs: Gilrs,
}

impl GamepadManager {
    pub fn new() -> Result<Self, String> {
        Gilrs::new()
            .map(|gilrs| Self { gilrs })
            .map_err(|err| err.to_string())
    }

    pub fn poll_wizard(&mut self, mut wizard: Option<&mut JoystickWizard>) {
        while let Some(event) = self.gilrs.next_event() {
            let gamepad = self.gilrs.gamepad(event.id);
            let uuid = format_uuid(gamepad.uuid());
            let name = gamepad.name().to_owned();
            let event = match event.event {
                EventType::AxisChanged(axis, value, _) => {
                    JoystickAxis::from_gilrs(axis).map(|axis| WizardEvent::Axis(axis, value))
                }
                EventType::ButtonPressed(button, _) => {
                    JoystickButton::from_gilrs(button).map(WizardEvent::Button)
                }
                _ => None,
            };
            if let (Some(wizard), Some(event)) = (wizard.as_deref_mut(), event) {
                wizard.accept(uuid, name, event);
            }
        }

        let Some(wizard) = wizard else {
            return;
        };
        if !matches!(
            wizard.step(),
            JoystickWizardStep::Center | JoystickWizardStep::Recenter
        ) {
            return;
        }
        for (_, gamepad) in self.gilrs.gamepads() {
            let mut axes = JoystickAxis::CENTERED_CONTROLS
                .into_iter()
                .filter_map(|axis| {
                    gamepad
                        .axis_code(axis.gilrs())
                        .map(|_| gamepad.value(axis.gilrs()))
                });
            if axes_centered(&mut axes) {
                wizard.accept(
                    format_uuid(gamepad.uuid()),
                    gamepad.name().to_owned(),
                    WizardEvent::Centered,
                );
                break;
            }
        }
    }

    pub fn sample(&self, binding: &JoystickBinding) -> Option<JoystickSample> {
        let (_, gamepad) = self
            .gilrs
            .gamepads()
            .find(|(_, gamepad)| format_uuid(gamepad.uuid()) == binding.controller_uuid)?;
        gamepad.axis_code(binding.x.control.gilrs())?;
        gamepad.axis_code(binding.y.control.gilrs())?;
        gamepad.button_code(binding.button_1.gilrs())?;
        gamepad.button_code(binding.button_2.gilrs())?;
        let x = gamepad.value(binding.x.control.gilrs());
        let y = gamepad.value(binding.y.control.gilrs());
        let button_1 = gamepad.is_pressed(binding.button_1.gilrs());
        let button_2 = gamepad.is_pressed(binding.button_2.gilrs());
        Some(JoystickSample {
            x: quantize_axis(binding.x.polarity.apply(x)),
            y: quantize_axis(binding.y.polarity.apply(y)),
            buttons: u8::from(button_1) | (u8::from(button_2) << 1),
        })
    }
}

fn axes_centered(axes: &mut impl Iterator<Item = f32>) -> bool {
    axes.next().is_some_and(|first| {
        first.abs() <= NEUTRAL_LIMIT && axes.all(|value| value.abs() <= NEUTRAL_LIMIT)
    })
}

fn quantize_axis(value: f32) -> u8 {
    let value = value.clamp(-1.0, 1.0);
    let scaled = if value.abs() <= RUNTIME_DEADZONE {
        0.0
    } else {
        value.signum() * (value.abs() - RUNTIME_DEADZONE) / (1.0 - RUNTIME_DEADZONE)
    };
    ((scaled + 1.0) * 127.5).round() as u8
}

fn format_uuid(bytes: [u8; 16]) -> String {
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

#[cfg(test)]
#[path = "joystick_test.rs"]
mod tests;
