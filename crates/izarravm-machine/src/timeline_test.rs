// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::{BIOS_ROM_SIZE, Machine, MachineCanonicalCaptureError, MachineProfile};
use izarravm_core::VideoCard;

fn test_machine() -> Machine {
    Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        vec![0; BIOS_ROM_SIZE],
    )
    .unwrap()
}

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
    assert_eq!(timeline.tsc_clocks(), 12);
    assert_eq!(timeline.ticks_per_cpu_clock(), 900);
    timeline.advance_cpu_clocks(1, DeviceRates::default());
    assert_eq!(timeline.now_ticks(), 1_300);
    assert_eq!(timeline.tsc_clocks(), 13);
}

#[test]
fn tsc_clock_is_batch_invariant_and_resets_fraction_on_mode_change() {
    let mut whole = Timeline::new(GswMode::Gsw486);
    whole.advance_master_ticks(10_099, DeviceRates::default());

    let mut split = Timeline::new(GswMode::Gsw486);
    for ticks in [1, 17, 81, 4_000, 6_000] {
        split.advance_master_ticks(ticks, DeviceRates::default());
    }
    assert_eq!(split, whole);
    assert_eq!(whole.tsc_clocks(), 100);

    whole.set_mode(GswMode::Gsw386);
    whole.advance_master_ticks(201, DeviceRates::default());
    assert_eq!(whole.tsc_clocks(), 100);
    whole.advance_master_ticks(99, DeviceRates::default());
    assert_eq!(whole.tsc_clocks(), 101);
}

#[test]
fn canonical_projection_pins_every_timeline_word() {
    let mut timeline = Timeline::new(GswMode::Gsw586);
    timeline.advance_io_stall_ticks(
        34,
        DeviceRates {
            dsp_hz: 2,
            wss_hz: 3,
            cd_playing: true,
            vga_dot_hz: 4,
        },
    );

    let projection = timeline.canonical_projection(GswMode::Gsw586).unwrap();

    assert_eq!(
        projection.words,
        [
            34,
            34,
            1,
            34_000_000,
            40_568_188,
            68,
            102,
            2_550,
            34,
            1_000_000_000,
            2_040,
            1_071_000,
            136,
        ]
    );
}

#[test]
fn absolute_tsc_origin_is_transparent_under_continuation() {
    let rates = DeviceRates {
        dsp_hz: 22_050,
        wss_hz: 48_000,
        cd_playing: true,
        vga_dot_hz: 25_175_000,
    };
    let mut left = Timeline::new(GswMode::Gsw386);
    let mut right = left;
    right.tsc_clocks = 0xd00d_f00d_dead_beef;

    left.set_mode(GswMode::Gsw586);
    right.set_mode(GswMode::Gsw586);
    assert_eq!(
        left.advance_master_ticks(1_003, rates),
        right.advance_master_ticks(1_003, rates)
    );
    assert_eq!(
        left.advance_io_stall_ticks(2_009, rates),
        right.advance_io_stall_ticks(2_009, rates)
    );
    assert_eq!(
        left.advance_master_ticks(3_007, rates),
        right.advance_master_ticks(3_007, rates)
    );

    left.now_ticks = u64::MAX - 7;
    right.now_ticks = u64::MAX - 7;
    assert_eq!(
        left.advance_master_ticks(19, rates),
        right.advance_master_ticks(19, rates)
    );

    assert_eq!(
        left.canonical_projection(GswMode::Gsw586),
        right.canonical_projection(GswMode::Gsw586)
    );
    left.tsc_clocks = 0;
    right.tsc_clocks = 0;
    assert_eq!(left, right);
}

#[test]
fn absolute_tsc_origin_cannot_change_machine_or_cpu_continuation() {
    let mut left = test_machine();
    let mut right = test_machine();
    right.timeline.tsc_clocks = 0xd00d_f00d_dead_beef;

    left.set_mode(GswMode::Gsw586);
    right.set_mode(GswMode::Gsw586);
    left.advance_cpu_work(7, 3);
    right.advance_cpu_work(7, 3);
    left.stall_for_master_ticks(101);
    right.stall_for_master_ticks(101);
    left.advance_halted_cpu_clocks(3);
    right.advance_halted_cpu_clocks(3);
    left.timeline.now_ticks = u64::MAX - 7;
    right.timeline.now_ticks = u64::MAX - 7;
    left.advance_halted_ticks(19);
    right.advance_halted_ticks(19);

    assert_eq!(
        left.cpu, right.cpu,
        "CPU state includes the architectural TSC"
    );
    assert_eq!(left.pic, right.pic);
    assert_eq!(left.pit, right.pit);
    assert_eq!(left.dsp, right.dsp);
    assert_eq!(left.wss, right.wss);
    assert_eq!(left.opl, right.opl);
    assert_eq!(left.elapsed_clocks, right.elapsed_clocks);
    assert_eq!(left.io_stall_clocks, right.io_stall_clocks);
    assert_eq!(left.halted_ticks, right.halted_ticks);
    let mut left_timeline = left.timeline;
    let mut right_timeline = right.timeline;
    left_timeline.tsc_clocks = 0;
    right_timeline.tsc_clocks = 0;
    assert_eq!(left_timeline, right_timeline);
}

#[test]
fn canonical_projection_rejects_each_invalid_timeline_invariant() {
    let mut quantum = test_machine();
    quantum.timeline.ticks_per_cpu_clock = 301;
    assert_eq!(
        quantum.canonical_state_capture().err().unwrap(),
        MachineCanonicalCaptureError::InconsistentTimelineQuantum {
            expected: 300,
            actual: 301,
        }
    );

    let mut tsc = test_machine();
    tsc.timeline.tsc_phase_ticks = 300;
    assert_eq!(
        tsc.canonical_state_capture().err().unwrap(),
        MachineCanonicalCaptureError::InvalidTimelineRemainder {
            phase: "tsc",
            remainder: 300,
            limit: 300,
        }
    );

    let mut rate = test_machine();
    rate.timeline.dsp = RatePhase {
        remainder: MASTER_CLOCK_HZ,
    };
    assert_eq!(
        rate.canonical_state_capture().err().unwrap(),
        MachineCanonicalCaptureError::InvalidTimelineRemainder {
            phase: "dsp",
            remainder: MASTER_CLOCK_HZ,
            limit: MASTER_CLOCK_HZ,
        }
    );

    let mut totals = test_machine();
    totals.timeline.now_ticks = 3;
    totals.timeline.io_stall_ticks = 4;
    assert_eq!(
        totals.canonical_state_capture().err().unwrap(),
        MachineCanonicalCaptureError::InvalidTimelineTotals {
            now_ticks: 3,
            io_stall_ticks: 4,
        }
    );
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
    let saturated_tsc = timeline.tsc_clocks();
    timeline.advance_master_ticks(1, DeviceRates::default());
    assert_eq!(timeline.now_ticks(), u64::MAX);
    assert_eq!(timeline.tsc_clocks(), saturated_tsc);

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
    assert_eq!(
        split.canonical_projection(GswMode::Gsw386),
        whole.canonical_projection(GswMode::Gsw386)
    );
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
                DeviceClock::Dsp
                | DeviceClock::Wss
                | DeviceClock::MargoFrame
                | DeviceClock::Rtc => unreachable!(),
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
