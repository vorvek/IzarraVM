// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

// ---------------------------------------------------------------------------
// The acceptance oracle.
//
// The eleven fixture rows of design §9.5 leg A, as whole-run counters read off
// `.bench/results/scoreboard-20260815-181222-armon/profiles/`. `.bench/` is
// git-ignored, so the numbers are carried here rather than the files: this
// table IS the gate, and `the_real_fixture_board_still_matches` re-reads the
// originals when they happen to be on disk.
//
// Columns: short, rt, instructions, jit_direct_insns, jit_direct_entries,
// smc_heat_demotions, callouts, side_exit_x87_eligibility, x87_pad_bails,
// callout_port_v86_served, expected buckets, expected health.
// ---------------------------------------------------------------------------
struct Fixture {
    short: &'static str,
    rt: f64,
    instructions: u64,
    jit_direct_insns: u64,
    jit_direct_entries: u64,
    smc_heat_demotions: u64,
    callouts: u64,
    x87_eligibility: u64,
    x87_pad_bails: u64,
    port_v86_served: u64,
    expected_buckets: &'static str,
    expected_health: &'static str,
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        short: "doom-486",
        rt: 5.555_500,
        instructions: 2_353_574_202,
        jit_direct_insns: 2_233_756_041,
        jit_direct_entries: 41_593_586,
        smc_heat_demotions: 2,
        callouts: 10_098_869,
        x87_eligibility: 0,
        x87_pad_bails: 0,
        port_v86_served: 0,
        expected_buckets: "",
        expected_health: "HEALTHY",
    },
    Fixture {
        short: "doom-586",
        rt: 2.144_138,
        instructions: 2_765_390_393,
        jit_direct_insns: 2_680_270_186,
        jit_direct_entries: 41_201_212,
        smc_heat_demotions: 2,
        callouts: 26_379_903,
        x87_eligibility: 0,
        x87_pad_bails: 0,
        port_v86_served: 0,
        expected_buckets: "",
        expected_health: "HEALTHY",
    },
    Fixture {
        short: "quake-586",
        rt: 1.846_418,
        instructions: 3_961_474_751,
        jit_direct_insns: 3_868_207_434,
        jit_direct_entries: 38_756_602,
        smc_heat_demotions: 2,
        callouts: 10_771_848,
        x87_eligibility: 785_923,
        x87_pad_bails: 0,
        port_v86_served: 0,
        expected_buckets: "",
        expected_health: "HEALTHY",
    },
    Fixture {
        short: "prince-486",
        rt: 0.839_756,
        instructions: 2_081_519_802,
        jit_direct_insns: 2_062_413_892,
        jit_direct_entries: 1_300_531_246,
        smc_heat_demotions: 1,
        callouts: 5_669_492,
        x87_eligibility: 0,
        x87_pad_bails: 0,
        port_v86_served: 5_669_463,
        expected_buckets: "B1",
        expected_health: "NON-HEALTHY",
    },
    Fixture {
        short: "duke3d-486",
        rt: 1.306_285,
        instructions: 6_210_559_883,
        jit_direct_insns: 4_819_040_888,
        jit_direct_entries: 437_378_287,
        smc_heat_demotions: 35_345,
        callouts: 5_623_937,
        x87_eligibility: 0,
        x87_pad_bails: 551_451,
        port_v86_served: 0,
        expected_buckets: "B2|B3|B5b",
        expected_health: "HEALTHY-WITH-FINDINGS",
    },
    Fixture {
        short: "duke3d-586",
        rt: 0.323_845,
        instructions: 23_053_472_906,
        jit_direct_insns: 17_701_909_333,
        jit_direct_entries: 1_657_719_855,
        smc_heat_demotions: 146_420,
        callouts: 6_404_487,
        x87_eligibility: 0,
        x87_pad_bails: 2_166_477,
        port_v86_served: 0,
        expected_buckets: "B2|B3|B5b",
        expected_health: "NON-HEALTHY",
    },
    Fixture {
        short: "nascar-586",
        rt: 0.454_635,
        instructions: 6_328_460_184,
        jit_direct_insns: 5_918_343_114,
        jit_direct_entries: 180_273_243,
        smc_heat_demotions: 24_988,
        callouts: 2_450_600,
        x87_eligibility: 0,
        x87_pad_bails: 0,
        port_v86_served: 0,
        expected_buckets: "B3",
        expected_health: "NON-HEALTHY",
    },
    Fixture {
        short: "gp2-586",
        rt: 0.390_845,
        instructions: 14_134_352_240,
        jit_direct_insns: 12_739_307_480,
        jit_direct_entries: 615_796_713,
        smc_heat_demotions: 8,
        callouts: 2_007_457_556,
        x87_eligibility: 0,
        x87_pad_bails: 0,
        port_v86_served: 0,
        expected_buckets: "B4",
        expected_health: "NON-HEALTHY",
    },
    Fixture {
        short: "tombraid-586",
        rt: 0.518_641,
        instructions: 19_518_687_347,
        jit_direct_insns: 16_847_943_903,
        jit_direct_entries: 1_468_365_923,
        smc_heat_demotions: 4,
        callouts: 187_229_754,
        x87_eligibility: 71_330_054,
        x87_pad_bails: 0,
        port_v86_served: 3_055,
        expected_buckets: "B5a",
        expected_health: "NON-HEALTHY",
    },
    Fixture {
        short: "wolf3d-486",
        rt: 3.365_758,
        instructions: 4_188_801_044,
        jit_direct_insns: 4_036_000_016,
        jit_direct_entries: 126_666_605,
        smc_heat_demotions: 16_913,
        callouts: 91_246_314,
        x87_eligibility: 0,
        x87_pad_bails: 0,
        port_v86_served: 91_246_311,
        expected_buckets: "B3|B4|B6",
        expected_health: "HEALTHY-WITH-FINDINGS",
    },
    Fixture {
        short: "wolf3d-586",
        rt: 0.848_121,
        instructions: 15_215_881_467,
        jit_direct_insns: 14_905_639_091,
        jit_direct_entries: 289_162_797,
        smc_heat_demotions: 5_751,
        callouts: 364_743_475,
        x87_eligibility: 0,
        x87_pad_bails: 0,
        port_v86_served: 364_743_472,
        expected_buckets: "B3|B4|B6",
        // rt 0.85 even after its lever landed: still short of the persona.
        expected_health: "NON-HEALTHY",
    },
];

