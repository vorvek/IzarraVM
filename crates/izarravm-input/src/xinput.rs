// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use rusty_xinput::{XInputHandle, XInputState, XInputUsageError};
use std::time::{Duration, Instant};

use crate::{
    ControllerDevice, ControllerDeviceMatcher, HostControlId, HostControlKind, HostControlValue,
    HostSemanticControl, JoystickAxis, JoystickButton,
};

const SLOT_COUNT: usize = 4;
const MISSING_PROBE_INTERVAL: Duration = Duration::from_secs(1);
const MISSING_PROBE_STAGGER: Duration = Duration::from_millis(250);
const AXIS_LEFT_X: u32 = 0;
const AXIS_LEFT_Y: u32 = 1;
const AXIS_RIGHT_X: u32 = 2;
const AXIS_RIGHT_Y: u32 = 3;
const AXIS_LEFT_TRIGGER: u32 = 4;
const AXIS_RIGHT_TRIGGER: u32 = 5;
const BUTTON_DPAD_UP: u32 = 0;
const BUTTON_DPAD_DOWN: u32 = 1;
const BUTTON_DPAD_LEFT: u32 = 2;
const BUTTON_DPAD_RIGHT: u32 = 3;
const BUTTON_SOUTH: u32 = 4;
const BUTTON_EAST: u32 = 5;
const BUTTON_WEST: u32 = 6;
const BUTTON_NORTH: u32 = 7;
const BUTTON_LEFT_SHOULDER: u32 = 8;
const BUTTON_RIGHT_SHOULDER: u32 = 9;
const BUTTON_SELECT: u32 = 10;
const BUTTON_START: u32 = 11;
const BUTTON_LEFT_THUMB: u32 = 12;
const BUTTON_RIGHT_THUMB: u32 = 13;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct XInputSnapshot {
    sticks: [i16; 4],
    triggers: [u8; 2],
    buttons: [bool; 14],
}

impl From<XInputState> for XInputSnapshot {
    fn from(state: XInputState) -> Self {
        let (left_x, left_y) = state.left_stick_raw();
        let (right_x, right_y) = state.right_stick_raw();
        Self {
            sticks: [left_x, left_y, right_x, right_y],
            triggers: [state.left_trigger(), state.right_trigger()],
            buttons: [
                state.arrow_up(),
                state.arrow_down(),
                state.arrow_left(),
                state.arrow_right(),
                state.south_button(),
                state.east_button(),
                state.west_button(),
                state.north_button(),
                state.left_shoulder(),
                state.right_shoulder(),
                state.select_button(),
                state.start_button(),
                state.left_thumb_button(),
                state.right_thumb_button(),
            ],
        }
    }
}

impl XInputSnapshot {
    fn values(self) -> Vec<HostControlValue> {
        let mut values = Vec::with_capacity(20);
        for index in 0..6 {
            values.push(axis_value(self, index));
        }
        for index in 0..self.buttons.len() {
            values.push(button_value(self, index));
        }
        values
    }
}

fn axis_value(snapshot: XInputSnapshot, index: usize) -> HostControlValue {
    let (raw_code, axis, value) = match index {
        0 => (
            AXIS_LEFT_X,
            JoystickAxis::LeftStickX,
            normalize_stick(snapshot.sticks[0]),
        ),
        1 => (
            AXIS_LEFT_Y,
            JoystickAxis::LeftStickY,
            normalize_stick(snapshot.sticks[1]),
        ),
        2 => (
            AXIS_RIGHT_X,
            JoystickAxis::RightStickX,
            normalize_stick(snapshot.sticks[2]),
        ),
        3 => (
            AXIS_RIGHT_Y,
            JoystickAxis::RightStickY,
            normalize_stick(snapshot.sticks[3]),
        ),
        4 => (
            AXIS_LEFT_TRIGGER,
            JoystickAxis::LeftZ,
            f32::from(snapshot.triggers[0]) / 255.0,
        ),
        5 => (
            AXIS_RIGHT_TRIGGER,
            JoystickAxis::RightZ,
            f32::from(snapshot.triggers[1]) / 255.0,
        ),
        _ => unreachable!(),
    };
    HostControlValue {
        control: HostControlId {
            kind: HostControlKind::Axis,
            raw_code: Some(raw_code),
            semantic: Some(HostSemanticControl::Axis(axis)),
        },
        value,
    }
}

