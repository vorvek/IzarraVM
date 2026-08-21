// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::ControllerDeviceMatcher;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

#[derive(Default)]
struct TestWgiState {
    devices: Vec<ControllerDevice>,
    events: Vec<HostControlValue>,
    final_values: Vec<HostControlValue>,
    devices_changed: bool,
    selected_disconnected: bool,
    polls: usize,
    refreshes: usize,
    drops: usize,
}

struct TestWgi {
    state: Rc<RefCell<TestWgiState>>,
}

impl Drop for TestWgi {
    fn drop(&mut self) {
        self.state.borrow_mut().drops += 1;
    }
}

impl WgiDriver for TestWgi {
    fn devices(&self) -> Vec<ControllerDevice> {
        self.state.borrow().devices.clone()
    }

    fn device_count(&self) -> usize {
        self.state.borrow().devices.len()
    }

    fn poll(&mut self, _selected: Option<ControllerRuntimeKey>) -> BackendPoll {
        let mut state = self.state.borrow_mut();
        state.polls += 1;
        BackendPoll {
            events: std::mem::take(&mut state.events),
            devices_changed: std::mem::take(&mut state.devices_changed),
            selected_disconnected: std::mem::take(&mut state.selected_disconnected),
            ..BackendPoll::default()
        }
    }

    fn final_values(&mut self, _runtime: ControllerRuntimeKey) -> Vec<HostControlValue> {
        self.state.borrow().final_values.clone()
    }

    fn refresh_devices(&mut self) {
        self.state.borrow_mut().refreshes += 1;
    }
}

struct TestWgiFactory {
    state: Rc<RefCell<TestWgiState>>,
    creates: Rc<Cell<usize>>,
}

impl WgiFactory for TestWgiFactory {
    fn create(&mut self, _previous: &[ControllerDevice]) -> Result<Box<dyn WgiDriver>, String> {
        self.creates.set(self.creates.get() + 1);
        Ok(Box::new(TestWgi {
            state: self.state.clone(),
        }))
    }
}

#[derive(Default)]
struct TestXInputState {
    devices: Vec<ControllerDevice>,
    activates: usize,
    deactivates: usize,
    polls: usize,
}

struct TestXInput {
    state: Rc<RefCell<TestXInputState>>,
}

impl XInputDriver for TestXInput {
    fn activate(&mut self, _now: Instant) {
        self.state.borrow_mut().activates += 1;
    }

    fn deactivate(&mut self) {
        self.state.borrow_mut().deactivates += 1;
    }

    fn poll(&mut self, _selected: Option<usize>, _now: Instant) -> XInputPoll {
        self.state.borrow_mut().polls += 1;
        XInputPoll {
            events: Vec::new(),
            selected_disconnected: false,
            devices_changed: false,
        }
    }

    fn devices(&self) -> Vec<ControllerDevice> {
        self.state.borrow().devices.clone()
    }

    fn values(&self, _slot: usize) -> Vec<HostControlValue> {
        Vec::new()
    }
}

fn matcher(backend: &str, vendor: u16, product: u16, name: &str) -> ControllerDeviceMatcher {
    ControllerDeviceMatcher {
        backend: backend.into(),
        platform: "windows".into(),
        guid: if backend == "xinput" {
            "xinput-slot-0".into()
        } else {
            "00000000-0000-0000-0000-000000000000".into()
        },
        vendor_id: (backend != "xinput").then_some(vendor),
        product_id: (backend != "xinput").then_some(product),
        name: name.into(),
        occurrence: 0,
    }
}

fn test_backend(
    wgi: Rc<RefCell<TestWgiState>>,
    creates: Rc<Cell<usize>>,
    xinput: Rc<RefCell<TestXInputState>>,
) -> WindowsControllerBackend {
    WindowsControllerBackend {
        wgi_factory: Box::new(TestWgiFactory {
            state: wgi,
            creates,
        }),
        gilrs: None,
        xinput: Some(Box::new(TestXInput { state: xinput })),
        devices: Vec::new(),
        wgi_history: Vec::new(),
        authority: WindowsControllerAuthority::AwaitingFocusedWgi,
        retry: WgiRetry::default(),
        focused: false,
        topology_generation: 0,
    }
}

#[test]
fn wgi_retry_is_immediate_bounded_and_rearmed() {
    let now = Instant::now();
    let mut retry = WgiRetry::default();
    retry.arm(now);
    assert!(retry.due(now));
    retry.record_empty(now);
    assert!(!retry.due(now + Duration::from_millis(249)));
    assert!(retry.due(now + Duration::from_millis(250)));
    retry.record_empty(now + Duration::from_millis(250));
    assert!(retry.due(now + Duration::from_millis(1_250)));
    retry.record_empty(now + Duration::from_millis(1_250));
    assert!(!retry.due(now + Duration::from_secs(30)));
    retry.arm(now + Duration::from_secs(31));
    assert!(retry.due(now + Duration::from_secs(31)));
}

#[test]
fn unfocused_backend_calls_neither_api_and_focus_loss_is_idempotent() {
    let wgi = Rc::new(RefCell::new(TestWgiState::default()));
    let creates = Rc::new(Cell::new(0));
    let xinput = Rc::new(RefCell::new(TestXInputState::default()));
    let mut backend = test_backend(wgi.clone(), creates.clone(), xinput.clone());
    let now = Instant::now();
    assert!(backend.poll(None, now).events.is_empty());
    backend.focus_lost();
    backend.focus_lost();
    assert_eq!(creates.get(), 0);
    assert_eq!(wgi.borrow().polls, 0);
    assert_eq!(xinput.borrow().polls, 0);
    assert_eq!(xinput.borrow().deactivates, 0);
}

