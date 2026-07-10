// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

/// An exact clock rate in hertz, represented as an integer ratio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClockRate {
    numerator_hz: u64,
    denominator: u64,
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
        if clocks > u64::MAX as u128 {
            u64::MAX
        } else {
            clocks as u64
        }
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