fn button_value(snapshot: XInputSnapshot, index: usize) -> HostControlValue {
    let (raw_code, button) = [
        (BUTTON_DPAD_UP, JoystickButton::DPadUp),
        (BUTTON_DPAD_DOWN, JoystickButton::DPadDown),
        (BUTTON_DPAD_LEFT, JoystickButton::DPadLeft),
        (BUTTON_DPAD_RIGHT, JoystickButton::DPadRight),
        (BUTTON_SOUTH, JoystickButton::South),
        (BUTTON_EAST, JoystickButton::East),
        (BUTTON_WEST, JoystickButton::West),
        (BUTTON_NORTH, JoystickButton::North),
        (BUTTON_LEFT_SHOULDER, JoystickButton::LeftTrigger),
        (BUTTON_RIGHT_SHOULDER, JoystickButton::RightTrigger),
        (BUTTON_SELECT, JoystickButton::Select),
        (BUTTON_START, JoystickButton::Start),
        (BUTTON_LEFT_THUMB, JoystickButton::LeftThumb),
        (BUTTON_RIGHT_THUMB, JoystickButton::RightThumb),
    ][index];
    HostControlValue {
        control: HostControlId {
            kind: HostControlKind::Button,
            raw_code: Some(raw_code),
            semantic: Some(HostSemanticControl::Button(button)),
        },
        value: f32::from(snapshot.buttons[index]),
    }
}