fn fixture_profile(fixture: &Fixture) -> Profile {
    Profile {
        schema: "izarravm-hdd-profile-v1".to_string(),
        real_time_factor: fixture.rt,
        guest_seconds: 120.0,
        wall_seconds: 120.0 / fixture.rt,
        master_ticks: 657_360_000_000,
        stop: Stop {
            kind: "cycle_limit".to_string(),
            ..Stop::default()
        },
        perf: Perf {
            instructions: fixture.instructions,
            jit_direct_insns: fixture.jit_direct_insns,
            jit_direct_entries: fixture.jit_direct_entries,
            smc_heat_demotions: fixture.smc_heat_demotions,
            jit_direct_x87_pad_bails: fixture.x87_pad_bails,
            ..Perf::default()
        },
        direct_stalls: DirectStalls {
            jit_direct_callout_executed: fixture.callouts,
            side_exit_x87_eligibility: fixture.x87_eligibility,
            jit_direct_callout_port_v86_served: fixture.port_v86_served,
            ..DirectStalls::default()
        },
        ..Profile::default()
    }
}

fn fixture_row(fixture: &Fixture) -> ClassifiedRow {
    classify_archive(&Archive {
        short: fixture.short.to_string(),
        profile: Some(fixture_profile(fixture)),
        ..Archive::default()
    })
}

/// THE GATE. Design §9.5 leg A, reproduced row for row. If this fails, either
/// the code is wrong or the table is; nothing here may be re-tuned to pass.
#[test]
fn the_by_hand_bucket_table_reproduces_exactly() {
    let mut disagreements = Vec::new();
    for fixture in FIXTURES {
        let row = fixture_row(fixture);
        let got = row.buckets.join("|");
        if got != fixture.expected_buckets || row.health != fixture.expected_health {
            disagreements.push(format!(
                "{}: got [{got}]/{} expected [{}]/{}",
                fixture.short, row.health, fixture.expected_buckets, fixture.expected_health
            ));
        }
    }
    assert!(disagreements.is_empty(), "{disagreements:#?}");
}

#[test]
fn no_bucket_fires_on_a_healthy_anchor() {
    for short in ["doom-486", "doom-586", "quake-586"] {
        let fixture = FIXTURES.iter().find(|f| f.short == short).unwrap();
        let row = fixture_row(fixture);
        assert!(row.buckets.is_empty(), "{short} fired {:?}", row.buckets);
        assert_eq!(row.health, "HEALTHY");
    }
}

#[test]
fn every_surviving_bucket_fires_on_at_least_one_fixture() {
    let mut seen: Vec<String> = FIXTURES
        .iter()
        .flat_map(|fixture| fixture_row(fixture).buckets)
        .collect();
    seen.sort();
    seen.dedup();
    assert_eq!(seen, ["B1", "B2", "B3", "B4", "B5a", "B5b", "B6"]);
}

/// The tightest margin in the repaired set, called out in §9.2 so the next
/// campaign does not discover it by surprise: wolf3d-586 clears B3 by 3.8x and
/// nothing else in the firing set is closer to the bar.
#[test]
fn wolf3d_586_clears_b3_by_only_three_point_eight_times() {
    let wolf = FIXTURES.iter().find(|f| f.short == "wolf3d-586").unwrap();
    let margin =
        (wolf.smc_heat_demotions as f64 / wolf.instructions as f64) / B3_DEMOTIONS_PER_INSN_MIN;
    assert!(
        (3.7..3.9).contains(&margin),
        "wolf3d-586 B3 margin moved to {margin}x; §9.2 recorded 3.8x. \
         A lever that shifts wolf's demotion rate by ~3x breaks B3 and the \
         threshold must be re-derived, not nudged."
    );

    // And it really is the tightest: every other firing row clears by more.
    let mut tighter = Vec::new();
    for fixture in FIXTURES {
        if fixture.short == "wolf3d-586" {
            continue;
        }
        let value = fixture.smc_heat_demotions as f64 / fixture.instructions as f64;
        if value > B3_DEMOTIONS_PER_INSN_MIN && value / B3_DEMOTIONS_PER_INSN_MIN <= margin {
            tighter.push(fixture.short);
        }
    }
    assert!(
        tighter.is_empty(),
        "closer to the B3 bar than wolf: {tighter:?}"
    );
}

