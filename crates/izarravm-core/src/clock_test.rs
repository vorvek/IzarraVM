// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn rational_conversions_keep_the_slow_clock_exact() {
    let rate = ClockRate::new(22_000_000, 3);

    assert_eq!(rate.floor_hz(), 7_333_333);
    assert!((rate.as_hz_f64() - 7_333_333.333_333_333).abs() < 0.001);
    assert_eq!(rate.seconds_for_clocks(22_000_000), 3.0);
    assert_eq!(rate.clocks_for_fraction_floor(3, 1), 22_000_000);
    assert_eq!(rate.clocks_for_fraction_floor(1, 1), 7_333_333);
    assert_eq!(rate.clocks_for_fraction_floor(1, 3), 2_444_444);
}

#[test]
fn rational_budget_overflow_saturates() {
    let rate = ClockRate::from_hz(u64::MAX);
    assert_eq!(rate.clocks_for_fraction_floor(u64::MAX, 1), u64::MAX);
}

#[test]
#[should_panic(expected = "clock-rate numerator must not be zero")]
fn zero_clock_is_rejected() {
    let _ = ClockRate::from_hz(0);
}

#[test]
#[should_panic(expected = "seconds denominator must not be zero")]
fn zero_seconds_denominator_is_rejected() {
    let _ = ClockRate::from_hz(1).clocks_for_fraction_floor(1, 0);
}
