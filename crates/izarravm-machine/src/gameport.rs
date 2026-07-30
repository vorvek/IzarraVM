// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use izarravm_core::{CanonicalFieldWriter, CanonicalStateError, MASTER_CLOCK_HZ};

const RC_BASE_NS: u64 = 24_200;
const RC_SPAN_NS: u64 = 2_750_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct JoystickState {
    pub x: u8,
    pub y: u8,
    /// Bits 0 and 1 are set while joystick A buttons 1 and 2 are pressed.
    pub buttons: u8,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct GamePort {
    state: Option<JoystickState>,
    discharge_deadlines: [u64; 2],
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CanonicalGamePort<'a> {
    gameport: &'a GamePort,
}

impl GamePort {
    pub(crate) fn set_state(&mut self, state: Option<JoystickState>) {
        self.state = state.map(|state| JoystickState {
            buttons: state.buttons & 0x03,
            ..state
        });
        if self.state.is_none() {
            self.discharge_deadlines = [0; 2];
        }
    }

    pub(crate) fn charge(&mut self, now: u64) {
        let Some(state) = self.state else {
            self.discharge_deadlines = [0; 2];
            return;
        };
        self.discharge_deadlines = [
            now.saturating_add(axis_ticks(state.x)),
            now.saturating_add(axis_ticks(state.y)),
        ];
    }

    pub(crate) fn read(&self, now: u64) -> u8 {
        let Some(state) = self.state else {
            return 0xf0;
        };
        let mut value = 0xf0;
        value |= u8::from(now < self.discharge_deadlines[0]);
        value |= u8::from(now < self.discharge_deadlines[1]) << 1;
        value &= !((state.buttons & 0x03) << 4);
        value
    }

    pub(crate) fn bios_switches(&self) -> u8 {
        self.state
            .map_or(0xf0, |state| 0xf0 & !((state.buttons & 0x03) << 4))
    }

    pub(crate) fn bios_axes(&self) -> (u16, u16) {
        self.state
            .map(|state| (u16::from(state.x), u16::from(state.y)))
            .unwrap_or((0, 0))
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
        out.write_u8(state.x)?;
        out.write_u8(state.y)?;
        out.write_u8(state.buttons)?;
        out.write_u64(self.gameport.discharge_deadlines[0])?;
        out.write_u64(self.gameport.discharge_deadlines[1])
    }
}

#[cfg(test)]
#[path = "gameport_test.rs"]
mod tests;
