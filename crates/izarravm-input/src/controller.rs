// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use gilrs::{EventType, Gamepad, Gilrs, GilrsBuilder, ev::Code};
use serde::{Deserialize, Serialize};
use winit::keyboard::KeyCode;

use crate::{
    GuestKey, GuestKeyChord, GuestKeyTransition, JoystickAxis, JoystickAxisBinding,
    JoystickBinding, JoystickButton, JoystickPolarity,
};

const DIGITAL_PRESS_THRESHOLD: f32 = 0.65;
const DIGITAL_RELEASE_THRESHOLD: f32 = 0.50;
const RECONNECT_NEUTRAL_TOLERANCE: f32 = 0.20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostControlKind {
    Axis,
    Button,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "control", rename_all = "snake_case")]
pub enum HostSemanticControl {
    Axis(JoystickAxis),
    Button(JoystickButton),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HostControlId {
    pub kind: HostControlKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_code: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic: Option<HostSemanticControl>,
}

impl HostControlId {
    pub const fn semantic_axis(axis: JoystickAxis) -> Self {
        Self {
            kind: HostControlKind::Axis,
            raw_code: None,
            semantic: Some(HostSemanticControl::Axis(axis)),
        }
    }

    pub const fn semantic_button(button: JoystickButton) -> Self {
        Self {
            kind: HostControlKind::Button,
            raw_code: None,
            semantic: Some(HostSemanticControl::Button(button)),
        }
    }

    pub fn matches(self, other: Self) -> bool {
        self.same_control(other)
    }

    fn same_control(self, other: Self) -> bool {
        if self.kind != other.kind {
            return false;
        }
        if let (Some(expected), Some(actual)) = (self.raw_code, other.raw_code) {
            return expected == actual;
        }
        self.semantic
            .is_some_and(|semantic| other.semantic == Some(semantic))
    }