/// The excluded side of B3's 445x gap: the highest non-firing row is doom-486
/// at 8.5e-10, three orders below the bar.
#[test]
fn b3_excludes_every_healthy_row_by_orders_of_magnitude() {
    let worst = FIXTURES
        .iter()
        .map(|f| f.smc_heat_demotions as f64 / f.instructions as f64)
        .filter(|value| *value <= B3_DEMOTIONS_PER_INSN_MIN)
        .fold(0.0f64, f64::max);
    assert!(worst < 1e-9, "highest excluded B3 value is {worst}");
}

// ---------------------------------------------------------------------------
// The window: delta arithmetic against a real mark series.
//
// The numbers are DOOM's own, from
// `.bench/results/exodos-smoke-20260816/DOOM/profile.json` — the mark at index
// 31 (guest 60.0003 s) and the final mark (guest 120.0000 s). The deltas below
// were computed by hand from those two absolute snapshots.
// ---------------------------------------------------------------------------

/// Master ticks per guest second on the smoke board.
const DOOM_HZ: u64 = 5_478_000_000;

fn doom_marks() -> Vec<Mark> {
    vec![
        // Guest 0 s. The BENCH_START mark: everything still zero.
        Mark {
            id: 201,
            master_ticks: 0,
            ..Mark::default()
        },
        // Guest 10 s, inside the boot phase.
        Mark {
            id: 200,
            master_ticks: 54_780_000_000,
            instructions: 1_000_000_000,
            jit_direct_insns: 900_000_000,
            jit_direct_entries: 8_000_000,
            smc_heat_demotions: 5,
            device_write_bytes: 1_000_000,
            halted_ticks: 4_000_000_000,
            ..Mark::default()
        },
        // Guest 60.0003 s. THE WINDOW BASE for a 120 s run.
        Mark {
            id: 200,
            master_ticks: 328_680_080_805,
            instructions: 10_182_650_494,
            jit_direct_insns: 10_042_467_551,
            jit_direct_entries: 60_766_559,
            smc_heat_demotions: 12,
            device_write_bytes: 6_809_781,
            halted_ticks: 10_786_543_020,
            ..Mark::default()
        },
        // Guest 120.0000 s. The BENCH_END mark; equals the whole-run totals.
        Mark {
            id: 202,
            master_ticks: 657_360_100_704,
            instructions: 21_166_804_925,
            jit_direct_insns: 20_941_471_869,
            jit_direct_entries: 104_027_882,
            smc_heat_demotions: 12,
            device_write_bytes: 6_809_781,
            halted_ticks: 10_786_543_020,
            ..Mark::default()
        },
    ]
}

fn doom_profile() -> Profile {
    Profile {
        schema: "izarravm-hdd-profile-v1".to_string(),
        real_time_factor: 2.053_152_738_536_905_3,
        guest_seconds: 120.000_018_383_351_59,
        wall_seconds: 58.451,
        master_ticks: 657_360_100_704,
        stop: Stop {
            kind: "cycle_limit".to_string(),
            ..Stop::default()
        },
        perf: Perf {
            instructions: 21_166_804_925,
            jit_direct_insns: 20_941_471_869,
            jit_direct_entries: 104_027_882,
            smc_heat_demotions: 12,
            device_write_bytes: 6_809_781,
            ..Perf::default()
        },
        phase_marks: doom_marks(),
        ..Profile::default()
    }
}

#[test]
fn master_hz_comes_from_the_profile_not_a_constant() {
    let hz = master_hz(&doom_profile());
    assert!((hz - DOOM_HZ as f64).abs() < 1.0, "{hz}");
}

#[test]
fn the_window_base_is_the_mark_nearest_sixty_guest_seconds_back() {
    let profile = doom_profile();
    let index = window_base_index(&profile.phase_marks, master_hz(&profile)).unwrap();
    assert_eq!(index, 2);
}

#[test]
fn window_deltas_match_the_hand_computed_values() {
    let window = compute_window(&doom_profile());
    assert!(window.available);
    assert_eq!(window.mark_count, 4);
    assert_eq!(window.base_index, 2);
    // 21_166_804_925 - 10_182_650_494
    assert_eq!(window.instructions, 10_984_154_431);
    // 20_941_471_869 - 10_042_467_551
    assert_eq!(window.jit_direct_insns, 10_899_004_318);
    // 104_027_882 - 60_766_559
    assert_eq!(window.jit_direct_entries, 43_261_323);
    // 657_360_100_704 - 328_680_080_805
    assert_eq!(window.master_ticks, 328_680_019_899);
    assert_eq!(window.smc_heat_demotions, 0);
    // The frame proxy is FLAT across the window on a game that is plainly
    // running. This is the measurement that refutes the counter-only idle
    // test; see `IdleEvidence`.
    assert_eq!(window.device_write_bytes, 0);
    assert_eq!(window.halted_ticks, 0);
    // 328_680_019_899 / 5_478_000_000 = 59.99999998...
    assert!(
        (window.guest_seconds - 60.0).abs() < 1e-3,
        "{}",
        window.guest_seconds
    );
    // The window carries 51.9% of the run's instructions.
    assert!(
        (window.window_fraction - 0.518_931).abs() < 1e-5,
        "{}",
        window.window_fraction
    );
}

