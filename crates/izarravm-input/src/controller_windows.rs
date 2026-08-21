// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use std::time::{Duration, Instant};

use crate::{
    ControllerDevice, HostControlValue,
    controller::{
        BackendPoll, ControllerBackendDriver, ControllerBackendKind, ControllerRuntimeKey,
        GilrsBackend,
    },
    xinput::{XInputBackend, XInputPoll},
};

const WGI_EMPTY_GRACE: Duration = Duration::from_millis(750);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowsControllerAuthority {
    AwaitingFocusedWgi,
    XInputFallback,
    WgiAuthoritative,
    WgiEmptyGrace { deadline: Instant },
}

#[derive(Debug, Default)]
struct WgiRetry {
    attempts: u8,
    next_attempt: Option<Instant>,
}

impl WgiRetry {
    fn arm(&mut self, now: Instant) {
        self.attempts = 0;
        self.next_attempt = Some(now);
    }

    fn due(&self, now: Instant) -> bool {
        self.next_attempt.is_some_and(|attempt| now >= attempt)
    }

    fn record_empty(&mut self, now: Instant) {
        self.attempts = self.attempts.saturating_add(1);
        self.next_attempt = match self.attempts {
            1 => Some(now + Duration::from_millis(250)),
            2 => Some(now + Duration::from_secs(1)),
            _ => None,
        };
    }

    fn complete(&mut self) {
        self.next_attempt = None;
    }
}

trait WgiDriver {
    fn devices(&self) -> Vec<ControllerDevice>;
    fn device_count(&self) -> usize;
    fn poll(&mut self, selected: Option<ControllerRuntimeKey>) -> BackendPoll;
    fn final_values(&mut self, runtime: ControllerRuntimeKey) -> Vec<HostControlValue>;
    fn refresh_devices(&mut self);
}

impl WgiDriver for GilrsBackend {
    fn devices(&self) -> Vec<ControllerDevice> {
        self.devices().to_vec()
    }

    fn device_count(&self) -> usize {
        self.devices().len()
    }

    fn poll(&mut self, selected: Option<ControllerRuntimeKey>) -> BackendPoll {
        self.poll_events(selected)
    }

    fn final_values(&mut self, runtime: ControllerRuntimeKey) -> Vec<HostControlValue> {
        self.final_values(runtime)
    }

    fn refresh_devices(&mut self) {
        self.refresh_devices(true);
    }
}

trait WgiFactory {
    fn create(&mut self, previous: &[ControllerDevice]) -> Result<Box<dyn WgiDriver>, String>;
}

struct NativeWgiFactory;

impl WgiFactory for NativeWgiFactory {
    fn create(&mut self, previous: &[ControllerDevice]) -> Result<Box<dyn WgiDriver>, String> {
        GilrsBackend::new_with_previous(previous)
            .map(|backend| Box::new(backend) as Box<dyn WgiDriver>)
    }
}

trait XInputDriver {
    fn activate(&mut self, now: Instant);
    fn deactivate(&mut self);
    fn poll(&mut self, selected: Option<usize>, now: Instant) -> XInputPoll;
    fn devices(&self) -> Vec<ControllerDevice>;
    fn values(&self, slot: usize) -> Vec<HostControlValue>;
}

impl XInputDriver for XInputBackend {
    fn activate(&mut self, now: Instant) {
        self.activate(now);
    }

    fn deactivate(&mut self) {
        self.deactivate();
    }

    fn poll(&mut self, selected: Option<usize>, now: Instant) -> XInputPoll {
        self.poll(selected, now)
    }

    fn devices(&self) -> Vec<ControllerDevice> {
        self.devices()
    }

    fn values(&self, slot: usize) -> Vec<HostControlValue> {
        self.values(slot)
    }
}

pub(super) struct WindowsControllerBackend {
    wgi_factory: Box<dyn WgiFactory>,
    gilrs: Option<Box<dyn WgiDriver>>,
    xinput: Option<Box<dyn XInputDriver>>,
    devices: Vec<ControllerDevice>,
    wgi_history: Vec<ControllerDevice>,
    authority: WindowsControllerAuthority,
    retry: WgiRetry,
    focused: bool,
    topology_generation: u64,
}

