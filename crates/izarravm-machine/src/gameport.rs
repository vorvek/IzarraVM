// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use izarravm_core::{CanonicalFieldWriter, CanonicalStateError, MASTER_CLOCK_HZ};

const RC_BASE_NS: u64 = 24_200;
const RC_SPAN_NS: u64 = 1_100_000;
const BUTTON_REPLAY_CAPACITY: usize = 8;
const BUTTON_MIN_DWELL_TICKS: u64 = MASTER_CLOCK_HZ / 1_000;
const TURBO_HALF_PERIOD_TICKS: u64 = MASTER_CLOCK_HZ / 20;

const _: () = assert!(MASTER_CLOCK_HZ.is_multiple_of(20));

pub(crate) const BIOS_AXIS_TIMEOUT: u16 = u16::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct JoystickState {
    pub axes: [u8; 4],
    /// Bits 0 through 3 identify electrically connected axis lines.
    pub axis_present: u8,
    /// Normal held sources for button lines 1 through 4.
    pub buttons: u8,
    /// Autofire held sources for button lines 1 through 4.
    pub turbo_buttons: u8,
}

impl JoystickState {
    pub fn joystick_a(x: u8, y: u8, buttons: u8) -> Self {
        Self {
            axes: [x, y, 0, 0],
            axis_present: 0x03,
            buttons: buttons & 0x0f,
            turbo_buttons: 0,
        }
    }

    fn sanitized(mut self) -> Self {
        self.axis_present &= 0x0f;
        self.buttons &= 0x0f;
        self.turbo_buttons &= 0x0f;
        self
    }