fn normalize_stick(value: i16) -> f32 {
    if value < 0 {
        f32::from(value) / 32768.0
    } else {
        f32::from(value) / 32767.0
    }
    .clamp(-1.0, 1.0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum XInputReadError {
    Disconnected,
    Other,
}

pub(super) trait XInputSource {
    fn read(&mut self, slot: usize) -> Result<XInputSnapshot, XInputReadError>;
}

pub(super) struct NativeXInputSource {
    handle: XInputHandle,
}

impl NativeXInputSource {
    fn load() -> Option<Self> {
        XInputHandle::load_default()
            .ok()
            .map(|handle| Self { handle })
    }
}

impl XInputSource for NativeXInputSource {
    fn read(&mut self, slot: usize) -> Result<XInputSnapshot, XInputReadError> {
        self.handle
            .get_state(slot as u32)
            .map(XInputSnapshot::from)
            .map_err(|error| match error {
                XInputUsageError::DeviceNotConnected => XInputReadError::Disconnected,
                _ => XInputReadError::Other,
            })
    }
}

pub(super) struct XInputPoll {
    pub events: Vec<HostControlValue>,
    pub selected_disconnected: bool,
    pub devices_changed: bool,
}

pub(super) struct XInputBackend<S = NativeXInputSource> {
    source: S,
    states: [Option<XInputSnapshot>; SLOT_COUNT],
    next_missing_probe: [Instant; SLOT_COUNT],
    active: bool,
}

impl XInputBackend<NativeXInputSource> {
    pub fn new() -> Option<Self> {
        NativeXInputSource::load().map(Self::inactive)
    }
}

impl<S: XInputSource> XInputBackend<S> {
    fn inactive(source: S) -> Self {
        let now = Instant::now();
        Self {
            source,
            states: [None; SLOT_COUNT],
            next_missing_probe: [now; SLOT_COUNT],
            active: false,
        }
    }

    #[cfg(test)]
    fn with_source(source: S, now: Instant) -> Self {
        let mut backend = Self::inactive(source);
        backend.activate(now);
        backend
    }

    pub fn activate(&mut self, now: Instant) {
        for (slot, state) in self.states.iter_mut().enumerate() {
            *state = self.source.read(slot).ok();
            self.next_missing_probe[slot] = now + MISSING_PROBE_STAGGER * (slot as u32 + 1);
        }
        self.active = true;
    }

    pub fn deactivate(&mut self) {
        self.states = [None; SLOT_COUNT];
        self.active = false;
    }

    pub fn poll(&mut self, selected: Option<usize>, now: Instant) -> XInputPoll {
        if !self.active {
            return XInputPoll {
                events: Vec::new(),
                selected_disconnected: false,
                devices_changed: false,
            };
        }
        let mut events = Vec::new();
        let mut selected_disconnected = false;
        let mut devices_changed = false;
        let mut disconnected_slots = [false; SLOT_COUNT];
        for (slot, disconnected_this_tick) in disconnected_slots.iter_mut().enumerate() {
            let Some(previous) = self.states[slot] else {
                continue;
            };
            match self.source.read(slot) {
                Ok(next) => {
                    if selected == Some(slot) {
                        push_changed_values(previous, next, &mut events);
                    }
                    self.states[slot] = Some(next);
                }
                Err(XInputReadError::Disconnected) => {
                    self.states[slot] = None;
                    *disconnected_this_tick = true;
                    self.next_missing_probe[slot] = now + MISSING_PROBE_INTERVAL;
                    selected_disconnected |= selected == Some(slot);
                    devices_changed = true;
                }
                Err(XInputReadError::Other) => {}
            }
        }

        let probe = (0..SLOT_COUNT).find(|slot| {
            self.states[*slot].is_none()
                && !disconnected_slots[*slot]
                && now >= self.next_missing_probe[*slot]
        });
        if let Some(slot) = probe
            && self.states[slot].is_none()
            && !disconnected_slots[slot]
        {
            self.next_missing_probe[slot] = now + MISSING_PROBE_INTERVAL;
            if let Ok(state) = self.source.read(slot) {
                self.states[slot] = Some(state);
                devices_changed = true;
            }
        }

        // XInput exposes snapshots, so edges wholly between polls cannot be recovered.
        XInputPoll {
            events,
            selected_disconnected,
            devices_changed,
        }
    }

    pub fn devices(&self) -> Vec<ControllerDevice> {
        self.states
            .iter()
            .enumerate()
            .filter(|(_, state)| state.is_some())
            .map(|(slot, _)| ControllerDevice {
                runtime_id: slot,
                matcher: ControllerDeviceMatcher {
                    backend: "xinput".to_owned(),
                    platform: "windows".to_owned(),
                    guid: format!("xinput-slot-{slot}"),
                    vendor_id: None,
                    product_id: None,
                    name: format!("XInput controller {}", slot + 1),
                    occurrence: slot as u16,
                },
            })
            .collect()
    }

    pub fn values(&self, slot: usize) -> Vec<HostControlValue> {
        self.states
            .get(slot)
            .and_then(|state| *state)
            .map_or_else(Vec::new, XInputSnapshot::values)
    }
}

fn push_changed_values(
    previous: XInputSnapshot,
    current: XInputSnapshot,
    out: &mut Vec<HostControlValue>,
) {
    for index in 0..6 {
        let current_value = axis_value(current, index);
        if axis_value(previous, index).value != current_value.value {
            out.push(current_value);
        }
    }
    for index in 0..current.buttons.len() {
        if previous.buttons[index] != current.buttons[index] {
            out.push(button_value(current, index));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct TestSource {
        states: [Option<XInputSnapshot>; SLOT_COUNT],
        calls: [usize; SLOT_COUNT],
    }

    impl XInputSource for TestSource {
        fn read(&mut self, slot: usize) -> Result<XInputSnapshot, XInputReadError> {
            self.calls[slot] += 1;
            self.states[slot].ok_or(XInputReadError::Disconnected)
        }
    }

    fn source(states: [Option<XInputSnapshot>; SLOT_COUNT]) -> TestSource {
        TestSource {
            states,
            calls: [0; SLOT_COUNT],
        }
    }

    #[test]
    fn stick_extrema_use_asymmetric_xinput_divisors() {
        assert_eq!(normalize_stick(i16::MIN), -1.0);
        assert_eq!(normalize_stick(i16::MAX), 1.0);
        assert_eq!(normalize_stick(0), 0.0);
        assert!(normalize_stick(-16_384) < -0.4999);
        assert!(normalize_stick(16_384) > 0.5000);
    }

    #[test]
    fn state_decoding_preserves_y_and_unipolar_triggers() {
        let snapshot = XInputSnapshot {
            sticks: [i16::MIN, i16::MAX, 0, -16_384],
            triggers: [0, 255],
            ..XInputSnapshot::default()
        };
        let values = snapshot.values();
        assert_eq!(values[0].value, -1.0);
        assert_eq!(values[1].value, 1.0, "positive XInput Y remains up");
        assert_eq!(values[4].value, 0.0);
        assert_eq!(values[5].value, 1.0);
    }

    #[test]
    fn diffs_follow_axis_then_button_order() {
        let previous = XInputSnapshot::default();
        let mut current = previous;
        current.sticks[1] = i16::MAX;
        current.triggers[0] = 255;
        current.buttons[0] = true;
        current.buttons[4] = true;
        let mut changed = Vec::new();
        push_changed_values(previous, current, &mut changed);
        assert_eq!(
            changed
                .iter()
                .map(|value| (value.control.kind, value.control.raw_code))
                .collect::<Vec<_>>(),
            [
                (HostControlKind::Axis, Some(1)),
                (HostControlKind::Axis, Some(4)),
                (HostControlKind::Button, Some(0)),
                (HostControlKind::Button, Some(4)),
            ]
        );
    }

    #[test]
    fn missing_slots_are_staggered_to_one_probe_each_second() {
        let now = Instant::now();
        let mut backend = XInputBackend::with_source(source([None; SLOT_COUNT]), now);
        backend.source.calls = [0; SLOT_COUNT];
        for quarter in 1..=8 {
            backend.poll(None, now + MISSING_PROBE_STAGGER * quarter);
        }
        assert_eq!(backend.source.calls, [2; SLOT_COUNT]);
    }

    #[test]
    fn inactive_backend_never_polls_xinput() {
        let mut backend =
            XInputBackend::inactive(source([Some(XInputSnapshot::default()), None, None, None]));
        assert!(backend.poll(Some(0), Instant::now()).events.is_empty());
        assert_eq!(backend.source.calls, [0; SLOT_COUNT]);
        assert!(backend.devices().is_empty());
    }

    #[test]
    fn unselected_slot_changes_are_not_replayed_after_selection() {
        let now = Instant::now();
        let mut backend = XInputBackend::with_source(
            source([
                Some(XInputSnapshot::default()),
                Some(XInputSnapshot::default()),
                None,
                None,
            ]),
            now,
        );
        let mut second = XInputSnapshot::default();
        second.buttons[4] = true;
        backend.source.states[1] = Some(second);
        assert!(backend.poll(Some(0), now).events.is_empty());
        assert!(backend.poll(Some(1), now).events.is_empty());
        assert_eq!(backend.values(1)[10].value, 1.0);
    }

    #[test]
    fn hotplug_seeds_state_and_disconnects_once() {
        let now = Instant::now();
        let mut backend = XInputBackend::with_source(source([None; SLOT_COUNT]), now);
        let mut active = XInputSnapshot::default();
        active.buttons[4] = true;
        backend.source.states[0] = Some(active);
        let connected = backend.poll(Some(0), now + MISSING_PROBE_STAGGER);
        assert!(connected.events.is_empty());
        assert!(!connected.selected_disconnected);
        assert!(connected.devices_changed);
        assert_eq!(backend.devices().len(), 1);

        active.buttons[4] = false;
        active.triggers[0] = 255;
        backend.source.states[0] = Some(active);
        let changed = backend.poll(Some(0), now + MISSING_PROBE_STAGGER);
        assert_eq!(changed.events.len(), 2);
        assert!(!changed.selected_disconnected);

        backend.source.states[0] = None;
        let disconnected = backend.poll(Some(0), now + MISSING_PROBE_STAGGER);
        assert!(disconnected.selected_disconnected);
        assert!(disconnected.devices_changed);
        assert!(backend.devices().is_empty());
        assert!(
            !backend
                .poll(Some(0), now + MISSING_PROBE_STAGGER)
                .selected_disconnected
        );
    }
}
