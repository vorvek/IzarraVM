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
fn master_clock_exactly_represents_every_gsw_rate() {
    for (rate, expected) in [
        (ClockRate::from_hz(166_000_000), 33),
        (ClockRate::from_hz(66_000_000), 83),
        (ClockRate::from_hz(22_000_000), 249),
        (ClockRate::new(22_000_000, 3), 747),
    ] {
        assert_eq!(rate.master_ticks_per_clock(), Some(expected));
        assert_eq!(rate.master_ticks_for_clocks_floor(1), expected);
        assert_eq!(rate.master_ticks_for_clocks_ceil(1), expected);
    }

    assert_eq!(ClockRate::from_hz(44_100).master_ticks_per_clock(), None);
}

#[test]
fn master_clock_inverse_uses_the_earliest_causal_clock() {
    let rate = ClockRate::from_hz(166_000_000);

    assert_eq!(rate.clocks_for_master_ticks_floor(32), 0);
    assert_eq!(rate.clocks_for_master_ticks_ceil(32), 1);
    assert_eq!(rate.clocks_for_master_ticks_floor(33), 1);
    assert_eq!(rate.clocks_for_master_ticks_ceil(33), 1);
    assert_eq!(rate.clocks_for_master_ticks_floor(34), 1);
    assert_eq!(rate.clocks_for_master_ticks_ceil(34), 2);
}

#[test]
fn master_clock_conversions_saturate_after_u128_arithmetic() {
    let slow = ClockRate::new(22_000_000, 3);
    assert_eq!(slow.master_ticks_for_clocks_floor(u64::MAX), u64::MAX);
    assert_eq!(slow.master_ticks_for_clocks_ceil(u64::MAX), u64::MAX);

    let largest_denominator = ClockRate::new(1, u64::MAX);
    assert_eq!(
        largest_denominator.master_ticks_for_clocks_floor(u64::MAX),
        u64::MAX
    );
    assert_eq!(
        largest_denominator.master_ticks_for_clocks_ceil(u64::MAX),
        u64::MAX
    );

    let fastest_ratio = ClockRate::from_hz(u64::MAX);
    assert_eq!(
        fastest_ratio.clocks_for_master_ticks_floor(u64::MAX),
        u64::MAX
    );
    assert_eq!(
        fastest_ratio.clocks_for_master_ticks_ceil(u64::MAX),
        u64::MAX
    );
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