    fn button_drive(self, line: usize) -> ButtonDriveState {
        let bit = 1 << line;
        ButtonDriveState {
            normal_held: self.buttons & bit != 0,
            turbo_held: self.turbo_buttons & bit != 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GamePortButtonTransition {
    pub line: u8,
    pub normal_held: bool,
    pub turbo_held: bool,
}

impl GamePortButtonTransition {
    fn drive(self) -> ButtonDriveState {
        ButtonDriveState {
            normal_held: self.normal_held,
            turbo_held: self.turbo_held,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GamePortUpdate {
    pub state: Option<JoystickState>,
    pub button_transitions: Vec<GamePortButtonTransition>,
    /// Flushes obsolete replay before a disconnect or profile change.
    pub reset_replay: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct ButtonDriveState {
    normal_held: bool,
    turbo_held: bool,
}

#[derive(Debug, Clone, Copy)]
struct ButtonReplayLine {
    queue: [ButtonDriveState; BUTTON_REPLAY_CAPACITY],
    head: u8,
    len: u8,
    current: ButtonDriveState,
    target: ButtonDriveState,
    next_transition_tick: u64,
    turbo_epoch: u64,
}

impl Default for ButtonReplayLine {
    fn default() -> Self {
        Self {
            queue: [ButtonDriveState::default(); BUTTON_REPLAY_CAPACITY],
            head: 0,
            len: 0,
            current: ButtonDriveState::default(),
            target: ButtonDriveState::default(),
            next_transition_tick: 0,
            turbo_epoch: 0,
        }
    }
}

impl ButtonReplayLine {
    fn reset(&mut self, target: ButtonDriveState, now: u64, attached: bool) {
        *self = Self::default();
        if !attached || target == ButtonDriveState::default() {
            return;
        }
        self.target = target;
        self.next_transition_tick = now.saturating_add(BUTTON_MIN_DWELL_TICKS);
        self.push_unchecked(target);
    }

    fn update_target(
        &mut self,
        transitions: impl Iterator<Item = ButtonDriveState>,
        target: ButtonDriveState,
        now: u64,
    ) {
        self.advance(now);
        if self.len == 0 && self.next_transition_tick < now {
            self.next_transition_tick = now;
        }
        self.target = target;
        for transition in transitions {
            if self.enqueue(transition) {
                self.advance(now);
                return;
            }
        }
        let _ = self.enqueue(target);
        self.advance(now);
    }

    /// Returns true when intermediate history overflowed and was replaced by target.
    fn enqueue(&mut self, drive: ButtonDriveState) -> bool {
        if self.last_scheduled() == drive {
            return false;
        }
        if usize::from(self.len) == BUTTON_REPLAY_CAPACITY {
            self.head = 0;
            self.len = 0;
            self.queue = [ButtonDriveState::default(); BUTTON_REPLAY_CAPACITY];
            if self.current != self.target {
                self.push_unchecked(self.target);
            }
            return true;
        }
        self.push_unchecked(drive);
        false
    }

    fn push_unchecked(&mut self, drive: ButtonDriveState) {
        let tail = (usize::from(self.head) + usize::from(self.len)) % BUTTON_REPLAY_CAPACITY;
        self.queue[tail] = drive;
        self.len += 1;
    }

    fn pop(&mut self) -> ButtonDriveState {
        let index = usize::from(self.head);
        let drive = self.queue[index];
        self.queue[index] = ButtonDriveState::default();
        self.head = ((index + 1) % BUTTON_REPLAY_CAPACITY) as u8;
        self.len -= 1;
        drive
    }

    fn last_scheduled(&self) -> ButtonDriveState {
        if self.len == 0 {
            self.current
        } else {
            let index =
                (usize::from(self.head) + usize::from(self.len) - 1) % BUTTON_REPLAY_CAPACITY;
            self.queue[index]
        }
    }

    fn advance(&mut self, now: u64) {
        while self.len != 0 && self.next_transition_tick <= now {
            let at = if self.next_transition_tick == 0 {
                now
            } else {
                self.next_transition_tick
            };
            let next = self.pop();
            if !self.current.turbo_held && next.turbo_held {
                self.turbo_epoch = at;
            } else if !next.turbo_held {
                self.turbo_epoch = 0;
            }
            self.current = next;
            self.next_transition_tick = at.saturating_add(BUTTON_MIN_DWELL_TICKS);
        }
    }

    fn pressed(&self, now: u64) -> bool {
        self.current.normal_held
            || (self.current.turbo_held
                && (now.saturating_sub(self.turbo_epoch) / TURBO_HALF_PERIOD_TICKS) & 1 == 0)
    }

    fn time_dependent(&self) -> bool {
        self.len != 0 || self.current.turbo_held
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct GamePort {
    state: Option<JoystickState>,
    discharge_deadlines: [u64; 4],
    button_lines: [ButtonReplayLine; 4],
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CanonicalGamePort<'a> {
    gameport: &'a GamePort,
}

impl GamePort {
    pub(crate) fn set_state(&mut self, state: Option<JoystickState>, now: u64) {
        let state = state.map(JoystickState::sanitized);
        self.state = state;
        if let Some(state) = state {
            for line in 0..4 {
                let target = state.button_drive(line);
                self.button_lines[line] = ButtonReplayLine {
                    current: target,
                    target,
                    next_transition_tick: now.saturating_add(BUTTON_MIN_DWELL_TICKS),
                    turbo_epoch: if target.turbo_held { now } else { 0 },
                    ..ButtonReplayLine::default()
                };
            }
        } else {
            self.discharge_deadlines = [0; 4];
            self.button_lines = [ButtonReplayLine::default(); 4];
        }
    }

    pub(crate) fn apply_update(&mut self, update: GamePortUpdate, now: u64) {
        let state = update.state.map(JoystickState::sanitized);
        if state.is_none() {
            self.state = None;
            self.discharge_deadlines = [0; 4];
            self.button_lines = [ButtonReplayLine::default(); 4];
            return;
        }

        let state = state.expect("checked attached state");
        let profile_changed = self
            .state
            .is_some_and(|old| old.axis_present != state.axis_present);
        self.state = Some(state);

        for line in 0..4 {
            let target = state.button_drive(line);
            if update.reset_replay || profile_changed {
                self.button_lines[line].reset(target, now, true);
                continue;
            }
            let transitions = update
                .button_transitions
                .iter()
                .copied()
                .filter(move |transition| usize::from(transition.line) == line)
                .map(GamePortButtonTransition::drive);
            self.button_lines[line].update_target(transitions, target, now);
        }
    }

    pub(crate) fn charge(&mut self, now: u64) {
        let Some(state) = self.state else {
            self.discharge_deadlines = [0; 4];
            return;
        };
        for (axis, deadline) in self.discharge_deadlines.iter_mut().enumerate() {
            *deadline = if state.axis_present & (1 << axis) != 0 {
                now.saturating_add(axis_ticks(state.axes[axis]))
            } else {
                0
            };
        }
    }

    pub(crate) fn read(&mut self, now: u64) -> u8 {
        let Some(state) = self.state else {
            return 0xff;
        };
        for line in &mut self.button_lines {
            line.advance(now);
        }

        let mut axes = !state.axis_present & 0x0f;
        for axis in 0..4 {
            if state.axis_present & (1 << axis) != 0 && now < self.discharge_deadlines[axis] {
                axes |= 1 << axis;
            }
        }
        let mut value = 0xf0 | axes;
        if axes & state.axis_present == 0 {
            for (line, replay) in self.button_lines.iter().enumerate() {
                if replay.pressed(now) {
                    value &= !(1 << (line + 4));
                }
            }
        }
        value
    }

    pub(crate) fn bios_switches(&mut self, now: u64) -> u8 {
        if self.state.is_none() {
            return 0xff;
        }
        for line in &mut self.button_lines {
            line.advance(now);
        }
        let mut value = 0xf0;
        for (line, replay) in self.button_lines.iter().enumerate() {
            if replay.pressed(now) {
                value &= !(1 << (line + 4));
            }
        }
        value
    }

    pub(crate) fn is_idle(&self, now: u64) -> bool {
        if self.state.is_none() {
            return true;
        }
        let axes_idle = self
            .discharge_deadlines
            .iter()
            .all(|deadline| now >= *deadline);
        axes_idle && self.button_lines.iter().all(|line| !line.time_dependent())
    }

    pub(crate) fn bios_axes(&self) -> [u16; 4] {
        let Some(state) = self.state else {
            return [BIOS_AXIS_TIMEOUT, BIOS_AXIS_TIMEOUT, 0, 0];
        };
        std::array::from_fn(|axis| {
            if state.axis_present & (1 << axis) != 0 {
                u16::from(state.axes[axis])
            } else if axis < 2 {
                BIOS_AXIS_TIMEOUT
            } else {
                0
            }
        })
    }

    pub(crate) fn canonical_projection(&self) -> CanonicalGamePort<'_> {
        CanonicalGamePort { gameport: self }
    }
}

fn axis_ticks(axis: u8) -> u64 {
    let ns = RC_BASE_NS + RC_SPAN_NS * u64::from(axis) / u64::from(u8::MAX);
    (u128::from(ns) * u128::from(MASTER_CLOCK_HZ))
        .div_ceil(1_000_000_000)
        .min(u128::from(u64::MAX)) as u64
}

impl CanonicalGamePort<'_> {
    pub(crate) fn write_payload(
        &self,
        out: &mut CanonicalFieldWriter<'_>,
    ) -> Result<(), CanonicalStateError> {
        let state = self.gameport.state.unwrap_or_default();
        out.write_bool(self.gameport.state.is_some())?;
        out.write_u8(state.axis_present)?;
        for axis in state.axes {
            out.write_u8(axis)?;
        }
        out.write_u8(state.buttons)?;
        out.write_u8(state.turbo_buttons)?;
        for deadline in self.gameport.discharge_deadlines {
            out.write_u64(deadline)?;
        }
        for line in &self.gameport.button_lines {
            out.write_u8(line.head)?;
            out.write_u8(line.len)?;
            for entry in line.queue {
                out.write_bool(entry.normal_held)?;
                out.write_bool(entry.turbo_held)?;
            }
            out.write_bool(line.current.normal_held)?;
            out.write_bool(line.current.turbo_held)?;
            out.write_bool(line.target.normal_held)?;
            out.write_bool(line.target.turbo_held)?;
            out.write_u64(line.next_transition_tick)?;
            out.write_u64(line.turbo_epoch)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "gameport_test.rs"]
mod tests;
