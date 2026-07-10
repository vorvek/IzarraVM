// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use izarravm_core::{GswMode, MASTER_CLOCK_HZ};

const fn saturating_u64(value: u128) -> u64 {
    if value > u64::MAX as u128 {
        u64::MAX
    } else {
        value as u64
    }
}

/// Fractional progress for a device clock measured against the fixed machine
/// timeline. The remainder is always less than [`MASTER_CLOCK_HZ`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RatePhase {
    remainder: u64,
}

impl RatePhase {
    pub fn with_remainder(remainder: u64) -> Self {
        assert!(
            remainder < MASTER_CLOCK_HZ,
            "rate-phase remainder must be below the master clock"
        );
        Self { remainder }
    }

    pub const fn remainder(self) -> u64 {
        self.remainder
    }

    /// Advance an integer-Hz device clock by `master_ticks`, returning the whole
    /// device events now due. Splitting one advance into batches gives the same
    /// event total and final remainder.
    pub fn advance(&mut self, master_ticks: u64, rate_hz: u64) -> u64 {
        let total = self.remainder as u128 + master_ticks as u128 * rate_hz as u128;
        self.remainder = (total % MASTER_CLOCK_HZ as u128) as u64;
        saturating_u64(total / MASTER_CLOCK_HZ as u128)
    }

    /// Earliest master-tick delta at which `events` device events are due.
    /// Returns `None` for a stopped device clock.
    pub fn ticks_until(self, events: u64, rate_hz: u64) -> Option<u64> {
        if events == 0 {
            return Some(0);
        }
        if rate_hz == 0 {
            return None;
        }
        let needed = events as u128 * MASTER_CLOCK_HZ as u128 - self.remainder as u128;
        Some(saturating_u64(needed.div_ceil(rate_hz as u128)))
    }
}

/// Mode-aware bookkeeping for the fixed machine timeline. Device advancement
/// will move behind this Module in the follow-up migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeline {
    now_ticks: u64,
    io_stall_ticks: u64,
    mode: GswMode,
    ticks_per_cpu_clock: u64,
}

impl Timeline {
    pub fn new(mode: GswMode) -> Self {
        Self {
            now_ticks: 0,
            io_stall_ticks: 0,
            mode,
            ticks_per_cpu_clock: exact_cpu_quantum(mode),
        }
    }

    pub const fn now_ticks(self) -> u64 {
        self.now_ticks
    }

    pub const fn io_stall_ticks(self) -> u64 {
        self.io_stall_ticks
    }

    pub const fn mode(self) -> GswMode {
        self.mode
    }

    pub const fn ticks_per_cpu_clock(self) -> u64 {
        self.ticks_per_cpu_clock
    }

    pub fn set_mode(&mut self, mode: GswMode) {
        self.mode = mode;
        self.ticks_per_cpu_clock = exact_cpu_quantum(mode);
    }

    pub fn master_ticks_for_cpu_clocks(self, cpu_clocks: u64) -> u64 {
        saturating_u64(cpu_clocks as u128 * self.ticks_per_cpu_clock as u128)
    }

    pub fn cpu_clocks_for_master_ticks_floor(self, master_ticks: u64) -> u64 {
        master_ticks / self.ticks_per_cpu_clock
    }

    pub fn cpu_clocks_for_master_ticks_ceil(self, master_ticks: u64) -> u64 {
        saturating_u64((master_ticks as u128).div_ceil(self.ticks_per_cpu_clock as u128))
    }

    pub fn advance_ticks(&mut self, master_ticks: u64) {
        self.now_ticks = saturating_u64(self.now_ticks as u128 + master_ticks as u128);
    }

    pub fn advance_cpu_clocks(&mut self, cpu_clocks: u64) {
        self.advance_ticks(self.master_ticks_for_cpu_clocks(cpu_clocks));
    }

    pub fn advance_io_stall_ticks(&mut self, master_ticks: u64) {
        self.advance_ticks(master_ticks);
        self.io_stall_ticks = saturating_u64(self.io_stall_ticks as u128 + master_ticks as u128);
    }
}

impl Default for Timeline {
    fn default() -> Self {
        Self::new(GswMode::default())
    }
}

fn exact_cpu_quantum(mode: GswMode) -> u64 {
    mode.clock_rate()
        .master_ticks_per_clock()
        .expect("every GSW CPU rate must divide the master clock exactly")
}

#[cfg(test)]
#[path = "timeline_test.rs"]
mod tests;
