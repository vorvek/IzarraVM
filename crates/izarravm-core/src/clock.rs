// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

/// An exact clock rate in hertz, represented as an integer ratio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClockRate {
    numerator_hz: u64,
    denominator: u64,
}

/// Fixed machine-timeline frequency. Every supported GSW CPU clock is an
/// integer number of these ticks.
pub const MASTER_CLOCK_HZ: u64 = 6_600_000_000;

const fn saturating_u64(value: u128) -> u64 {
    if value > u64::MAX as u128 {
        u64::MAX
    } else {
        value as u64
    }
}

impl ClockRate {
    pub const fn new(numerator_hz: u64, denominator: u64) -> Self {
        assert!(numerator_hz != 0, "clock-rate numerator must not be zero");
        assert!(denominator != 0, "clock-rate denominator must not be zero");
        Self {
            numerator_hz,
            denominator,
        }
    }

    pub const fn from_hz(hz: u64) -> Self {
        Self::new(hz, 1)
    }

    pub const fn numerator_hz(self) -> u64 {
        self.numerator_hz
    }

    pub const fn denominator(self) -> u64 {
        self.denominator
    }

    pub fn as_hz_f64(self) -> f64 {
        self.numerator_hz as f64 / self.denominator as f64
    }

    pub fn seconds_for_clocks(self, clocks: u64) -> f64 {
        clocks as f64 * self.denominator as f64 / self.numerator_hz as f64
    }

    /// Exact master ticks in one clock, or `None` when this rate does not divide
    /// the fixed machine timeline evenly.
    pub const fn master_ticks_per_clock(self) -> Option<u64> {
        let scaled = MASTER_CLOCK_HZ as u128 * self.denominator as u128;
        let divisor = self.numerator_hz as u128;
        if !scaled.is_multiple_of(divisor) {
            return None;
        }
        let ticks = scaled / divisor;
        if ticks > u64::MAX as u128 {
            None
        } else {
            Some(ticks as u64)
        }
    }

    /// Master ticks for `clocks`, rounded down and saturated to `u64`.
    pub const fn master_ticks_for_clocks_floor(self, clocks: u64) -> u64 {
        let per_clock = MASTER_CLOCK_HZ as u128 * self.denominator as u128;
        match per_clock.checked_mul(clocks as u128) {
            Some(scaled) => saturating_u64(scaled / self.numerator_hz as u128),
            None => u64::MAX,
        }
    }

    /// Master ticks for `clocks`, rounded up to the earliest causal timeline
    /// tick and saturated to `u64`.
    pub const fn master_ticks_for_clocks_ceil(self, clocks: u64) -> u64 {
        let per_clock = MASTER_CLOCK_HZ as u128 * self.denominator as u128;
        match per_clock.checked_mul(clocks as u128) {
            Some(scaled) => saturating_u64(scaled.div_ceil(self.numerator_hz as u128)),
            None => u64::MAX,
        }
    }

    /// Whole clocks contained in `master_ticks`, rounded down.
    pub const fn clocks_for_master_ticks_floor(self, master_ticks: u64) -> u64 {
        let scaled = master_ticks as u128 * self.numerator_hz as u128;
        let divisor = MASTER_CLOCK_HZ as u128 * self.denominator as u128;
        saturating_u64(scaled / divisor)
    }

    /// First clock at or after `master_ticks`. This is the causal inverse used
    /// for deadlines that fall between two CPU clocks.
    pub const fn clocks_for_master_ticks_ceil(self, master_ticks: u64) -> u64 {
        let scaled = master_ticks as u128 * self.numerator_hz as u128;
        let divisor = MASTER_CLOCK_HZ as u128 * self.denominator as u128;
        saturating_u64(scaled.div_ceil(divisor))
    }

    /// Convert a rational number of seconds to clocks, rounding down. Rounding
    /// down chooses the first clock inside a deadline interval.
    pub const fn clocks_for_fraction_floor(
        self,
        seconds_numerator: u64,
        seconds_denominator: u64,
    ) -> u64 {
        assert!(
            seconds_denominator != 0,
            "seconds denominator must not be zero"
        );
        let clocks = (self.numerator_hz as u128 * seconds_numerator as u128)
            / (self.denominator as u128 * seconds_denominator as u128);
        saturating_u64(clocks)
    }

    /// Whole hertz, rounded down. Compatibility callers that require an integer
    /// clock should use this; timing code should retain the full ratio.
    pub const fn floor_hz(self) -> u64 {
        self.numerator_hz / self.denominator
    }
}

#[cfg(test)]
#[path = "clock_test.rs"]
mod tests;