#[test]
fn a_series_shorter_than_two_marks_has_no_window() {
    let mut profile = doom_profile();
    profile.phase_marks.truncate(1);
    let window = compute_window(&profile);
    assert!(!window.available);
    assert_eq!(window.mark_count, 1);
    profile.phase_marks.clear();
    assert!(!compute_window(&profile).available);
}

#[test]
fn the_final_mark_is_never_its_own_window_base() {
    let marks = vec![
        Mark {
            master_ticks: 0,
            ..Mark::default()
        },
        Mark {
            master_ticks: 657_360_000_000,
            ..Mark::default()
        },
    ];
    // The target sits 60 s before the end, which is nearer the LAST mark than
    // the first; the base must still be the first.
    assert_eq!(window_base_index(&marks, DOOM_HZ as f64), Some(0));
}

#[test]
fn window_deltas_never_underflow_on_a_non_monotonic_series() {
    let mut profile = doom_profile();
    let last = profile.phase_marks.len() - 1;
    profile.phase_marks[last].instructions = 0;
    let window = compute_window(&profile);
    assert_eq!(window.instructions, 0);
}

#[test]
fn a_windowed_bucket_reads_the_window_and_not_the_whole_run() {
    // Boot burns 1e9 instructions fully interpreted; the window is native.
    let profile = Profile {
        schema: "izarravm-hdd-profile-v1".to_string(),
        real_time_factor: 1.0,
        guest_seconds: 120.0,
        master_ticks: 657_360_000_000,
        perf: Perf {
            instructions: 2_000_000_000,
            jit_direct_insns: 1_000_000_000,
            ..Perf::default()
        },
        phase_marks: vec![
            Mark {
                master_ticks: 0,
                ..Mark::default()
            },
            Mark {
                master_ticks: 328_680_000_000,
                instructions: 1_000_000_000,
                jit_direct_insns: 0,
                ..Mark::default()
            },
            Mark {
                master_ticks: 657_360_000_000,
                instructions: 2_000_000_000,
                jit_direct_insns: 1_000_000_000,
                ..Mark::default()
            },
        ],
        ..Profile::default()
    };
    let window = compute_window(&profile);
    let inputs = BucketInputs::from_profile(&profile, &window);
    assert!(inputs.windowed);
    // Whole-run interpreter share is 0.50 and would trip B2; the window's is
    // 0.0 and does not.
    assert!((inputs.interpreter_share()).abs() < 1e-12);
    assert!(buckets(&inputs).is_empty());
}

// ---------------------------------------------------------------------------
// Outcomes
// ---------------------------------------------------------------------------

fn marks(count: usize) -> Vec<Mark> {
    (0..count)
        .map(|index| Mark {
            id: 200,
            master_ticks: index as u64 * 2 * DOOM_HZ,
            instructions: index as u64 * 100_000_000,
            jit_direct_insns: index as u64 * 99_000_000,
            jit_direct_entries: index as u64 * 1_000_000,
            ..Mark::default()
        })
        .collect()
}

fn ran_profile(mark_count: usize) -> Profile {
    let series = marks(mark_count);
    let master = series.last().map(|mark| mark.master_ticks).unwrap_or(0);
    Profile {
        schema: "izarravm-hdd-profile-v1".to_string(),
        real_time_factor: 2.0,
        guest_seconds: master as f64 / DOOM_HZ as f64,
        master_ticks: master,
        stop: Stop {
            kind: "cycle_limit".to_string(),
            ..Stop::default()
        },
        perf: Perf {
            instructions: 100_000_000 * mark_count as u64,
            jit_direct_insns: 99_000_000 * mark_count as u64,
            jit_direct_entries: 1_000_000 * mark_count as u64,
            ..Perf::default()
        },
        phase_marks: series,
        ..Profile::default()
    }
}

#[test]
fn short_run_boundary_is_exactly_thirty_one_marks() {
    let thirty = ran_profile(30);
    assert_eq!(
        decide_outcome(Some(&thirty), &[], HostVerdict::default()),
        Outcome::ShortRun
    );
    let thirty_one = ran_profile(31);
    assert_eq!(
        decide_outcome(Some(&thirty_one), &[], HostVerdict::default()),
        Outcome::Ran
    );
}

#[test]
fn a_short_run_carries_no_buckets_and_is_excluded() {
    let row = classify_archive(&Archive {
        short: "short".to_string(),
        profile: Some(ran_profile(30)),
        ..Archive::default()
    });
    assert_eq!(row.outcome, "SHORT-RUN");
    assert!(row.buckets.is_empty());
    assert_eq!(row.health, "EXCLUDED");
}