impl WindowsControllerBackend {
    pub(super) fn new() -> Self {
        Self {
            wgi_factory: Box::new(NativeWgiFactory),
            gilrs: None,
            xinput: XInputBackend::new().map(|backend| Box::new(backend) as Box<dyn XInputDriver>),
            devices: Vec::new(),
            wgi_history: Vec::new(),
            authority: WindowsControllerAuthority::AwaitingFocusedWgi,
            retry: WgiRetry::default(),
            focused: false,
            topology_generation: 0,
        }
    }

    fn bump_topology(&mut self) {
        self.topology_generation = self.topology_generation.wrapping_add(1);
    }

    fn publish(&mut self, force_generation: bool) {
        let devices = match self.authority {
            WindowsControllerAuthority::AwaitingFocusedWgi => Vec::new(),
            WindowsControllerAuthority::XInputFallback => self
                .xinput
                .as_ref()
                .map_or_else(Vec::new, |xinput| xinput.devices()),
            WindowsControllerAuthority::WgiAuthoritative
            | WindowsControllerAuthority::WgiEmptyGrace { .. } => self
                .gilrs
                .as_ref()
                .map_or_else(Vec::new, |gilrs| gilrs.devices()),
        };
        if force_generation || devices != self.devices {
            self.devices = devices;
            self.bump_topology();
        }
    }

    fn selected_xinput(selected: Option<ControllerRuntimeKey>) -> bool {
        selected.is_some_and(|key| key.backend == ControllerBackendKind::XInput)
    }

    fn promote_wgi(&mut self, selected: Option<ControllerRuntimeKey>) -> bool {
        let reset = Self::selected_xinput(selected);
        if let Some(xinput) = &mut self.xinput {
            xinput.deactivate();
        }
        self.authority = WindowsControllerAuthority::WgiAuthoritative;
        self.retry.complete();
        self.publish(true);
        reset
    }

    fn enter_xinput_fallback(&mut self, now: Instant) {
        if !matches!(self.authority, WindowsControllerAuthority::XInputFallback) {
            if let Some(xinput) = &mut self.xinput {
                xinput.activate(now);
            }
            self.authority = WindowsControllerAuthority::XInputFallback;
            self.publish(true);
        }
    }

    fn try_wgi(&mut self, now: Instant, selected: Option<ControllerRuntimeKey>) -> bool {
        if !self.retry.due(now) {
            return false;
        }
        let backend = self.wgi_factory.create(&self.wgi_history);
        match backend {
            Ok(backend) => {
                let devices = backend.devices();
                let populated = !devices.is_empty();
                self.wgi_history = devices;
                self.gilrs = Some(backend);
                self.bump_topology();
                if populated {
                    self.promote_wgi(selected)
                } else {
                    self.enter_xinput_fallback(now);
                    self.retry.record_empty(now);
                    false
                }
            }
            Err(_) => {
                self.gilrs = None;
                self.enter_xinput_fallback(now);
                self.retry.record_empty(now);
                false
            }
        }
    }

