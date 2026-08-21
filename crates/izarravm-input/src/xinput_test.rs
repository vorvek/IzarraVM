// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

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
