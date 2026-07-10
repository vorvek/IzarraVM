// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn timeline_uses_the_exact_quantum_for_every_mode() {
    for (mode, expected) in [
        (GswMode::Gsw586, 33),
        (GswMode::Gsw486, 100),
        (GswMode::Gsw386, 300),
        (GswMode::Gsw386Slow, 900),
    ] {
        let timeline = Timeline::new(mode);
        assert_eq!(timeline.mode(), mode);
        assert_eq!(timeline.ticks_per_cpu_clock(), expected);
        assert_eq!(timeline.master_ticks_for_cpu_clocks(10), expected * 10);
    }
}

#[test]
fn mode_switch_preserves_elapsed_and_stall_time() {
    let mut timeline = Timeline::new(GswMode::Gsw586);
    timeline.advance_cpu_clocks(10);
    timeline.advance_io_stall_ticks(70);

    timeline.set_mode(GswMode::Gsw386Slow);

    assert_eq!(timeline.now_ticks(), 400);
    assert_eq!(timeline.io_stall_ticks(), 70);
    assert_eq!(timeline.ticks_per_cpu_clock(), 900);
    timeline.advance_cpu_clocks(1);
    assert_eq!(timeline.now_ticks(), 1_300);
}

#[test]
fn rate_phase_is_batch_invariant() {
    let total_ticks = 123_456_789;
    let chunks = [1, 17, 31_337, 5_000_000, 90_000_000, 28_425_434];
    assert_eq!(chunks.into_iter().sum::<u64>(), total_ticks);

    for rate in [
        1,
        44_100,
        49_716,
        1_193_182,
        25_175_000,
        28_322_000,
        1_000_000_000,
    ] {
        let mut whole = RatePhase::default();
        let whole_events = whole.advance(total_ticks, rate);

        let mut split = RatePhase::default();
        let split_events = chunks
            .into_iter()
            .map(|chunk| split.advance(chunk, rate))
            .sum::<u64>();

        assert_eq!(split_events, whole_events, "rate {rate}");
        assert_eq!(split, whole, "rate {rate}");
    }
}

#[test]
fn rate_phase_deadline_is_the_first_tick_that_produces_the_event() {
    for rate in [44_100, 49_716, 1_193_182, 28_322_000] {
        let phase = RatePhase::default();
        let deadline = phase.ticks_until(1, rate).unwrap();
        assert!(deadline > 0);

        let mut before = phase;
        assert_eq!(before.advance(deadline - 1, rate), 0, "rate {rate}");
        let mut at = phase;
        assert_eq!(at.advance(deadline, rate), 1, "rate {rate}");
    }

    let almost_due = RatePhase::with_remainder(MASTER_CLOCK_HZ - 1);
    assert_eq!(almost_due.ticks_until(1, 1), Some(1));
    assert_eq!(RatePhase::default().ticks_until(1, 0), None);
}

#[test]
fn timeline_and_rate_phase_saturate_after_wide_arithmetic() {
    let mut timeline = Timeline::new(GswMode::Gsw386Slow);
    assert_eq!(timeline.master_ticks_for_cpu_clocks(u64::MAX), u64::MAX);
    timeline.advance_cpu_clocks(u64::MAX);
    timeline.advance_ticks(1);
    assert_eq!(timeline.now_ticks(), u64::MAX);

    timeline.advance_io_stall_ticks(u64::MAX);
    assert_eq!(timeline.io_stall_ticks(), u64::MAX);
    assert_eq!(timeline.now_ticks(), u64::MAX);

    let mut phase = RatePhase::with_remainder(MASTER_CLOCK_HZ - 1);
    assert_eq!(phase.advance(u64::MAX, u64::MAX), u64::MAX);
    assert!(phase.remainder() < MASTER_CLOCK_HZ);
    assert_eq!(phase.ticks_until(u64::MAX, 1), Some(u64::MAX));
}

#[test]
fn cpu_clock_inverse_has_causal_and_budget_forms() {
    let timeline = Timeline::new(GswMode::Gsw386);
    assert_eq!(timeline.cpu_clocks_for_master_ticks_floor(299), 0);
    assert_eq!(timeline.cpu_clocks_for_master_ticks_ceil(299), 1);
    assert_eq!(timeline.cpu_clocks_for_master_ticks_floor(300), 1);
    assert_eq!(timeline.cpu_clocks_for_master_ticks_ceil(300), 1);
    assert_eq!(timeline.cpu_clocks_for_master_ticks_floor(301), 1);
    assert_eq!(timeline.cpu_clocks_for_master_ticks_ceil(301), 2);
}