    fn poll_focused(
        &mut self,
        selected: Option<ControllerRuntimeKey>,
        now: Instant,
    ) -> BackendPoll {
        let mut boundary_reset = self.try_wgi(now, selected);
        let selected_wgi = selected.filter(|key| key.backend == ControllerBackendKind::Gilrs);
        let mut wgi_poll = self
            .gilrs
            .as_mut()
            .map_or_else(BackendPoll::default, |gilrs| {
                let routed = matches!(
                    self.authority,
                    WindowsControllerAuthority::WgiAuthoritative
                        | WindowsControllerAuthority::WgiEmptyGrace { .. }
                )
                .then_some(selected_wgi)
                .flatten();
                gilrs.poll(routed)
            });

        if wgi_poll.devices_changed {
            if let Some(gilrs) = &self.gilrs {
                self.wgi_history = gilrs.devices();
            }
            self.publish(true);
        }
        let mut wgi_count = self.gilrs.as_ref().map_or(0, |gilrs| gilrs.device_count());

        match self.authority {
            WindowsControllerAuthority::XInputFallback if wgi_count > 0 => {
                boundary_reset |= self.promote_wgi(selected);
                wgi_poll.events.clear();
                return BackendPoll {
                    boundary_reset,
                    devices_changed: true,
                    ..BackendPoll::default()
                };
            }
            WindowsControllerAuthority::WgiAuthoritative if wgi_count == 0 => {
                self.authority = WindowsControllerAuthority::WgiEmptyGrace {
                    deadline: now + WGI_EMPTY_GRACE,
                };
                self.publish(true);
            }
            WindowsControllerAuthority::WgiEmptyGrace { .. } if wgi_count > 0 => {
                self.authority = WindowsControllerAuthority::WgiAuthoritative;
                self.publish(true);
                wgi_poll.events.clear();
            }
            WindowsControllerAuthority::WgiEmptyGrace { deadline } if now >= deadline => {
                if let Some(gilrs) = &mut self.gilrs {
                    gilrs.refresh_devices();
                    self.wgi_history = gilrs.devices();
                    wgi_count = self.wgi_history.len();
                }
                if wgi_count == 0 {
                    self.enter_xinput_fallback(now);
                } else {
                    self.authority = WindowsControllerAuthority::WgiAuthoritative;
                    self.publish(true);
                }
                wgi_poll.events.clear();
            }
            _ => {}
        }

        match self.authority {
            WindowsControllerAuthority::WgiAuthoritative => BackendPoll {
                boundary_reset,
                ..wgi_poll
            },
            WindowsControllerAuthority::WgiEmptyGrace { .. }
            | WindowsControllerAuthority::AwaitingFocusedWgi => BackendPoll {
                selected_disconnected: wgi_poll.selected_disconnected,
                devices_changed: wgi_poll.devices_changed,
                boundary_reset,
                ..BackendPoll::default()
            },
            WindowsControllerAuthority::XInputFallback => {
                let Some(xinput) = &mut self.xinput else {
                    return BackendPoll {
                        boundary_reset,
                        ..BackendPoll::default()
                    };
                };
                let selected_slot = selected
                    .filter(|key| key.backend == ControllerBackendKind::XInput)
                    .map(|key| key.runtime_id);
                let poll = xinput.poll(selected_slot, now);
                if poll.devices_changed {
                    self.publish(true);
                }
                BackendPoll {
                    events: poll.events,
                    selected_disconnected: poll.selected_disconnected,
                    devices_changed: poll.devices_changed,
                    boundary_reset,
                }
            }
        }
    }
}

impl ControllerBackendDriver for WindowsControllerBackend {
    fn devices(&self) -> &[ControllerDevice] {
        &self.devices
    }

    fn topology_generation(&self) -> u64 {
        self.topology_generation
    }

    fn focus_gained(&mut self, now: Instant) {
        if self.focused {
            return;
        }
        self.focused = true;
        self.authority = WindowsControllerAuthority::AwaitingFocusedWgi;
        self.retry.arm(now);
        self.publish(true);
    }

    fn focus_lost(&mut self) {
        if !self.focused {
            return;
        }
        self.focused = false;
        if let Some(gilrs) = &self.gilrs {
            self.wgi_history = gilrs.devices();
        }
        self.gilrs = None;
        if let Some(xinput) = &mut self.xinput {
            xinput.deactivate();
        }
        self.authority = WindowsControllerAuthority::AwaitingFocusedWgi;
        self.retry = WgiRetry::default();
        self.devices.clear();
        self.bump_topology();
    }

    fn poll(&mut self, selected: Option<ControllerRuntimeKey>, now: Instant) -> BackendPoll {
        if self.focused {
            self.poll_focused(selected, now)
        } else {
            BackendPoll::default()
        }
    }

    fn final_values(&mut self, runtime: ControllerRuntimeKey) -> Vec<HostControlValue> {
        match runtime.backend {
            ControllerBackendKind::Gilrs
                if matches!(self.authority, WindowsControllerAuthority::WgiAuthoritative) =>
            {
                self.gilrs
                    .as_mut()
                    .map_or_else(Vec::new, |gilrs| gilrs.final_values(runtime))
            }
            ControllerBackendKind::XInput
                if matches!(self.authority, WindowsControllerAuthority::XInputFallback) =>
            {
                self.xinput
                    .as_ref()
                    .map_or_else(Vec::new, |xinput| xinput.values(runtime.runtime_id))
            }
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
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
}
