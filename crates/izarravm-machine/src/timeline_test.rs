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
        assert_eq!(timeline.ticks_per_cpu_clock(), expected);
        assert_eq!(timeline.master_ticks_for_cpu_clocks(10), expected * 10);
    }
}

#[test]
fn mode_switch_preserves_elapsed_and_stall_time() {
    let mut timeline = Timeline::new(GswMode::Gsw586);
    timeline.advance_cpu_clocks(10, DeviceRates::default());
    timeline.advance_io_stall_ticks(70, DeviceRates::default());

    timeline.set_mode(GswMode::Gsw386Slow);

    assert_eq!(timeline.now_ticks(), 400);
    assert_eq!(timeline.io_stall_ticks(), 70);
    assert_eq!(timeline.ticks_per_cpu_clock(), 900);
    timeline.advance_cpu_clocks(1, DeviceRates::default());
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
    timeline.advance_cpu_clocks(u64::MAX, DeviceRates::default());
    timeline.advance_master_ticks(1, DeviceRates::default());
    assert_eq!(timeline.now_ticks(), u64::MAX);

    timeline.advance_io_stall_ticks(u64::MAX, DeviceRates::default());
    assert_eq!(timeline.io_stall_ticks(), 0);
    assert_eq!(timeline.now_ticks(), u64::MAX);

    let mut phase = RatePhase::with_remainder(MASTER_CLOCK_HZ - 1);
    assert_eq!(phase.advance(u64::MAX, u64::MAX), u64::MAX);
    assert!(phase.remainder() < MASTER_CLOCK_HZ);
    assert_eq!(phase.ticks_until(u64::MAX, 1), Some(u64::MAX));
}

#[test]
fn device_events_and_phases_are_batch_invariant() {
    let rates = DeviceRates {
        dsp_hz: 22_050,
        wss_hz: 48_000,
        cd_playing: true,
        vga_dot_hz: 25_175_000,
    };
    let mut whole = Timeline::new(GswMode::Gsw386);
    let expected = whole.advance_master_ticks(123_456_789, rates);

    let mut split = Timeline::new(GswMode::Gsw386);
    let mut actual = DeviceAdvance::default();
    for ticks in [1, 17, 31_337, 5_000_000, 90_000_000, 28_425_434] {
        let step = split.advance_master_ticks(ticks, rates);
        actual.master_ticks += step.master_ticks;
        actual.microseconds += step.microseconds;
        actual.pit_clocks += step.pit_clocks;
        actual.dsp_frames += step.dsp_frames;
        actual.wss_frames += step.wss_frames;
        actual.cd_frames += step.cd_frames;
        actual.rtc_seconds += step.rtc_seconds;
        actual.margo_nanoseconds += step.margo_nanoseconds;
        actual.margo_frames += step.margo_frames;
        actual.distira_lines += step.distira_lines;
        actual.vga_dots += step.vga_dots;
    }

    assert_eq!(actual.master_ticks, expected.master_ticks);
    assert_eq!(actual.microseconds, expected.microseconds);
    assert_eq!(actual.pit_clocks, expected.pit_clocks);
    assert_eq!(actual.dsp_frames, expected.dsp_frames);
    assert_eq!(actual.wss_frames, expected.wss_frames);
    assert_eq!(actual.cd_frames, expected.cd_frames);
    assert_eq!(actual.rtc_seconds, expected.rtc_seconds);
    assert_eq!(actual.margo_nanoseconds, expected.margo_nanoseconds);
    assert_eq!(actual.margo_frames, expected.margo_frames);
    assert_eq!(actual.distira_lines, expected.distira_lines);
    assert_eq!(actual.vga_dots, expected.vga_dots);
    assert_eq!(split, whole);
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

#[test]
fn pit_and_video_deadlines_choose_the_first_causal_cpu_clock() {
    for mode in [
        GswMode::Gsw586,
        GswMode::Gsw486,
        GswMode::Gsw386,
        GswMode::Gsw386Slow,
    ] {
        let timeline = Timeline::new(mode);
        for (clock, rate) in [
            (DeviceClock::Pit, u64::from(PIT_INPUT_HZ)),
            (DeviceClock::Vga, 25_175_000),
        ] {
            let clocks = timeline.cpu_clocks_until(clock, 1, rate).unwrap();
            let before = clocks.saturating_sub(1);
            let (pit_before, vga_before) = timeline.preview_cpu_clocks(before, 25_175_000);
            let (pit_at, vga_at) = timeline.preview_cpu_clocks(clocks, 25_175_000);
            let (before_events, at_events) = match clock {
                DeviceClock::Pit => (pit_before, pit_at),
                DeviceClock::Vga => (vga_before, vga_at),
                DeviceClock::Dsp | DeviceClock::Wss | DeviceClock::MargoFrame => unreachable!(),
            };
            assert_eq!(before_events, 0, "{mode:?} {clock:?}");
            assert!(at_events >= 1, "{mode:?} {clock:?}");
        }
    }
}

#[test]
fn margo_and_distira_scanout_are_sixty_hz_in_every_cpu_mode() {
    for mode in [
        GswMode::Gsw586,
        GswMode::Gsw486,
        GswMode::Gsw386,
        GswMode::Gsw386Slow,
    ] {
        let mut timeline = Timeline::new(mode);
        let clocks = timeline.cpu_clocks_for_master_ticks_ceil(MASTER_CLOCK_HZ);
        let advance = timeline.advance_cpu_clocks(clocks, DeviceRates::default());
        assert_eq!(advance.margo_frames, 60, "{mode:?}");
        assert_eq!(advance.distira_lines, 525 * 60, "{mode:?}");
        assert!(advance.master_ticks - MASTER_CLOCK_HZ < timeline.ticks_per_cpu_clock());
    }
}