/// The deviation from §9.4, pinned so it cannot be "fixed" by accident: an
/// EMPTY mark series means marks were never armed, which is not a short run.
/// Every in-tree fixture profile is this shape and the gate depends on it.
#[test]
fn an_unarmed_mark_series_is_not_a_short_run() {
    let mut profile = ran_profile(31);
    profile.phase_marks.clear();
    assert_eq!(
        decide_outcome(Some(&profile), &[], HostVerdict::default()),
        Outcome::Ran
    );
    let row = classify_archive(&Archive {
        short: "unarmed".to_string(),
        profile: Some(profile),
        ..Archive::default()
    });
    assert!(row.flags.iter().any(|flag| flag == "NO-MARKS"));
    assert_eq!(row.health, "HEALTHY");
}

fn screen(index: u64, hash: &str, mode: Option<&str>) -> Screen {
    Screen {
        i: index,
        master_ticks: index * 5 * DOOM_HZ,
        guest_ms: index * 5_000,
        display: "vga".to_string(),
        video_mode: mode.map(str::to_string),
        hash: hash.to_string(),
        changed: false,
        text_glyphs: None,
    }
}

#[test]
fn a_crash_beats_every_other_signal() {
    let mut profile = ran_profile(31);
    profile.stop.kind = "cpu_error".to_string();
    assert_eq!(
        decide_outcome(Some(&profile), &[], HostVerdict::default()),
        Outcome::Crashed
    );
}

#[test]
fn two_returns_to_the_opening_frame_are_a_reboot_loop() {
    let screens = vec![
        screen(0, "aaa", Some("text")),
        screen(1, "bbb", Some("modex")),
        screen(2, "aaa", Some("text")),
        screen(3, "bbb", Some("modex")),
        screen(4, "aaa", Some("text")),
    ];
    assert_eq!(screen_recurrences(&screens), 2);
    let profile = ran_profile(31);
    assert_eq!(
        decide_outcome(Some(&profile), &screens, HostVerdict::default()),
        Outcome::RebootLoop
    );
}

#[test]
fn one_return_to_the_opening_frame_is_not_a_reboot_loop() {
    let screens = vec![
        screen(0, "aaa", Some("modex")),
        screen(1, "bbb", Some("modex")),
        screen(2, "aaa", Some("modex")),
        screen(3, "ccc", Some("modex")),
        screen(4, "ddd", Some("modex")),
        screen(5, "eee", Some("modex")),
    ];
    assert_eq!(screen_recurrences(&screens), 1);
    assert_eq!(
        decide_outcome(Some(&ran_profile(31)), &screens, HostVerdict::default()),
        Outcome::Ran
    );
}

/// A flat picture in a graphics mode, with the guest asleep, is a menu wait.
#[test]
fn a_flat_graphics_picture_with_a_polling_signature_is_idle_at_menu() {
    let mut profile = ran_profile(31);
    // Park the guest: the whole window is halted ticks.
    let last = profile.phase_marks.len() - 1;
    profile.phase_marks[last].halted_ticks = profile.phase_marks[last].master_ticks;
    let end = profile.master_ticks;
    let screens: Vec<Screen> = (0..4)
        .map(|index| Screen {
            master_ticks: end - index * DOOM_HZ,
            ..screen(index, "same", Some("modex"))
        })
        .collect();
    let picture = screen_window(&screens, &profile);
    assert!(picture.flat, "{picture:?}");
    assert_eq!(
        decide_outcome(Some(&profile), &screens, HostVerdict::default()),
        Outcome::IdleAtMenu
    );
}

#[test]
fn a_flat_text_picture_is_idle_text_not_idle_at_menu() {
    let mut profile = ran_profile(31);
    let last = profile.phase_marks.len() - 1;
    profile.phase_marks[last].halted_ticks = profile.phase_marks[last].master_ticks;
    let end = profile.master_ticks;
    let screens: Vec<Screen> = (0..4)
        .map(|index| Screen {
            master_ticks: end - index * DOOM_HZ,
            ..screen(index, "same", Some("text"))
        })
        .collect();
    assert_eq!(
        decide_outcome(Some(&profile), &screens, HostVerdict::default()),
        Outcome::IdleText
    );
}

/// The owner's rule is a conjunction. A flat picture with no polling term
/// stays `RAN` and says so in a flag rather than disappearing.
#[test]
fn a_flat_picture_without_a_polling_term_stays_ran_and_is_flagged() {
    let mut profile = ran_profile(31);
    // Give the window live device writes so no polling term fires.
    let last = profile.phase_marks.len() - 1;
    for mark in profile.phase_marks.iter_mut() {
        mark.device_write_bytes = mark.master_ticks / 1_000;
    }
    let _ = last;
    let end = profile.master_ticks;
    let screens: Vec<Screen> = (0..4)
        .map(|index| Screen {
            master_ticks: end - index * DOOM_HZ,
            ..screen(index, "same", Some("modex"))
        })
        .collect();
    let row = classify_archive(&Archive {
        short: "flat".to_string(),
        profile: Some(profile),
        screens,
        ..Archive::default()
    });
    assert_eq!(row.outcome, "RAN");
    assert!(row.idle.frame_flat);
    assert!(!row.idle.polling_signature);
    assert!(row.flags.iter().any(|f| f == "FLAT-PICTURE-NOT-IDLE"));
}

