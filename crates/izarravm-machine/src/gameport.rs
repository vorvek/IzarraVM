// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use izarravm_core::{CanonicalFieldWriter, CanonicalStateError, MASTER_CLOCK_HZ};

/// The 558 quad one-shot's fixed term: 24.2 us before the pot resistance is
/// counted at all.
const RC_BASE_NS: u64 = 24_200;
/// The pot's own contribution at full deflection. The 555/558 formula is
/// ~11 us per kOhm, and a PC joystick axis pot runs 0-100 kOhm, so the span is
/// 1,100 us and a centred axis lands near 0.57 ms. 86Box derives exactly that
/// (`src/game/gameport.c:269-273`: `axis * 100 / 65` ohms, then `* 11 / 1000`
/// us, then `+ 24`); DOSBox-X's `read_p201_timed` agrees within 20 %. The old
/// 2,750 us here implied a 250 kOhm pot and stretched every pulse 2.5x.
const RC_SPAN_NS: u64 = 1_100_000;

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
            // An empty connector is an open circuit on every line. The one-shots
            // cannot be triggered, so the axis bits stay pulled up and software
            // times out waiting for them to fall, and the button lines read
            // released for the same reason. 86Box returns 0xff with no joystick
            // (`src/game/gameport.c:307`) and DOSBox-X leaves the bits set. The
            // old 0xF0 here read as "all four axes fired instantly".
            return 0xff;
        };
        let mut value = 0xf0;
        value |= u8::from(now < self.discharge_deadlines[0]);
        value |= u8::from(now < self.discharge_deadlines[1]) << 1;
        // Compatibility behaviour, NOT hardware: while any one-shot is still
        // discharging the button lines read released. Real cards do not do this;
        // 86Box ships it (`src/game/gameport.c:315-317`, `if (state & 0x0f)
        // buttons = 0xf0;`) because software depends on it, so we ship it too.
        // Do not "fix" this back out.
        if value & 0x03 == 0 {
            value &= !((state.buttons & 0x03) << 4);
        }
        value
    }

    /// INT 15h AH=84h BX=0 reports the joystick switch settings in bits 4-7.
    /// With no stick attached those lines are open, so they read released --
    /// the same 1s `read` reports for the same physical reason, which is why
    /// the whole byte follows `read`'s absent-stick answer rather than keeping
    /// a separate 0xF0 convention. Bits 0-3 are not switch state on either
    /// path; a guest that masks the nibble, as every documented user does, sees
    /// no difference.
    pub(crate) fn bios_switches(&self) -> u8 {
        self.state
            .map_or(0xff, |state| 0xf0 & !((state.buttons & 0x03) << 4))
    }

    /// True when nothing about a `read` at `now` is time-dependent: either no
    /// stick is attached (so the one-shots can never fire and the deadlines are
    /// held at zero by `set_state`/`charge`) or both monostables have already
    /// discharged. The button lines are host-event state, not time state, so
    /// they are not part of this question.
    ///
    /// This is the whole precondition for serving a gameport read without
    /// ending the CPU batch; see the caller in `MachineBus::read_io`.
    pub(crate) fn is_idle(&self, now: u64) -> bool {
        self.state.is_none()
            || (now >= self.discharge_deadlines[0] && now >= self.discharge_deadlines[1])
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
