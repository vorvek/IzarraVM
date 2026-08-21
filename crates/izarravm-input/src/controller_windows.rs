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
#[path = "controller_windows_test.rs"]
mod tests;