/// Two samples cannot establish flatness, whatever they show.
#[test]
fn flatness_needs_at_least_three_in_window_samples() {
    let profile = ran_profile(31);
    let end = profile.master_ticks;
    let screens: Vec<Screen> = (0..2)
        .map(|index| Screen {
            master_ticks: end - index * DOOM_HZ,
            ..screen(index, "same", Some("modex"))
        })
        .collect();
    assert!(!screen_window(&screens, &profile).flat);
}

/// A Margo-framebuffer title has no mode line at all: it is flagged blind
/// rather than being called text.
#[test]
fn a_margo_display_is_flagged_idle_blind() {
    let profile = ran_profile(31);
    let end = profile.master_ticks;
    let screens: Vec<Screen> = (0..4)
        .map(|index| Screen {
            master_ticks: end - index * DOOM_HZ,
            display: "margo".to_string(),
            hash: format!("h{index}"),
            ..screen(index, "unused", None)
        })
        .collect();
    let row = classify_archive(&Archive {
        short: "margo".to_string(),
        profile: Some(profile),
        screens,
        ..Archive::default()
    });
    assert!(row.flags.iter().any(|flag| flag == "IDLE-BLIND"));
    assert_eq!(row.outcome, "RAN");
}

#[test]
fn host_side_verdicts_win_over_the_profile() {
    let profile = ran_profile(31);
    assert_eq!(
        decide_outcome(
            Some(&profile),
            &[],
            HostVerdict {
                stalled: true,
                ..HostVerdict::default()
            }
        ),
        Outcome::Stalled
    );
    assert_eq!(
        decide_outcome(
            Some(&profile),
            &[],
            HostVerdict {
                timed_out: true,
                ..HostVerdict::default()
            }
        ),
        Outcome::HungHost
    );
    assert_eq!(
        decide_outcome(None, &[], HostVerdict::default()),
        Outcome::NoProfile
    );
}

#[test]
fn halted_and_dos_exit_are_bucketable_outcomes() {
    for (kind, expected) in [
        ("halted", Outcome::Halted),
        ("dos_exit", Outcome::Exited),
        ("test_exit", Outcome::Exited),
    ] {
        let mut profile = ran_profile(31);
        profile.stop.kind = kind.to_string();
        let outcome = decide_outcome(Some(&profile), &[], HostVerdict::default());
        assert_eq!(outcome, expected);
        assert!(outcome.bucketable());
    }
}

// ---------------------------------------------------------------------------
// Each threshold, at its edge. Every rule is a strict inequality, so a metric
// sitting exactly on the bar must NOT fire.
// ---------------------------------------------------------------------------

fn edge_inputs() -> BucketInputs {
    BucketInputs {
        windowed_instructions: 1_000_000_000,
        windowed_jit_direct_insns: 1_000_000_000,
        windowed_jit_direct_entries: 10_000_000,
        windowed_smc_heat_demotions: 0,
        instructions: 1_000_000_000,
        callouts: 0,
        side_exit_x87_eligibility: 0,
        x87_pad_bails: 0,
        callout_port_v86_served: 0,
        real_time_factor: 1.0,
        windowed: true,
    }
}

fn ids(inputs: &BucketInputs) -> Vec<&'static str> {
    buckets(inputs).into_iter().map(|hit| hit.id).collect()
}

#[test]
fn b1_needs_both_clauses_and_is_strict_on_each() {
    let mut inputs = edge_inputs();
    // ipe exactly 4.0 with entries/I 0.25: the ipe clause is `< 4`, so no.
    inputs.windowed_jit_direct_entries = 250_000_000;
    inputs.windowed_jit_direct_insns = 1_000_000_000;
    assert!((inputs.insns_per_entry() - 4.0).abs() < 1e-12);
    assert!(!ids(&inputs).contains(&"B1"));

    // ipe 3.99 -> fires.
    inputs.windowed_jit_direct_insns = 997_500_000;
    assert!(ids(&inputs).contains(&"B1"));

    // ipe low but entries/I exactly 0.05: the second clause is `> 0.05`.
    let mut narrow = edge_inputs();
    narrow.windowed_jit_direct_entries = 50_000_000;
    narrow.windowed_jit_direct_insns = 100_000_000;
    assert!((narrow.entries_per_insn() - 0.05).abs() < 1e-12);
    assert!(narrow.insns_per_entry() < B1_IPE_MAX);
    assert!(!ids(&narrow).contains(&"B1"));
    narrow.windowed_jit_direct_entries = 50_000_001;
    assert!(ids(&narrow).contains(&"B1"));
}

#[test]
fn b2_is_strict_at_fifteen_percent_interpreted() {
    let mut inputs = edge_inputs();
    inputs.windowed_jit_direct_insns = 850_000_001;
    assert!(inputs.interpreter_share() < 0.15);
    assert!(!ids(&inputs).contains(&"B2"));
    inputs.windowed_jit_direct_insns = 849_999_999;
    assert!(inputs.interpreter_share() > 0.15);
    assert!(ids(&inputs).contains(&"B2"));

    // Recorded rather than hidden: B2's metric is `1 - N/I`, a subtraction, so
    // there is no input that lands it exactly on 0.15. At N/I = 0.85 the
    // result is one ulp ABOVE the bar and the bucket fires. The bar is
    // effectively 0.15 minus an ulp, which is a property of the formula and
    // not of the fixtures — no fixture sits anywhere near it (duke 0.224,
    // tombraid 0.137).
    inputs.windowed_jit_direct_insns = 850_000_000;
    assert!(inputs.interpreter_share() > 0.15);
    assert!((inputs.interpreter_share() - 0.15).abs() < 1e-15);
}