#[test]
fn wgi_promotion_deactivates_xinput_before_routing_and_discards_old_edges() {
    let wgi = Rc::new(RefCell::new(TestWgiState::default()));
    let creates = Rc::new(Cell::new(0));
    let xinput = Rc::new(RefCell::new(TestXInputState {
        devices: vec![ControllerDevice {
            runtime_id: 0,
            matcher: matcher("xinput", 0, 0, "XInput controller 1"),
        }],
        ..TestXInputState::default()
    }));
    let mut backend = test_backend(wgi.clone(), creates, xinput.clone());
    let now = Instant::now();
    backend.focus_gained(now);
    backend.poll(None, now);
    assert_eq!(xinput.borrow().activates, 1);
    assert_eq!(xinput.borrow().polls, 1);

    wgi.borrow_mut().devices = vec![ControllerDevice {
        runtime_id: 7,
        matcher: matcher("gilrs-wgi", 0x3434, 0x1061, "Generic pad"),
    }];
    wgi.borrow_mut().events.push(HostControlValue {
        control: crate::HostControlId::semantic_button(crate::JoystickButton::South),
        value: 1.0,
    });
    wgi.borrow_mut().devices_changed = true;
    let poll = backend.poll(
        Some(ControllerRuntimeKey {
            backend: ControllerBackendKind::XInput,
            runtime_id: 0,
        }),
        now + Duration::from_millis(10),
    );
    assert!(poll.boundary_reset);
    assert!(poll.events.is_empty());
    assert_eq!(xinput.borrow().deactivates, 1);
    assert_eq!(xinput.borrow().polls, 1);
    assert_eq!(backend.devices.len(), 1);
    assert_eq!(backend.devices[0].matcher.backend, "gilrs-wgi");
}

#[test]
fn same_identity_topology_event_invalidates_name_generation() {
    let wgi = Rc::new(RefCell::new(TestWgiState {
        devices: vec![ControllerDevice {
            runtime_id: 2,
            matcher: matcher("gilrs-wgi", 0x3434, 0x1061, "Generic pad"),
        }],
        ..TestWgiState::default()
    }));
    let creates = Rc::new(Cell::new(0));
    let xinput = Rc::new(RefCell::new(TestXInputState::default()));
    let mut backend = test_backend(wgi.clone(), creates, xinput);
    let now = Instant::now();
    backend.focus_gained(now);
    backend.poll(None, now);
    let generation = backend.topology_generation;
    wgi.borrow_mut().devices_changed = true;
    backend.poll(None, now + Duration::from_millis(10));
    assert!(backend.topology_generation > generation);
}

#[test]
fn nonempty_wgi_hotplug_republishes_rows_and_disconnects_the_selected_runtime() {
    let wgi = Rc::new(RefCell::new(TestWgiState {
        devices: vec![
            ControllerDevice {
                runtime_id: 1,
                matcher: matcher("gilrs-wgi", 0x1111, 0x0001, "Pad A"),
            },
            ControllerDevice {
                runtime_id: 2,
                matcher: matcher("gilrs-wgi", 0x2222, 0x0002, "Pad B"),
            },
        ],
        ..TestWgiState::default()
    }));
    let creates = Rc::new(Cell::new(0));
    let xinput = Rc::new(RefCell::new(TestXInputState::default()));
    let mut backend = test_backend(wgi.clone(), creates, xinput);
    let now = Instant::now();
    backend.focus_gained(now);
    backend.poll(None, now);
    assert_eq!(
        backend
            .devices
            .iter()
            .map(|device| device.runtime_id)
            .collect::<Vec<_>>(),
        [1, 2]
    );

    wgi.borrow_mut().devices.push(ControllerDevice {
        runtime_id: 3,
        matcher: matcher("gilrs-wgi", 0x3333, 0x0003, "Pad C"),
    });
    wgi.borrow_mut().devices_changed = true;
    backend.poll(None, now + Duration::from_millis(10));
    assert_eq!(
        backend
            .devices
            .iter()
            .map(|device| device.runtime_id)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );

    wgi.borrow_mut()
        .devices
        .retain(|device| device.runtime_id != 2);
    wgi.borrow_mut().devices_changed = true;
    wgi.borrow_mut().selected_disconnected = true;
    let poll = backend.poll(
        Some(ControllerRuntimeKey {
            backend: ControllerBackendKind::Gilrs,
            runtime_id: 2,
        }),
        now + Duration::from_millis(20),
    );
    assert!(poll.selected_disconnected);
    assert_eq!(
        backend
            .devices
            .iter()
            .map(|device| device.runtime_id)
            .collect::<Vec<_>>(),
        [1, 3]
    );
}

#[test]
fn focused_backend_is_dropped_and_stays_quiet_after_focus_loss() {
    let wgi = Rc::new(RefCell::new(TestWgiState::default()));
    let creates = Rc::new(Cell::new(0));
    let xinput = Rc::new(RefCell::new(TestXInputState::default()));
    let mut backend = test_backend(wgi.clone(), creates, xinput.clone());
    let now = Instant::now();
    backend.focus_gained(now);
    backend.poll(None, now);
    let polls = xinput.borrow().polls;
    backend.focus_lost();
    assert_eq!(wgi.borrow().drops, 1);
    assert_eq!(xinput.borrow().deactivates, 1);
    backend.poll(None, now + Duration::from_secs(30));
    assert_eq!(xinput.borrow().polls, polls);
}