    pub fn display(self) -> String {
        match self.semantic {
            Some(HostSemanticControl::Axis(axis)) => format!("{axis:?}"),
            Some(HostSemanticControl::Button(button)) => format!("{button:?}"),
            None => match self.raw_code {
                Some(raw_code) => format!(
                    "Raw {} {raw_code}",
                    match self.kind {
                        HostControlKind::Axis => "axis",
                        HostControlKind::Button => "button",
                    }
                ),
                None => "Unidentified control".to_owned(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerDeviceMatcher {
    pub backend: String,
    pub platform: String,
    pub guid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_id: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<u16>,
    pub name: String,
    #[serde(default)]
    pub occurrence: u16,
}

impl ControllerDeviceMatcher {
    pub fn matches(&self, actual: &Self) -> bool {
        device_matches(self, actual)
    }

    pub fn strongly_matches(&self, actual: &Self) -> bool {
        if !self.matches(actual) {
            return false;
        }
        let usb_matches = matches!(
            (
                self.vendor_id,
                self.product_id,
                actual.vendor_id,
                actual.product_id,
            ),
            (Some(vendor), Some(product), Some(other_vendor), Some(other_product))
                if vendor == other_vendor && product == other_product
        );
        let guid_matches = meaningful_guid(&self.guid)
            && meaningful_guid(&actual.guid)
            && self.guid.eq_ignore_ascii_case(&actual.guid);
        usb_matches || guid_matches
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GravisMode {
    #[default]
    FourButton,
    TwoButtonTurbo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GravisHandedness {
    #[default]
    RightHanded,
    LeftHanded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GuestControllerProfile {
    KeyboardOnly,
    #[default]
    Standard,
    Gravis {
        #[serde(default)]
        mode: GravisMode,
        #[serde(default)]
        handedness: GravisHandedness,
    },
    WheelPedals,
}

impl GuestControllerProfile {
    pub const fn axis_present(self) -> u8 {
        match self {
            Self::KeyboardOnly => 0,
            Self::Standard | Self::Gravis { .. } => 0x03,
            Self::WheelPedals => 0x0f,
        }
    }

    pub const fn connects_gameport(self) -> bool {
        !matches!(self, Self::KeyboardOnly)
    }

    pub const fn button_count(self) -> usize {
        match self {
            Self::KeyboardOnly => 0,
            Self::Standard => 2,
            Self::Gravis { .. } | Self::WheelPedals => 4,
        }
    }

    pub const fn default_axes(self) -> [u8; 4] {
        match self {
            Self::KeyboardOnly => [0; 4],
            Self::Standard | Self::Gravis { .. } => [128, 128, 0, 0],
            Self::WheelPedals => [128, 0, 0, 0],
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::KeyboardOnly => "Keyboard only",
            Self::Standard => "Standard joystick",
            Self::Gravis { .. } => "4 button gamepad",
            Self::WheelPedals => "Wheel and pedals",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AxisSpan {
    #[default]
    Full,
    PositiveHalf,
    NegativeHalf,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AxisCalibration {
    pub minimum: f32,
    pub center: f32,
    pub maximum: f32,
    pub deadzone: f32,
    pub saturation: f32,
}

impl Default for AxisCalibration {
    fn default() -> Self {
        Self {
            minimum: -1.0,
            center: 0.0,
            maximum: 1.0,
            deadzone: 0.15,
            saturation: 1.0,
        }
    }
}

impl AxisCalibration {
    fn normalize(self, raw: f32) -> f32 {
        let center = self.center.clamp(-1.0, 1.0);
        let minimum = self.minimum.clamp(-1.0, center);
        let maximum = self.maximum.clamp(center, 1.0);
        let raw = raw.clamp(minimum, maximum);
        let centered = if raw >= center {
            (raw - center) / (maximum - center).max(f32::EPSILON)
        } else {
            -(center - raw) / (center - minimum).max(f32::EPSILON)
        };
        let deadzone = self.deadzone.clamp(0.0, 0.95);
        let magnitude = centered.abs();
        let without_deadzone = if magnitude <= deadzone {
            0.0
        } else {
            (magnitude - deadzone) / (1.0 - deadzone)
        };
        let saturation = self.saturation.clamp(0.05, 1.0);
        centered.signum() * (without_deadzone / saturation).min(1.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AxisTransform {
    #[serde(default)]
    pub span: AxisSpan,
    #[serde(default)]
    pub inverted: bool,
    #[serde(default)]
    pub calibration: AxisCalibration,
}

impl Default for AxisTransform {
    fn default() -> Self {
        Self {
            span: AxisSpan::Full,
            inverted: false,
            calibration: AxisCalibration::default(),
        }
    }
}

impl AxisTransform {
    pub fn apply(self, raw: f32) -> u8 {
        let value = self.calibration.normalize(raw);
        let mut unit = match self.span {
            AxisSpan::Full => (value + 1.0) * 0.5,
            AxisSpan::PositiveHalf => value.clamp(0.0, 1.0),
            AxisSpan::NegativeHalf => (-value).clamp(0.0, 1.0),
        };
        if self.inverted {
            unit = 1.0 - unit;
        }
        (unit.clamp(0.0, 1.0) * 255.0).round() as u8
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ControllerAxisBinding {
    pub host: HostControlId,
    #[serde(default)]
    pub transform: AxisTransform,
}

impl Default for ControllerAxisBinding {
    fn default() -> Self {
        Self {
            host: HostControlId::semantic_axis(JoystickAxis::LeftStickX),
            transform: AxisTransform::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DigitalDirection {
    #[default]
    Positive,
    Negative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostDigitalBinding {
    pub host: HostControlId,
    #[serde(default)]
    pub direction: DigitalDirection,
}

impl HostDigitalBinding {
    fn directed_value(self, value: f32) -> f32 {
        match self.direction {
            DigitalDirection::Positive => value,
            DigitalDirection::Negative => -value,
        }
    }

    fn update(self, value: f32, held: bool) -> bool {
        let value = self.directed_value(value);
        if held {
            value > DIGITAL_RELEASE_THRESHOLD
        } else {
            value >= DIGITAL_PRESS_THRESHOLD
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyboardControlSpec {
    pub label: &'static str,
    pub host: HostDigitalBinding,
    default_key: Option<KeyCode>,
}

impl KeyboardControlSpec {
    fn binding(self) -> ControllerKeyBinding {
        ControllerKeyBinding {
            host: self.host,
            guest: self
                .default_key
                .and_then(GuestKey::from_key_code)
                .map(GuestKeyChord::from)
                .unwrap_or_default(),
        }
    }
}

const fn keyboard_axis(
    label: &'static str,
    axis: JoystickAxis,
    direction: DigitalDirection,
    default_key: Option<KeyCode>,
) -> KeyboardControlSpec {
    KeyboardControlSpec {
        label,
        host: HostDigitalBinding {
            host: HostControlId::semantic_axis(axis),
            direction,
        },
        default_key,
    }
}

const fn keyboard_button(
    label: &'static str,
    button: JoystickButton,
    default_key: Option<KeyCode>,
) -> KeyboardControlSpec {
    KeyboardControlSpec {
        label,
        host: HostDigitalBinding {
            host: HostControlId::semantic_button(button),
            direction: DigitalDirection::Positive,
        },
        default_key,
    }
}

const KEYBOARD_CONTROLS: [KeyboardControlSpec; 24] = [
    keyboard_axis(
        "Left stick up",
        JoystickAxis::LeftStickY,
        DigitalDirection::Positive,
        Some(KeyCode::ArrowUp),
    ),
    keyboard_axis(
        "Left stick down",
        JoystickAxis::LeftStickY,
        DigitalDirection::Negative,
        Some(KeyCode::ArrowDown),
    ),
    keyboard_axis(
        "Left stick left",
        JoystickAxis::LeftStickX,
        DigitalDirection::Negative,
        Some(KeyCode::ArrowLeft),
    ),
    keyboard_axis(
        "Left stick right",
        JoystickAxis::LeftStickX,
        DigitalDirection::Positive,
        Some(KeyCode::ArrowRight),
    ),
    keyboard_axis(
        "Right stick up",
        JoystickAxis::RightStickY,
        DigitalDirection::Positive,
        None,
    ),
    keyboard_axis(
        "Right stick down",
        JoystickAxis::RightStickY,
        DigitalDirection::Negative,
        None,
    ),
    keyboard_axis(
        "Right stick left",
        JoystickAxis::RightStickX,
        DigitalDirection::Negative,
        None,
    ),
    keyboard_axis(
        "Right stick right",
        JoystickAxis::RightStickX,
        DigitalDirection::Positive,
        None,
    ),
    keyboard_button("D-pad up", JoystickButton::DPadUp, Some(KeyCode::ArrowUp)),
    keyboard_button(
        "D-pad down",
        JoystickButton::DPadDown,
        Some(KeyCode::ArrowDown),
    ),
    keyboard_button(
        "D-pad left",
        JoystickButton::DPadLeft,
        Some(KeyCode::ArrowLeft),
    ),
    keyboard_button(
        "D-pad right",
        JoystickButton::DPadRight,
        Some(KeyCode::ArrowRight),
    ),
    keyboard_button("Face A", JoystickButton::South, Some(KeyCode::ControlLeft)),
    keyboard_button("Face B", JoystickButton::East, Some(KeyCode::AltLeft)),
    keyboard_button("Face X", JoystickButton::West, Some(KeyCode::Space)),
    keyboard_button("Face Y", JoystickButton::North, Some(KeyCode::ShiftLeft)),
    keyboard_button("Left shoulder", JoystickButton::LeftTrigger, None),
    keyboard_button("Right shoulder", JoystickButton::RightTrigger, None),
    keyboard_axis(
        "Left trigger",
        JoystickAxis::LeftZ,
        DigitalDirection::Positive,
        None,
    ),
    keyboard_axis(
        "Right trigger",
        JoystickAxis::RightZ,
        DigitalDirection::Positive,
        None,
    ),
    keyboard_button("Select", JoystickButton::Select, None),
    keyboard_button("Start", JoystickButton::Start, None),
    keyboard_button("Left stick press", JoystickButton::LeftThumb, None),
    keyboard_button("Right stick press", JoystickButton::RightThumb, None),
];

pub fn keyboard_controls() -> &'static [KeyboardControlSpec] {
    &KEYBOARD_CONTROLS
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerButtonBinding {
    pub host: HostDigitalBinding,
    /// Profile-relative action A through D, encoded as 0 through 3.
    pub action: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerKeyBinding {
    pub host: HostDigitalBinding,
    pub guest: GuestKeyChord,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControllerConfig {
    pub device: ControllerDeviceMatcher,
    #[serde(default)]
    pub profile: GuestControllerProfile,
    #[serde(default = "default_axis_bindings")]
    pub axes: [ControllerAxisBinding; 4],
    #[serde(default)]
    pub buttons: Vec<ControllerButtonBinding>,
    #[serde(default)]
    pub keys: Vec<ControllerKeyBinding>,
}

impl ControllerConfig {
    pub fn default_keyboard(device: ControllerDeviceMatcher) -> Self {
        Self::for_profile(device, GuestControllerProfile::KeyboardOnly)
    }

    pub fn default_gravis(device: ControllerDeviceMatcher) -> Self {
        Self::for_profile(
            device,
            GuestControllerProfile::Gravis {
                mode: GravisMode::FourButton,
                handedness: GravisHandedness::RightHanded,
            },
        )
    }

    pub fn apply_profile_defaults(&mut self, profile: GuestControllerProfile) {
        let profile_changed = profile != self.profile;
        self.profile = profile;
        self.axes = profile_axis_bindings(profile);
        self.buttons = default_button_bindings(profile);
        if profile_changed {
            self.keys = if matches!(profile, GuestControllerProfile::KeyboardOnly) {
                default_keyboard_bindings()
            } else {
                Vec::new()
            };
        }
        self.normalize_profile_bindings();
    }

    pub fn normalize_profile_bindings(&mut self) {
        let button_count = self.profile.button_count();
        self.buttons
            .retain(|binding| usize::from(binding.action) < button_count);
        let defaults = default_button_bindings(self.profile);
        for default in defaults {
            if !self
                .buttons
                .iter()
                .any(|binding| binding.action == default.action)
            {
                self.buttons.push(default);
            }
        }
        if matches!(self.profile, GuestControllerProfile::KeyboardOnly) {
            for control in keyboard_controls() {
                if !self.keys.iter().any(|binding| binding.host == control.host) {
                    self.keys.push(control.binding());
                }
            }
        }
    }

    pub fn from_legacy(binding: JoystickBinding) -> Self {
        let device = ControllerDeviceMatcher {
            backend: backend_name().to_owned(),
            platform: std::env::consts::OS.to_owned(),
            guid: binding.controller_uuid,
            vendor_id: None,
            product_id: None,
            name: binding.controller_name,
            occurrence: 0,
        };
        let mut config = Self::for_profile(device, GuestControllerProfile::Standard);
        config.axes[0] = legacy_axis(binding.x);
        config.axes[1] = legacy_axis(binding.y);
        config.buttons = [binding.button_1, binding.button_2]
            .into_iter()
            .enumerate()
            .map(|(action, button)| ControllerButtonBinding {
                host: HostDigitalBinding {
                    host: HostControlId::semantic_button(button),
                    direction: DigitalDirection::Positive,
                },
                action: action as u8,
            })
            .collect();
        config
    }

    fn for_profile(device: ControllerDeviceMatcher, profile: GuestControllerProfile) -> Self {
        Self {
            device,
            profile,
            axes: profile_axis_bindings(profile),
            buttons: default_button_bindings(profile),
            keys: if matches!(profile, GuestControllerProfile::KeyboardOnly) {
                default_keyboard_bindings()
            } else {
                Vec::new()
            },
        }
    }
}

fn profile_axis_bindings(profile: GuestControllerProfile) -> [ControllerAxisBinding; 4] {
    let full = AxisTransform::default();
    let half = AxisTransform {
        span: AxisSpan::PositiveHalf,
        ..full
    };
    match profile {
        GuestControllerProfile::KeyboardOnly
        | GuestControllerProfile::Standard
        | GuestControllerProfile::Gravis { .. } => default_axis_bindings(),
        GuestControllerProfile::WheelPedals => [
            ControllerAxisBinding {
                host: HostControlId::semantic_axis(JoystickAxis::LeftStickX),
                transform: full,
            },
            ControllerAxisBinding {
                host: HostControlId::semantic_axis(JoystickAxis::LeftZ),
                transform: half,
            },
            ControllerAxisBinding {
                host: HostControlId::semantic_axis(JoystickAxis::RightZ),
                transform: half,
            },
            ControllerAxisBinding {
                host: HostControlId::semantic_axis(JoystickAxis::RightStickY),
                transform: half,
            },
        ],
    }
}

fn default_button_bindings(profile: GuestControllerProfile) -> Vec<ControllerButtonBinding> {
    [
        JoystickButton::South,
        JoystickButton::East,
        JoystickButton::West,
        JoystickButton::North,
    ]
    .into_iter()
    .take(profile.button_count())
    .enumerate()
    .map(|(action, button)| ControllerButtonBinding {
        host: HostDigitalBinding {
            host: HostControlId::semantic_button(button),
            direction: DigitalDirection::Positive,
        },
        action: action as u8,
    })
    .collect()
}

fn default_keyboard_bindings() -> Vec<ControllerKeyBinding> {
    keyboard_controls()
        .iter()
        .copied()
        .map(KeyboardControlSpec::binding)
        .collect()
}

fn default_axis_bindings() -> [ControllerAxisBinding; 4] {
    [
        ControllerAxisBinding {
            host: HostControlId::semantic_axis(JoystickAxis::LeftStickX),
            transform: AxisTransform::default(),
        },
        ControllerAxisBinding {
            host: HostControlId::semantic_axis(JoystickAxis::LeftStickY),
            transform: AxisTransform::default(),
        },
        ControllerAxisBinding {
            host: HostControlId::semantic_axis(JoystickAxis::RightStickX),
            transform: AxisTransform::default(),
        },
        ControllerAxisBinding {
            host: HostControlId::semantic_axis(JoystickAxis::RightStickY),
            transform: AxisTransform::default(),
        },
    ]
}

fn legacy_axis(binding: JoystickAxisBinding) -> ControllerAxisBinding {
    ControllerAxisBinding {
        host: HostControlId::semantic_axis(binding.control),
        transform: AxisTransform {
            inverted: binding.polarity == JoystickPolarity::Negative,
            ..AxisTransform::default()
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HostControlValue {
    pub control: HostControlId,
    pub value: f32,
}

pub fn resolve_control_value(values: &[HostControlValue], requested: HostControlId) -> Option<f32> {
    let exact = values.iter().find(|value| {
        let actual = value.control;
        requested.kind == actual.kind
            && requested.raw_code.is_some()
            && requested.raw_code == actual.raw_code
            && match (requested.semantic, actual.semantic) {
                (Some(expected), Some(found)) => expected == found,
                _ => true,
            }
    });
    if let Some(value) = exact {
        return Some(value.value);
    }

    let semantic = requested.semantic.and_then(|semantic| {
        values.iter().find(|value| {
            value.control.kind == requested.kind && value.control.semantic == Some(semantic)
        })
    });
    if let Some(value) = semantic {
        return Some(value.value);
    }

    if requested.raw_code.is_some()
        && let Some(value) = values.iter().find(|value| {
            value.control.kind == requested.kind && value.control.raw_code == requested.raw_code
        })
    {
        return Some(value.value);
    }

    let alias = requested.semantic.and_then(trigger_alias)?;
    values
        .iter()
        .find(|value| value.control.semantic == Some(alias))
        .map(|value| value.value)
}

fn trigger_alias(semantic: HostSemanticControl) -> Option<HostSemanticControl> {
    match semantic {
        HostSemanticControl::Axis(JoystickAxis::LeftZ) => {
            Some(HostSemanticControl::Button(JoystickButton::LeftTrigger2))
        }
        HostSemanticControl::Button(JoystickButton::LeftTrigger2) => {
            Some(HostSemanticControl::Axis(JoystickAxis::LeftZ))
        }
        HostSemanticControl::Axis(JoystickAxis::RightZ) => {
            Some(HostSemanticControl::Button(JoystickButton::RightTrigger2))
        }
        HostSemanticControl::Button(JoystickButton::RightTrigger2) => {
            Some(HostSemanticControl::Axis(JoystickAxis::RightZ))
        }
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HostControllerBatch {
    pub connected: bool,
    pub reset: bool,
    pub events: Vec<HostControlValue>,
    pub final_values: Vec<HostControlValue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ControllerButtonState {
    pub line: u8,
    pub normal_held: bool,
    pub turbo_held: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerGamePortState {
    pub axes: [u8; 4],
    pub axis_present: u8,
    pub normal_buttons: u8,
    pub turbo_buttons: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ControllerKeyTransition {
    pub source: u16,
    pub transition: GuestKeyTransition,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ControllerGuestDelta {
    pub keys: Vec<ControllerKeyTransition>,
    pub gameport: Option<ControllerGamePortState>,
    pub button_transitions: Vec<ControllerButtonState>,
    pub reset_gameport: bool,
}

#[derive(Debug, Clone)]
pub struct ControllerMapper {
    config: ControllerConfig,
    values: Vec<HostControlValue>,
    button_held: Vec<bool>,
    key_held: Vec<bool>,
    normal_buttons: u8,
    turbo_buttons: u8,
    connected: bool,
}

impl ControllerMapper {
    pub fn new(mut config: ControllerConfig) -> Self {
        config.normalize_profile_bindings();
        let button_count = config.buttons.len();
        let key_count = config.keys.len();
        Self {
            config,
            values: Vec::new(),
            button_held: vec![false; button_count],
            key_held: vec![false; key_count],
            normal_buttons: 0,
            turbo_buttons: 0,
            connected: false,
        }
    }

    pub fn apply(&mut self, batch: HostControllerBatch) -> ControllerGuestDelta {
        if !batch.connected {
            return self.disconnect();
        }

        let reset_gameport =
            self.config.profile.connects_gameport() && (batch.reset || !self.connected);
        let mut keys = Vec::new();
        if batch.reset {
            self.clear_state(&mut keys);
        }
        self.connected = true;
        let mut button_transitions = Vec::new();
        for event in batch.events {
            self.set_value(event);
            self.update_digital(&mut keys, &mut button_transitions);
        }
        for value in batch.final_values {
            self.set_value(value);
        }
        self.update_digital(&mut keys, &mut button_transitions);

        let mut axes = self.config.profile.default_axes();
        for (axis, binding) in self.config.axes.iter().enumerate() {
            if self.config.profile.axis_present() & (1 << axis) != 0
                && let Some(value) = self.value(binding.host)
            {
                axes[axis] = binding.transform.apply(value);
            }
        }
        let gameport = self
            .config
            .profile
            .connects_gameport()
            .then_some(ControllerGamePortState {
                axes,
                axis_present: self.config.profile.axis_present(),
                normal_buttons: self.normal_buttons,
                turbo_buttons: self.turbo_buttons,
            });
        ControllerGuestDelta {
            keys,
            gameport,
            button_transitions,
            reset_gameport,
        }
    }

    fn disconnect(&mut self) -> ControllerGuestDelta {
        let was_active = self.connected
            || self.normal_buttons != 0
            || self.turbo_buttons != 0
            || self.key_held.iter().any(|held| *held);
        let mut keys = Vec::new();
        self.clear_state(&mut keys);
        ControllerGuestDelta {
            keys,
            gameport: None,
            button_transitions: Vec::new(),
            reset_gameport: self.config.profile.connects_gameport() && was_active,
        }
    }

    fn clear_state(&mut self, keys: &mut Vec<ControllerKeyTransition>) {
        for (index, binding) in self.config.keys.iter().enumerate() {
            if self.key_held[index] {
                push_key_chord_transitions(keys, index, &binding.guest, false);
            }
        }
        self.values.clear();
        self.button_held.fill(false);
        self.key_held.fill(false);
        self.normal_buttons = 0;
        self.turbo_buttons = 0;
        self.connected = false;
    }

    fn set_value(&mut self, value: HostControlValue) {
        if let Some(slot) = self
            .values
            .iter_mut()
            .find(|slot| slot.control.same_control(value.control))
        {
            *slot = value;
        } else {
            self.values.push(value);
        }
    }

    fn value(&self, control: HostControlId) -> Option<f32> {
        resolve_control_value(&self.values, control)
    }

    fn update_digital(
        &mut self,
        keys: &mut Vec<ControllerKeyTransition>,
        transitions: &mut Vec<ControllerButtonState>,
    ) {
        for (index, binding) in self.config.buttons.iter().enumerate() {
            let value = self.value(binding.host.host).unwrap_or(0.0);
            self.button_held[index] = binding.host.update(value, self.button_held[index]);
        }
        for (index, binding) in self.config.keys.iter().enumerate() {
            let value = self.value(binding.host.host).unwrap_or(0.0);
            let held = binding.host.update(value, self.key_held[index]);
            if held != self.key_held[index] {
                self.key_held[index] = held;
                push_key_chord_transitions(keys, index, &binding.guest, held);
            }
        }

        let (normal, turbo) = self.aggregate_buttons();
        let changed = (self.normal_buttons ^ normal) | (self.turbo_buttons ^ turbo);
        for line in 0..4 {
            let bit = 1 << line;
            if changed & bit != 0 {
                transitions.push(ControllerButtonState {
                    line,
                    normal_held: normal & bit != 0,
                    turbo_held: turbo & bit != 0,
                });
            }
        }
        self.normal_buttons = normal;
        self.turbo_buttons = turbo;
    }

    fn aggregate_buttons(&self) -> (u8, u8) {
        let mut normal = 0;
        let mut turbo = 0;
        for (binding, held) in self.config.buttons.iter().zip(&self.button_held) {
            if !held || usize::from(binding.action) >= self.config.profile.button_count() {
                continue;
            }
            match self.config.profile {
                GuestControllerProfile::Gravis {
                    mode: GravisMode::TwoButtonTurbo,
                    ..
                } if binding.action >= 2 => turbo |= 1 << (binding.action - 2),
                _ => normal |= 1 << binding.action,
            }
        }
        (normal, turbo)
    }
}

fn push_key_chord_transitions(
    out: &mut Vec<ControllerKeyTransition>,
    source: usize,
    chord: &GuestKeyChord,
    pressed: bool,
) {
    let mut push = |key| {
        out.push(ControllerKeyTransition {
            source: source as u16,
            transition: GuestKeyTransition {
                key,
                pressed,
                repeat: false,
            },
        });
    };
    if pressed {
        for key in chord.keys().iter().copied() {
            push(key);
        }
    } else {
        for key in chord.keys().iter().rev().copied() {
            push(key);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerDevice {
    pub runtime_id: usize,
    pub matcher: ControllerDeviceMatcher,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ControllerBackendKind {
    Gilrs,
    #[cfg(windows)]
    XInput,
}

impl ControllerBackendKind {
    pub(super) fn for_matcher(matcher: &ControllerDeviceMatcher) -> Option<Self> {
        if matcher.backend == backend_name() {
            return Some(Self::Gilrs);
        }
        #[cfg(windows)]
        if matcher.backend == "xinput" {
            return Some(Self::XInput);
        }
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ControllerRuntimeKey {
    pub(super) backend: ControllerBackendKind,
    pub(super) runtime_id: usize,
}

#[derive(Default)]
struct ControllerSelection {
    matcher: Option<ControllerDeviceMatcher>,
}

impl ControllerSelection {
    fn update(&mut self, matcher: &ControllerDeviceMatcher) -> bool {
        if self
            .matcher
            .as_ref()
            .is_some_and(|current| current == matcher || current.strongly_matches(matcher))
        {
            self.matcher = Some(matcher.clone());
            return false;
        }
        self.matcher = Some(matcher.clone());
        true
    }
}

#[derive(Default)]
pub(super) struct BackendPoll {
    pub(super) events: Vec<HostControlValue>,
    pub(super) selected_disconnected: bool,
    pub(super) devices_changed: bool,
    pub(super) boundary_reset: bool,
}

pub(super) trait ControllerBackendDriver {
    fn devices(&self) -> &[ControllerDevice];
    fn topology_generation(&self) -> u64;
    fn focus_gained(&mut self, now: Instant);
    fn focus_lost(&mut self);
    fn poll(&mut self, selected: Option<ControllerRuntimeKey>, now: Instant) -> BackendPoll;
    fn final_values(&mut self, runtime: ControllerRuntimeKey) -> Vec<HostControlValue>;
}

pub(super) struct GilrsBackend {
    gilrs: Gilrs,
    values: BTreeMap<(ControllerRuntimeKey, HostControlKind, u32), HostControlValue>,
    devices: Vec<ControllerDevice>,
    topology_generation: u64,
}

impl GilrsBackend {
    #[cfg(not(windows))]
    pub(super) fn new() -> Result<Self, String> {
        Self::new_with_previous(&[])
    }

    pub(super) fn new_with_previous(previous: &[ControllerDevice]) -> Result<Self, String> {
        let gilrs = GilrsBuilder::new()
            .with_default_filters(false)
            .build()
            .map_err(|error| error.to_string())?;
        let mut backend = Self {
            gilrs,
            values: BTreeMap::new(),
            devices: previous.to_vec(),
            topology_generation: 0,
        };
        backend.refresh_devices(true);
        Ok(backend)
    }

    pub(super) fn refresh_devices(&mut self, force_generation: bool) {
        let previous = self.devices.clone();
        let mut devices = self
            .gilrs
            .gamepads()
            .filter(|(_, gamepad)| gamepad.is_connected())
            .map(|(id, gamepad)| ControllerDevice {
                runtime_id: usize::from(id),
                matcher: ControllerDeviceMatcher {
                    backend: backend_name().to_owned(),
                    platform: std::env::consts::OS.to_owned(),
                    guid: format_uuid(gamepad.uuid()),
                    vendor_id: gamepad.vendor_id(),
                    product_id: gamepad.product_id(),
                    name: gamepad.os_name().to_owned(),
                    occurrence: 0,
                },
            })
            .collect::<Vec<_>>();
        assign_device_occurrences(&previous, &mut devices);
        if force_generation || devices != previous {
            self.topology_generation = self.topology_generation.wrapping_add(1);
        }
        self.devices = devices;
    }

    pub(super) fn poll_events(&mut self, selected: Option<ControllerRuntimeKey>) -> BackendPoll {
        let selected = selected.filter(|key| key.backend == ControllerBackendKind::Gilrs);
        let mut poll = BackendPoll::default();
        while let Some(event) = self.gilrs.next_event() {
            let runtime = usize::from(event.id);
            let key = ControllerRuntimeKey {
                backend: ControllerBackendKind::Gilrs,
                runtime_id: runtime,
            };
            poll.devices_changed |=
                matches!(event.event, EventType::Connected | EventType::Disconnected);
            if selected.is_some()
                && let Some(value) = event_value(&self.gilrs.gamepad(event.id), event.event)
            {
                if let Some(raw_code) = value.control.raw_code {
                    self.values
                        .insert((key, value.control.kind, raw_code), value);
                }
                if selected == Some(key) {
                    poll.events.push(value);
                }
            }
            if matches!(event.event, EventType::Disconnected) {
                self.values.retain(|(cached, _, _), _| *cached != key);
                poll.selected_disconnected |= selected == Some(key);
            }
        }

        if poll.devices_changed {
            self.refresh_devices(true);
        }
        poll
    }

    pub(super) fn final_values(&mut self, runtime: ControllerRuntimeKey) -> Vec<HostControlValue> {
        self.refresh_values();
        self.values
            .iter()
            .filter(|((key, _, _), _)| *key == runtime)
            .map(|(_, value)| *value)
            .collect()
    }

    fn refresh_values(&mut self) {
        for (id, gamepad) in self
            .gilrs
            .gamepads()
            .filter(|(_, gamepad)| gamepad.is_connected())
        {
            let runtime = usize::from(id);
            let key = ControllerRuntimeKey {
                backend: ControllerBackendKind::Gilrs,
                runtime_id: runtime,
            };
            for (code, data) in gamepad.state().axes() {
                let control = control_id(&gamepad, HostControlKind::Axis, code);
                self.values.insert(
                    (key, HostControlKind::Axis, code.into_u32()),
                    HostControlValue {
                        control,
                        value: data.value(),
                    },
                );
            }
            for (code, data) in gamepad.state().buttons() {
                let control = control_id(&gamepad, HostControlKind::Button, code);
                self.values.insert(
                    (key, HostControlKind::Button, code.into_u32()),
                    HostControlValue {
                        control,
                        value: data.value(),
                    },
                );
            }
        }
    }

    pub(super) fn devices(&self) -> &[ControllerDevice] {
        &self.devices
    }

    #[cfg(not(windows))]
    pub(super) fn topology_generation(&self) -> u64 {
        self.topology_generation
    }
}

#[cfg(not(windows))]
impl ControllerBackendDriver for GilrsBackend {
    fn devices(&self) -> &[ControllerDevice] {
        self.devices()
    }

    fn topology_generation(&self) -> u64 {
        self.topology_generation()
    }

    fn focus_gained(&mut self, _now: Instant) {}

    fn focus_lost(&mut self) {}

    fn poll(&mut self, selected: Option<ControllerRuntimeKey>, _now: Instant) -> BackendPoll {
        self.poll_events(selected)
    }

    fn final_values(&mut self, runtime: ControllerRuntimeKey) -> Vec<HostControlValue> {
        self.final_values(runtime)
    }
}

pub struct ControllerManager {
    backend: Box<dyn ControllerBackendDriver>,
    selection: ControllerSelection,
    connection: ControllerConnectionLatch,
    focused: bool,
}

impl ControllerManager {
    pub fn new() -> Result<Self, String> {
        #[cfg(windows)]
        let backend: Box<dyn ControllerBackendDriver> =
            Box::new(crate::controller_windows::WindowsControllerBackend::new());
        #[cfg(not(windows))]
        let backend: Box<dyn ControllerBackendDriver> = Box::new(GilrsBackend::new()?);
        Ok(Self {
            backend,
            selection: ControllerSelection::default(),
            connection: ControllerConnectionLatch::default(),
            focused: false,
        })
    }

    pub fn devices(&self) -> &[ControllerDevice] {
        self.backend.devices()
    }

    pub fn topology_generation(&self) -> u64 {
        self.backend.topology_generation()
    }

    pub fn focus_gained(&mut self) {
        if self.focused {
            return;
        }
        self.focused = true;
        self.backend.focus_gained(Instant::now());
    }

    pub fn maintain(&mut self) {
        if self.focused {
            let _ = self.backend.poll(None, Instant::now());
        }
    }

    pub fn focus_lost(&mut self) -> HostControllerBatch {
        if !self.focused {
            return HostControllerBatch {
                connected: false,
                reset: false,
                events: Vec::new(),
                final_values: Vec::new(),
            };
        }
        self.focused = false;
        self.backend.focus_lost();
        self.connection.update(None, true, false);
        HostControllerBatch {
            connected: false,
            reset: true,
            events: Vec::new(),
            final_values: Vec::new(),
        }
    }

    pub fn poll(&mut self, config: &ControllerConfig) -> HostControllerBatch {
        if !self.focused {
            return HostControllerBatch {
                connected: false,
                reset: false,
                events: Vec::new(),
                final_values: Vec::new(),
            };
        }
        let selection_changed = self.selection.update(&config.device);
        let selected_before = self.matching_runtime(&config.device);
        let poll = self.backend.poll(selected_before, Instant::now());
        if selection_changed || poll.boundary_reset {
            self.connection.update(None, true, false);
            return HostControllerBatch {
                connected: false,
                reset: true,
                events: Vec::new(),
                final_values: Vec::new(),
            };
        }

        let active = self.matching_runtime(&config.device);
        let final_values =
            active.map_or_else(Vec::new, |runtime| self.backend.final_values(runtime));
        let mut ordered = poll.events;
        let Some(runtime) = active else {
            let decision = self
                .connection
                .update(None, poll.selected_disconnected, false);
            return HostControllerBatch {
                connected: false,
                reset: decision.reset,
                events: Vec::new(),
                final_values: Vec::new(),
            };
        };
        let decision = self.connection.update(
            Some(runtime),
            poll.selected_disconnected,
            controls_neutral(config, &final_values),
        );
        if decision.reset {
            ordered.clear();
        }
        HostControllerBatch {
            connected: decision.connected,
            reset: decision.reset,
            events: if decision.connected {
                ordered
            } else {
                Vec::new()
            },
            final_values,
        }
    }

    fn matching_runtime(&self, matcher: &ControllerDeviceMatcher) -> Option<ControllerRuntimeKey> {
        let backend = ControllerBackendKind::for_matcher(matcher)?;
        self.backend
            .devices()
            .iter()
            .find(|device| device_matches(matcher, &device.matcher))
            .map(|device| ControllerRuntimeKey {
                backend,
                runtime_id: device.runtime_id,
            })
    }
}

type ControllerDeviceIdentity = (String, Option<u16>, Option<u16>, String);

fn device_identity(matcher: &ControllerDeviceMatcher) -> ControllerDeviceIdentity {
    (
        matcher.guid.clone(),
        matcher.vendor_id,
        matcher.product_id,
        matcher.name.clone(),
    )
}

fn assign_device_occurrences(previous: &[ControllerDevice], devices: &mut [ControllerDevice]) {
    let mut used = BTreeMap::<ControllerDeviceIdentity, BTreeSet<u16>>::new();
    let mut preserved = vec![false; devices.len()];

    for (index, device) in devices.iter_mut().enumerate() {
        let identity = device_identity(&device.matcher);
        let Some(old) = previous.iter().find(|old| {
            old.runtime_id == device.runtime_id && device_identity(&old.matcher) == identity
        }) else {
            continue;
        };
        if used
            .entry(identity)
            .or_default()
            .insert(old.matcher.occurrence)
        {
            device.matcher.occurrence = old.matcher.occurrence;
            preserved[index] = true;
        }
    }

    for (index, device) in devices.iter_mut().enumerate() {
        if preserved[index] {
            continue;
        }
        let occupied = used.entry(device_identity(&device.matcher)).or_default();
        let occurrence = (0..=u16::MAX)
            .find(|candidate| !occupied.contains(candidate))
            .unwrap_or(u16::MAX);
        device.matcher.occurrence = occurrence;
        occupied.insert(occurrence);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct ControllerConnectionLatch {
    active_runtime: Option<ControllerRuntimeKey>,
    ready: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ControllerConnectionDecision {
    connected: bool,
    reset: bool,
}

impl ControllerConnectionLatch {
    fn update(
        &mut self,
        active_runtime: Option<ControllerRuntimeKey>,
        active_disconnected: bool,
        controls_neutral: bool,
    ) -> ControllerConnectionDecision {
        let connection_reset = active_disconnected || active_runtime != self.active_runtime;
        if connection_reset {
            self.ready = false;
        }
        self.active_runtime = active_runtime;
        if active_runtime.is_none() {
            self.ready = false;
        } else if !self.ready && controls_neutral {
            self.ready = true;
        }
        ControllerConnectionDecision {
            connected: self.ready,
            reset: connection_reset || (active_runtime.is_some() && !self.ready),
        }
    }
}

fn meaningful_guid(guid: &str) -> bool {
    !guid.is_empty() && !guid.eq_ignore_ascii_case("00000000-0000-0000-0000-000000000000")
}

fn device_matches(expected: &ControllerDeviceMatcher, actual: &ControllerDeviceMatcher) -> bool {
    if expected.backend != actual.backend || expected.platform != actual.platform {
        return false;
    }
    let mut compared_strong_identity = false;
    if !expected.guid.is_empty() && !actual.guid.is_empty() {
        compared_strong_identity = true;
        if expected.guid != actual.guid {
            return false;
        }
    }
    if let (
        Some(expected_vendor),
        Some(expected_product),
        Some(actual_vendor),
        Some(actual_product),
    ) = (
        expected.vendor_id,
        expected.product_id,
        actual.vendor_id,
        actual.product_id,
    ) {
        compared_strong_identity = true;
        if expected_vendor != actual_vendor || expected_product != actual_product {
            return false;
        }
    }
    let identity_matches =
        compared_strong_identity || (!expected.name.is_empty() && expected.name == actual.name);
    identity_matches && expected.occurrence == actual.occurrence
}

fn controls_neutral(config: &ControllerConfig, values: &[HostControlValue]) -> bool {
    let value = |control: HostControlId| resolve_control_value(values, control).unwrap_or(0.0);
    let axes_neutral = config.axes.iter().enumerate().all(|(axis, binding)| {
        config.profile.axis_present() & (1 << axis) == 0
            || axis_is_neutral(binding.transform, value(binding.host))
    });
    let buttons_neutral = config.buttons.iter().all(|binding| {
        binding.host.directed_value(value(binding.host.host)) < DIGITAL_PRESS_THRESHOLD
    }) && config.keys.iter().all(|binding| {
        binding.host.directed_value(value(binding.host.host)) < DIGITAL_PRESS_THRESHOLD
    });
    axes_neutral && buttons_neutral
}

fn axis_is_neutral(transform: AxisTransform, raw: f32) -> bool {
    let center = transform.calibration.center.clamp(-1.0, 1.0);
    match transform.span {
        AxisSpan::Full => (raw - center).abs() <= RECONNECT_NEUTRAL_TOLERANCE,
        AxisSpan::PositiveHalf => raw <= center + RECONNECT_NEUTRAL_TOLERANCE,
        AxisSpan::NegativeHalf => raw >= center - RECONNECT_NEUTRAL_TOLERANCE,
    }
}

fn event_value(gamepad: &Gamepad<'_>, event: EventType) -> Option<HostControlValue> {
    let (kind, code, value) = match event {
        EventType::ButtonPressed(_, code) => (HostControlKind::Button, code, 1.0),
        EventType::ButtonReleased(_, code) => (HostControlKind::Button, code, 0.0),
        EventType::ButtonChanged(_, value, code) => (HostControlKind::Button, code, value),
        EventType::AxisChanged(_, value, code) => (HostControlKind::Axis, code, value),
        _ => return None,
    };
    Some(HostControlValue {
        control: control_id(gamepad, kind, code),
        value,
    })
}

fn control_id(gamepad: &Gamepad<'_>, kind: HostControlKind, code: Code) -> HostControlId {
    let semantic = match kind {
        HostControlKind::Axis => semantic_axes()
            .find(|axis| gamepad.axis_code(axis.gilrs()) == Some(code))
            .map(HostSemanticControl::Axis),
        HostControlKind::Button => semantic_buttons()
            .find(|button| gamepad.button_code(button.gilrs()) == Some(code))
            .map(HostSemanticControl::Button),
    };
    HostControlId {
        kind,
        raw_code: Some(code.into_u32()),
        semantic,
    }
}

fn semantic_axes() -> impl Iterator<Item = JoystickAxis> {
    [
        JoystickAxis::LeftStickX,
        JoystickAxis::LeftStickY,
        JoystickAxis::LeftZ,
        JoystickAxis::RightStickX,
        JoystickAxis::RightStickY,
        JoystickAxis::RightZ,
        JoystickAxis::DPadX,
        JoystickAxis::DPadY,
    ]
    .into_iter()
}

fn semantic_buttons() -> impl Iterator<Item = JoystickButton> {
    [
        JoystickButton::South,
        JoystickButton::East,
        JoystickButton::North,
        JoystickButton::West,
        JoystickButton::C,
        JoystickButton::Z,
        JoystickButton::LeftTrigger,
        JoystickButton::LeftTrigger2,
        JoystickButton::RightTrigger,
        JoystickButton::RightTrigger2,
        JoystickButton::Select,
        JoystickButton::Start,
        JoystickButton::Mode,
        JoystickButton::LeftThumb,
        JoystickButton::RightThumb,
        JoystickButton::DPadUp,
        JoystickButton::DPadDown,
        JoystickButton::DPadLeft,
        JoystickButton::DPadRight,
    ]
    .into_iter()
}

fn backend_name() -> &'static str {
    if cfg!(windows) {
        "gilrs-wgi"
    } else if cfg!(target_os = "linux") {
        "gilrs-evdev"
    } else if cfg!(target_os = "macos") {
        "gilrs-iokit"
    } else {
        "gilrs"
    }
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
#[path = "controller_test.rs"]
mod tests;