#[test]
fn b3_is_strict_at_one_e_minus_seven() {
    let mut inputs = edge_inputs();
    // 100 / 1e9 = 1e-7 exactly.
    inputs.windowed_smc_heat_demotions = 100;
    assert!(!ids(&inputs).contains(&"B3"));
    inputs.windowed_smc_heat_demotions = 101;
    assert!(ids(&inputs).contains(&"B3"));
}

#[test]
fn b4_is_strict_at_fifteen_thousandths() {
    let mut inputs = edge_inputs();
    inputs.callouts = 15_000_000;
    assert!(!ids(&inputs).contains(&"B4"));
    inputs.callouts = 15_000_001;
    assert!(ids(&inputs).contains(&"B4"));
}

/// The B4 bar must keep both wolf rows and drop tomb and doom-586. This is the
/// arithmetic the review got wrong (it proposed 0.03, which excludes wolf).
#[test]
fn the_b4_bar_sits_between_tombraid_and_wolf3d_486() {
    let value = |short: &str| {
        let fixture = FIXTURES.iter().find(|f| f.short == short).unwrap();
        fixture.callouts as f64 / fixture.instructions as f64
    };
    assert!(value("tombraid-586") < B4_CALLOUTS_PER_INSN_MIN);
    assert!(value("doom-586") < B4_CALLOUTS_PER_INSN_MIN);
    assert!(value("wolf3d-486") > B4_CALLOUTS_PER_INSN_MIN);
    assert!(value("wolf3d-586") > B4_CALLOUTS_PER_INSN_MIN);
    // And the review's 0.03 would have excluded both wolf rows.
    assert!(value("wolf3d-486") < 0.03);
    assert!(value("wolf3d-586") < 0.03);
}

#[test]
fn b5a_is_strict_at_one_e_minus_three() {
    let mut inputs = edge_inputs();
    inputs.side_exit_x87_eligibility = 1_000_000;
    assert!(!ids(&inputs).contains(&"B5a"));
    inputs.side_exit_x87_eligibility = 1_000_001;
    assert!(ids(&inputs).contains(&"B5a"));
}

/// B5a must exclude quake, which takes eligibility exits too.
///
/// §9.2 says B5a "selects tomb, excluding quake by 18x". That 18x is the
/// tomb-to-quake separation (3.65e-3 / 1.98e-4), not the distance from the
/// bar: quake sits 5.0x below the 1e-3 threshold. Both are asserted so the
/// prose cannot be read as a 18x safety margin on the bar itself.
#[test]
fn b5a_excludes_quake_and_selects_tombraid() {
    let value = |short: &str| {
        let fixture = FIXTURES.iter().find(|f| f.short == short).unwrap();
        fixture.x87_eligibility as f64 / fixture.instructions as f64
    };
    let quake = value("quake-586");
    let tomb = value("tombraid-586");
    assert!(quake < B5A_X87_ELIGIBILITY_PER_INSN_MIN);
    assert!(tomb > B5A_X87_ELIGIBILITY_PER_INSN_MIN);
    let bar_margin = B5A_X87_ELIGIBILITY_PER_INSN_MIN / quake;
    assert!(
        (4.9..5.2).contains(&bar_margin),
        "quake to bar {bar_margin}x"
    );
    let separation = tomb / quake;
    assert!(
        (18.0..19.0).contains(&separation),
        "tomb to quake {separation}x"
    );
}

#[test]
fn b5b_is_strict_at_one_e_minus_five() {
    let mut inputs = edge_inputs();
    inputs.x87_pad_bails = 10_000;
    assert!(!ids(&inputs).contains(&"B5b"));
    inputs.x87_pad_bails = 10_001;
    assert!(ids(&inputs).contains(&"B5b"));
}

#[test]
fn b6_is_strict_at_one_hundredth() {
    let mut inputs = edge_inputs();
    inputs.callout_port_v86_served = 10_000_000;
    assert!(!ids(&inputs).contains(&"B6"));
    inputs.callout_port_v86_served = 10_000_001;
    assert!(ids(&inputs).contains(&"B6"));
}

/// prince sits 3.7x below B6's bar and must not be pulled into the polling
/// bucket by it.
#[test]
fn b6_excludes_prince_by_three_point_seven_times() {
    let prince = FIXTURES.iter().find(|f| f.short == "prince-486").unwrap();
    let value = prince.port_v86_served as f64 / prince.instructions as f64;
    let margin = B6_PORT_V86_SERVED_PER_INSN_MIN / value;
    assert!((3.5..4.0).contains(&margin), "prince B6 margin {margin}x");
}

// ---------------------------------------------------------------------------
// Severity and the reported columns
// ---------------------------------------------------------------------------

