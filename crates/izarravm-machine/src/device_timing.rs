// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! `IZARRAVM_DEVICE_TIMING`, the slice-9 device-timing knob.
//!
//! Slice 9 (`dev_docs/2026-09-05-device-timing-slice9-design.md`) lives on the
//! **deadline lane** (`MASTER_CLOCK_HZ` ticks: *when* a device event fires),
//! never the charge lane (`guest_clocks`, what rt divides). Nothing this knob
//! controls may touch `level_timing`, `bus_timing`, `wait_states.io` or any
//! `clocks(N)` literal -- see the design's §3.3 point 4.
//!
//! `DeviceTimingProfile` is parsed exactly ONCE, at `Machine::new`
//! (`default-off-instruments-tax-hot-path`: never re-read `std::env` on a hot
//! path), and copied by value into every device that needs it. It is `Copy`
//! for that reason.
//!
//! **Unset or empty is today's behaviour, bit-identical.** Per
//! `parameter-knobs-have-no-off-spelling`, there is deliberately no `=0`
//! spelling -- the "off" arm for a *family list* is the empty list, which is
//! also what unset and `""` both parse to.
//!
//! ```text
//! IZARRAVM_DEVICE_TIMING=period        # every family armed
//! IZARRAVM_DEVICE_TIMING=ata,cd        # only these two families armed
//! ```
//!
//! Families, exactly as the design's §3.1 names them: `pic dma ata cd fdc kbc
//! sb`. `period` is a whole-profile alias, not a family name of its own --
//! `IZARRAVM_DEVICE_TIMING=period,ata` is redundant, not an error, since
//! `period` already implies every family.
//!
//! Slice 9-0 adds ONLY the knob and the identity fixture. No constant reads
//! this profile yet; a family flag armed here changes nothing until a later
//! slice (9A, 9B, ...) adds the code path that consults it. That is
//! deliberate: `9-0`'s certifier is that the knob-unset machine is
//! byte-identical to a machine built before this file existed.

/// Per-family armed/disarmed state for the slice-9 device-timing constants.
/// `Copy` so every device that needs it gets its own value, never a borrow of
/// `Machine`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct DeviceTimingProfile {
    pub(crate) ata: bool,
    pub(crate) cd: bool,
    pub(crate) dma: bool,
    pub(crate) pic: bool,
    pub(crate) kbc: bool,
    pub(crate) sb: bool,
    pub(crate) fdc: bool,
}

impl DeviceTimingProfile {
    /// The knob-unset arm: every family disarmed, today's behaviour exactly.
    const fn none() -> Self {
        Self {
            ata: false,
            cd: false,
            dma: false,
            pic: false,
            kbc: false,
            sb: false,
            fdc: false,
        }
    }

    /// `period`: every family armed.
    const fn all() -> Self {
        Self {
            ata: true,
            cd: true,
            dma: true,
            pic: true,
            kbc: true,
            sb: true,
            fdc: true,
        }
    }

    /// True when no family is armed -- the knob-unset identity arm, and the
    /// one `9-0`'s fixture certifies.
    // Limit: no production caller yet. Exercised directly by
    // `device_timing_test.rs`; a later slice's JSON diagnostics emission is
    // the natural first production caller.
    #[allow(dead_code)]
    pub(crate) fn is_none(&self) -> bool {
        *self == Self::none()
    }

    fn set_family(&mut self, name: &str) -> bool {
        match name {
            "ata" => self.ata = true,
            "cd" => self.cd = true,
            "dma" => self.dma = true,
            "pic" => self.pic = true,
            "kbc" => self.kbc = true,
            "sb" => self.sb = true,
            "fdc" => self.fdc = true,
            _ => return false,
        }
        true
    }
}

/// Read `IZARRAVM_DEVICE_TIMING` and parse it exactly once. Call this ONLY at
/// `Machine::new`; every device sees the returned value by copy from then on.
pub(crate) fn device_timing_profile_default() -> DeviceTimingProfile {
    parse_device_timing_profile(std::env::var("IZARRAVM_DEVICE_TIMING"))
}

fn parse_device_timing_profile(value: Result<String, std::env::VarError>) -> DeviceTimingProfile {
    let raw = match value {
        Err(std::env::VarError::NotPresent) => return DeviceTimingProfile::none(),
        // Not-UTF-8 is not a spelling of any arm -- someone set the variable
        // and meant something by it, so this must not fall back to silence.
        Err(std::env::VarError::NotUnicode(_)) => panic!(
            "IZARRAVM_DEVICE_TIMING is set to a value that is not valid UTF-8; accepted \
             spellings are unset or empty (today's behaviour, no family armed), `period` (every \
             family armed), or a comma-separated family list drawn from: \
             pic dma ata cd fdc kbc sb"
        ),
        Ok(raw) => raw,
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return DeviceTimingProfile::none();
    }
    if trimmed.eq_ignore_ascii_case("period") {
        return DeviceTimingProfile::all();
    }
    let mut profile = DeviceTimingProfile::none();
    for family in trimmed.split(',') {
        let family = family.trim();
        if family.is_empty() {
            // A stray comma (leading, trailing, doubled) names no family and
            // is not itself a typo -- treat it as a separator artifact rather
            // than panicking on it, matching a permissive comma-list split
            // elsewhere in the codebase (`Resolve-KnobPassthrough`-adjacent
            // Rust-side parsing has no precedent to follow more strictly).
            continue;
        }
        if family.eq_ignore_ascii_case("period") {
            // "ata,period" etc: redundant, not an error, since period already
            // implies every family.
            return DeviceTimingProfile::all();
        }
        if !profile.set_family(&family.to_ascii_lowercase()) {
            panic!(
                "IZARRAVM_DEVICE_TIMING names an unknown family {family:?}; accepted spellings \
                 are unset or empty (today's behaviour), `period` (every family), or a \
                 comma-separated list drawn from: pic dma ata cd fdc kbc sb. Refusing to guess: \
                 a mistyped family would silently run with that family disarmed and be read as \
                 \"the family I asked for changed nothing\""
            );
        }
    }
    profile
}

#[cfg(test)]
#[path = "device_timing_test.rs"]
mod tests;