/// The §4.4 floor. Without it every `HEALTHY-WITH-FINDINGS` row weighs zero
/// and the category deletes itself.
#[test]
fn severity_never_falls_to_zero_on_a_fast_row() {
    let wolf = FIXTURES.iter().find(|f| f.short == "wolf3d-486").unwrap();
    let row = fixture_row(wolf);
    assert_eq!(row.health, "HEALTHY-WITH-FINDINGS");
    assert!(row.real_time_factor > 1.0);
    for hit in &row.bucket_hits {
        assert!(hit.severity >= SEVERITY_FLOOR, "{hit:?}");
    }
}

#[test]
fn severity_intensity_is_capped_at_four_times_the_threshold() {
    let mut inputs = edge_inputs();
    inputs.real_time_factor = 0.0;
    inputs.windowed_smc_heat_demotions = 1_000_000_000;
    let hit = buckets(&inputs)
        .into_iter()
        .find(|hit| hit.id == "B3")
        .unwrap();
    assert!(
        (hit.severity - SEVERITY_INTENSITY_CAP).abs() < 1e-9,
        "{hit:?}"
    );
}

#[test]
fn whole_run_buckets_are_marked_not_windowed() {
    let mut inputs = edge_inputs();
    inputs.callouts = 100_000_000;
    inputs.callout_port_v86_served = 100_000_000;
    inputs.windowed_smc_heat_demotions = 1_000;
    for hit in buckets(&inputs) {
        match hit.id {
            "B3" => assert!(hit.windowed),
            "B4" | "B6" => assert!(!hit.windowed, "{} claimed a window", hit.id),
            _ => {}
        }
    }
}

#[test]
fn the_cut_buckets_survive_as_columns() {
    let profile = Profile {
        wall_seconds: 10.0,
        perf: Perf {
            instructions: 1_000_000,
            decode_misses: 1_000,
            decode_probes: 10_000,
            jit_direct_compile_ns: 1_000_000_000,
            jit_direct_blocks_installed: 50,
            jit_direct_compile_attempts: 100,
            jit_direct_linked_transfers: 400,
            jit_direct_unresolved_exits: 200,
            jit_direct_side_exits: 800,
            ..Perf::default()
        },
        katea: Katea {
            host_wall_ns: 500_000_000,
        },
        ..Profile::default()
    };
    let columns = reported_columns(&profile);
    assert!((columns.b7_decode_misses_per_insn - 0.001).abs() < 1e-12);
    assert!((columns.b7_decode_miss_rate - 0.1).abs() < 1e-12);
    assert!((columns.b8_katea_ratio - 0.05).abs() < 1e-12);
    assert!((columns.b9_compile_ns_ratio - 0.1).abs() < 1e-12);
    assert!((columns.b9_installed_per_attempt - 0.5).abs() < 1e-12);
    assert!((columns.b10_linked_per_side_exit - 0.5).abs() < 1e-12);
    assert!((columns.b10_unresolved_per_side_exit - 0.25).abs() < 1e-12);
}

#[test]
fn a_row_with_no_counters_at_all_divides_by_nothing() {
    let row = classify_archive(&Archive {
        short: "empty".to_string(),
        profile: Some(Profile::default()),
        ..Archive::default()
    });
    assert!(row.buckets.is_empty());
    assert!(row.insns_per_entry.is_finite());
    assert!(row.interpreter_share.is_finite());
    assert!(row.reported.b7_decode_miss_rate.is_finite());
}

#[test]
fn the_tsv_has_one_header_and_one_line_per_row() {
    let rows: Vec<ClassifiedRow> = FIXTURES.iter().map(fixture_row).collect();
    let tsv = rows_to_tsv(&rows);
    let lines: Vec<&str> = tsv.lines().collect();
    assert_eq!(lines.len(), FIXTURES.len() + 1);
    assert!(lines[0].starts_with("short\toutcome\thealth"));
    assert!(lines.iter().any(|line| line.contains("B2|B3|B5b")));
}

// ---------------------------------------------------------------------------
// The on-disk board, when it happens to be there. `.bench/` is git-ignored, so
// this cannot be the gate — it is a cross-check that the embedded table above
// still equals the files it was read from.
// ---------------------------------------------------------------------------

#[test]
fn the_real_fixture_board_still_matches_the_embedded_table() {
    let board = Path::new(".bench/results/scoreboard-20260815-181222-armon/profiles");
    let board = if board.is_dir() {
        board.to_path_buf()
    } else {
        let up = Path::new("../..").join(board);
        if !up.is_dir() {
            eprintln!("skipped: no fixture board on disk");
            return;
        }
        up
    };
    let archives = load_input(&board).expect("fixture board loads");
    assert_eq!(archives.len(), FIXTURES.len());
    for archive in &archives {
        let fixture = FIXTURES
            .iter()
            .find(|f| f.short == archive.short)
            .unwrap_or_else(|| panic!("unexpected fixture {}", archive.short));
        let row = classify_archive(archive);
        assert_eq!(
            row.buckets.join("|"),
            fixture.expected_buckets,
            "{} buckets",
            archive.short
        );
        assert_eq!(row.health, fixture.expected_health, "{}", archive.short);
        let profile = archive.profile.as_ref().unwrap();
        assert_eq!(profile.perf.instructions, fixture.instructions);
        assert_eq!(profile.perf.smc_heat_demotions, fixture.smc_heat_demotions);
    }
}
