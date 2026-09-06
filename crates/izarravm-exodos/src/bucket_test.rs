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
//
// ## v2, 2026-08-17: what changed in the expected column, and why
//
// Two v2 changes touch this table, and NEITHER is a threshold nudge:
//
// 1. **B4 and B6 are one bucket.** Stage 1 measured `port_v86_served` at >=98%
//    of all callouts on 35 of 51 callout rows, so the two rules were reading
//    one mechanism twice and double-counting it into class mass. The merged
//    bucket keeps the id `B4`. The wolf rows therefore read `B3|B4` where v1
//    read `B3|B4|B6`; nothing else about them moved, and
//    `the_b4_b6_merge_is_a_rename_not_a_re_bar` proves the firing SET is
//    identical.
// 2. **B11 (V86-monitor residency) is new**, and it fires on gp2, nascar and
//    tombraid. That is not a regression: those three were already NON-HEALTHY
//    and the new bucket names a cost their existing buckets do not. Their
//    health is unchanged, which is what the acceptance gate is about.
//
// The `monitor`/`core_clocks`/`vec13_trips`/`decode_*` columns are read off the
// same board as the rest of the row.
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
    monitor_clocks: u64,
    core_clocks: u64,
    vec13_trips: u64,
    decode_misses: u64,
    decode_probes: u64,
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
        monitor_clocks: 471_287_454,
        core_clocks: 486_118_346,
        vec13_trips: 1_251,
        decode_misses: 20_585_569,
        decode_probes: 157_841_045,
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
        monitor_clocks: 597_243_071,
        core_clocks: 611_921_090,
        vec13_trips: 337,
        decode_misses: 12_498_503,
        decode_probes: 122_801_180,
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
        monitor_clocks: 59_219,
        core_clocks: 3_320_147_944,
        vec13_trips: 0,
        decode_misses: 9_693_882,
        decode_probes: 123_647_035,
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
        monitor_clocks: 1_541_187,
        core_clocks: 685_475_807,
        vec13_trips: 73_437,
        decode_misses: 263_347,
        decode_probes: 1_319_480_877,
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
        monitor_clocks: 1_077_467_670,
        core_clocks: 1_292_062_044,
        vec13_trips: 21,
        decode_misses: 92_195_454,
        decode_probes: 1_825_706_403,
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
        monitor_clocks: 4_393_811_398,
        core_clocks: 4_605_681_426,
        vec13_trips: 21,
        decode_misses: 169_324_787,
        decode_probes: 7_005_668_257,
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
        monitor_clocks: 1_312_536_125,
        core_clocks: 1_322_652_954,
        vec13_trips: 262_769,
        decode_misses: 64_492_273,
        decode_probes: 589_946_294,
        expected_buckets: "B3|B11",
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
        monitor_clocks: 4_955_753_152,
        core_clocks: 4_966_342_835,
        vec13_trips: 220_123,
        decode_misses: 52_151_157,
        decode_probes: 1_950_509_004,
        expected_buckets: "B4|B11",
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
        monitor_clocks: 14_185_031_068,
        core_clocks: 15_096_983_694,
        vec13_trips: 325_809,
        decode_misses: 239_130_114,
        decode_probes: 4_012_246_769,
        expected_buckets: "B5a|B11",
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
        monitor_clocks: 7_401_634,
        core_clocks: 1_019_289_245,
        vec13_trips: 300_216,
        decode_misses: 10_899_180,
        decode_probes: 277_675_773,
        // v1 read `B3|B4|B6`. B4 and B6 are one bucket in v2; see the table's
        // header note. wolf's vec13 rate is high but its monitor residency is
        // 0.7%, so B11 correctly does NOT fire: wolf's cost is the port itself,
        // not time spent inside the monitor.
        expected_buckets: "B3|B4",
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
        monitor_clocks: 5_378_649,
        core_clocks: 3_713_417_593,
        vec13_trips: 232_825,
        decode_misses: 4_573_694,
        decode_probes: 598_214_121,
        expected_buckets: "B3|B4",
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
        executed_cpu_core_clocks: fixture.core_clocks,
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
            monitor_resident_core_clocks: fixture.monitor_clocks,
            monitor_trips_vec13: fixture.vec13_trips,
            decode_misses: fixture.decode_misses,
            decode_probes: fixture.decode_probes,
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

/// Which buckets the fixture board can prove, and which it cannot.
///
/// v1 asserted that EVERY bucket fires on some fixture. v2 cannot: B7 is
/// restored on corpus evidence alone (`decode_misses/instructions` tops out at
/// 0.0148 on the board, below the 0.05 bar, but reads 0.678 on Drilling — 13.6x
/// over the bar). Splitting the assertion keeps the board's guarantee exact
/// instead of weakening it to "most buckets".
#[test]
fn the_fixture_board_proves_every_bucket_except_the_corpus_only_ones() {
    let mut seen: Vec<String> = FIXTURES
        .iter()
        .flat_map(|fixture| fixture_row(fixture).buckets)
        .collect();
    seen.sort();
    seen.dedup();
    // Lexically sorted, so B11 sorts before B2.
    assert_eq!(seen, ["B1", "B11", "B2", "B3", "B4", "B5a", "B5b"]);

    // And B7 really is unreachable from the board, which is why it needs the
    // corpus. If a future fixture DOES trip it, this fails and the split above
    // stops being necessary.
    let worst = FIXTURES
        .iter()
        .map(|f| f.decode_misses as f64 / f.instructions as f64)
        .fold(0.0f64, f64::max);
    assert!(
        worst < B7_DECODE_MISSES_PER_INSN_MIN,
        "a fixture now trips B7 at {worst}"
    );
    assert!(
        (0.014..0.015).contains(&worst),
        "board's worst B7 is {worst}"
    );
}

/// The B4/B6 merge must be a RENAME, not a re-bar: exactly the rows that fired
/// B4 or B6 in v1 fire the merged bucket in v2, and no other row gains it.
#[test]
fn the_b4_b6_merge_is_a_rename_not_a_re_bar() {
    for fixture in FIXTURES {
        let v1_would_fire = fixture.callouts as f64 / fixture.instructions as f64
            > B4_CALLOUTS_PER_INSN_MIN
            || fixture.port_v86_served as f64 / fixture.instructions as f64
                > B6_PORT_V86_SERVED_PER_INSN_MIN;
        let fires = fixture_row(fixture).buckets.iter().any(|id| id == "B4");
        assert_eq!(
            fires, v1_would_fire,
            "{} changed membership under the merge",
            fixture.short
        );
    }
}

/// B11's separation is carried entirely by the vec13 rate, not by the residency
/// share, and that must be stated where a future campaign will read it.
///
/// `monitor_resident_core_clocks` charges clocks to any instruction that retires
/// while ring-0-protected, so a DOS/4GW game running flat in ring 0 reads ~0.97
/// with no monitor involved at all: doom-486 0.9695 and doom-586 0.9760 sit
/// ABOVE five of the six corpus rows the bucket exists for. The residency share
/// therefore cannot separate healthy from non-healthy, and it is kept only to
/// give the class its meaning. `monitor_trips_vec13/instructions` does the
/// separating: a V86 guest reaches ring 0 only through vector 13.
#[test]
fn b11_separates_on_the_vec13_rate_and_not_on_residency() {
    let vec13 = |short: &str| {
        let f = FIXTURES.iter().find(|f| f.short == short).unwrap();
        f.vec13_trips as f64 / f.instructions as f64
    };
    let share = |short: &str| {
        let f = FIXTURES.iter().find(|f| f.short == short).unwrap();
        f.monitor_clocks as f64 / f.core_clocks as f64
    };

    // The residency share does NOT separate: both doom rows clear the bar.
    assert!(share("doom-486") > B11_MONITOR_SHARE_MIN);
    assert!(share("doom-586") > B11_MONITOR_SHARE_MIN);

    // The vec13 rate does. doom-486 is the highest excluded fixture.
    let highest_excluded = vec13("doom-486");
    assert!(highest_excluded < B11_VEC13_TRIPS_PER_INSN_MIN);
    let margin = B11_VEC13_TRIPS_PER_INSN_MIN / highest_excluded;
    assert!(
        (3.5..4.0).contains(&margin),
        "doom-486 sits {margin}x below the B11 vec13 bar"
    );

    // And the lowest corpus row the bucket exists for, conqstND, clears the bar
    // by a matching margin: 175_038 trips over 23_951_973_632 instructions.
    let conqstnd = 175_038.0 / 23_951_973_632.0;
    let corpus_margin = conqstnd / B11_VEC13_TRIPS_PER_INSN_MIN;
    assert!(
        (3.5..4.0).contains(&corpus_margin),
        "conqstND sits {corpus_margin}x above the B11 vec13 bar"
    );
}

/// prince and the wolf rows have HIGH vec13 rates and must still not enter B11:
/// their monitor residency is under 1%, so the trips are cheap.
#[test]
fn b11_excludes_prince_and_wolf_on_the_residency_clause() {
    for short in ["prince-486", "wolf3d-486", "wolf3d-586"] {
        let f = FIXTURES.iter().find(|f| f.short == short).unwrap();
        let vec13 = f.vec13_trips as f64 / f.instructions as f64;
        let share = f.monitor_clocks as f64 / f.core_clocks as f64;
        assert!(
            vec13 > B11_VEC13_TRIPS_PER_INSN_MIN,
            "{short} vec13 {vec13}"
        );
        assert!(share < B11_MONITOR_SHARE_MIN, "{short} share {share}");
        assert!(!fixture_row(f).buckets.iter().any(|id| id == "B11"));
    }
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
        decide_outcome(
            Some(&thirty),
            &[],
            &FrameFacts::default(),
            HostVerdict::default()
        ),
        Outcome::ShortRun
    );
    let thirty_one = ran_profile(31);
    assert_eq!(
        decide_outcome(
            Some(&thirty_one),
            &[],
            &FrameFacts::default(),
            HostVerdict::default()
        ),
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
        decide_outcome(
            Some(&profile),
            &[],
            &FrameFacts::default(),
            HostVerdict::default()
        ),
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
        presented: true,
        hash: Some(hash.to_string()),
        changed: false,
        ppm: None,
        text_glyphs: None,
    }
}

#[test]
fn a_crash_beats_every_other_signal() {
    let mut profile = ran_profile(31);
    profile.stop.kind = "cpu_error".to_string();
    assert_eq!(
        decide_outcome(
            Some(&profile),
            &[],
            &FrameFacts::default(),
            HostVerdict::default()
        ),
        Outcome::Crashed
    );
}

/// **The v1 REBOOT-LOOP defect, pinned as a test.**
///
/// v1 called a row a reboot loop when the OPENING FRAME HASH recurred twice.
/// Stage 1 measured that rule at 0/8 true positives: a blinking text cursor, an
/// attract cycle and a black fade frame all return to `screens[0]`, and the
/// check ran first in `decide_outcome`, so it also excluded four rows that were
/// otherwise fine. The recurrence count survives as a reported column; it no
/// longer decides anything.
#[test]
fn a_recurring_opening_frame_is_no_longer_a_reboot_loop() {
    let screens = vec![
        screen(0, "aaa", Some("modex")),
        screen(1, "bbb", Some("modex")),
        screen(2, "aaa", Some("modex")),
        screen(3, "bbb", Some("modex")),
        screen(4, "aaa", Some("modex")),
    ];
    // The v1 detector still reads 2 — the count is kept, the verdict is not.
    assert_eq!(screen_recurrences(&screens), 2);
    assert_eq!(
        decide_outcome(
            Some(&ran_profile(31)),
            &screens,
            &FrameFacts::default(),
            HostVerdict::default()
        ),
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
    let picture = screen_window(&screens, &profile, &FrameFacts::default());
    assert!(picture.flat, "{picture:?}");
    assert_eq!(
        decide_outcome(
            Some(&profile),
            &screens,
            &FrameFacts::default(),
            HostVerdict::default()
        ),
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
        decide_outcome(
            Some(&profile),
            &screens,
            &FrameFacts::default(),
            HostVerdict::default()
        ),
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
    assert!(!screen_window(&screens, &profile, &FrameFacts::default()).flat);
}

/// A Margo-framebuffer title has no mode line at all. v1 called that
/// `IDLE-BLIND`; v2 calls it `NO-MODE-LINE`, because it is a statement about the
/// DISPLAY PATH and stage 1 read the name as a statement about the picture. See
/// `idle_blind_now_names_a_blank_picture`.
#[test]
fn a_margo_display_is_flagged_no_mode_line() {
    let profile = ran_profile(31);
    let end = profile.master_ticks;
    let screens: Vec<Screen> = (0..4)
        .map(|index| Screen {
            master_ticks: end - index * DOOM_HZ,
            display: "margo".to_string(),
            hash: Some(format!("h{index}")),
            ..screen(index, "unused", None)
        })
        .collect();
    let row = classify_archive(&Archive {
        short: "margo".to_string(),
        profile: Some(profile),
        screens,
        ..Archive::default()
    });
    assert!(row.flags.iter().any(|flag| flag == "NO-MODE-LINE"));
    assert!(!row.flags.iter().any(|flag| flag == "IDLE-BLIND"));
    assert_eq!(row.outcome, "RAN");
}

#[test]
fn host_side_verdicts_win_over_the_profile() {
    let profile = ran_profile(31);
    assert_eq!(
        decide_outcome(
            Some(&profile),
            &[],
            &FrameFacts::default(),
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
            &FrameFacts::default(),
            HostVerdict {
                timed_out: true,
                ..HostVerdict::default()
            }
        ),
        Outcome::HungHost
    );
    assert_eq!(
        decide_outcome(None, &[], &FrameFacts::default(), HostVerdict::default()),
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
        let outcome = decide_outcome(
            Some(&profile),
            &[],
            &FrameFacts::default(),
            HostVerdict::default(),
        );
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
        windowed_decode_misses: 0,
        instructions: 1_000_000_000,
        callouts: 0,
        side_exit_x87_eligibility: 0,
        x87_pad_bails: 0,
        callout_port_v86_served: 0,
        monitor_resident_core_clocks: 0,
        monitor_trips_vec13: 0,
        core_clocks: 1_000_000_000,
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

/// B7, restored. The bar is the design's original 0.05; nothing about it was
/// re-derived. See `the_fixture_board_proves_every_bucket_except_the_corpus_only_ones`
/// for why the board cannot prove it.
#[test]
fn b7_is_restored_and_strict_at_five_hundredths() {
    let mut inputs = edge_inputs();
    inputs.windowed_decode_misses = 50_000_000;
    assert!(!ids(&inputs).contains(&"B7"));
    inputs.windowed_decode_misses = 50_000_001;
    assert!(ids(&inputs).contains(&"B7"));
}

/// The corpus row that earns B7 its restoration: Drilling reads 0.678, which is
/// 13.6x the bar, and it is a RAN row at rt 0.11 — a slow row whose only other
/// bucket is B2.
#[test]
fn b7_fires_on_the_corpus_row_that_earned_its_restoration() {
    let mut inputs = edge_inputs();
    inputs.instructions = 11_784_504_924;
    inputs.windowed_instructions = 11_784_504_924;
    inputs.windowed_jit_direct_insns = 11_784_504_924;
    inputs.windowed_decode_misses = 7_991_683_066;
    let value = inputs.windowed_decode_misses as f64 / inputs.instructions as f64;
    let margin = value / B7_DECODE_MISSES_PER_INSN_MIN;
    assert!(
        (13.5..13.7).contains(&margin),
        "Drilling's B7 margin is {margin}x, §2 recorded 13.6x"
    );
    assert!(ids(&inputs).contains(&"B7"));
}

/// B11 is a conjunction and strict on each clause.
#[test]
fn b11_is_a_conjunction_and_strict_on_each_clause() {
    let mut inputs = edge_inputs();
    inputs.core_clocks = 1_000_000_000;

    // Residency exactly on the bar: the clause is `>`, so no.
    inputs.monitor_resident_core_clocks = 500_000_000;
    inputs.monitor_trips_vec13 = 1_000_000; // 1e-3 per instruction, far over
    assert!(!ids(&inputs).contains(&"B11"));
    inputs.monitor_resident_core_clocks = 500_000_001;
    assert!(ids(&inputs).contains(&"B11"));

    // Trips exactly on the bar with residency clear: still no.
    inputs.monitor_trips_vec13 = 2_000; // 2_000 / 1e9 = 2e-6 exactly
    assert!(!ids(&inputs).contains(&"B11"));
    inputs.monitor_trips_vec13 = 2_001;
    assert!(ids(&inputs).contains(&"B11"));

    // Neither clause alone is enough.
    let mut residency_only = edge_inputs();
    residency_only.core_clocks = 1_000_000_000;
    residency_only.monitor_resident_core_clocks = 999_000_000;
    residency_only.monitor_trips_vec13 = 0;
    assert!(!ids(&residency_only).contains(&"B11"));
}

/// A profile with no core-clock total cannot claim a residency share.
#[test]
fn b11_never_divides_by_a_missing_core_clock_total() {
    let mut inputs = edge_inputs();
    inputs.core_clocks = 0;
    inputs.monitor_resident_core_clocks = 1_000_000;
    inputs.monitor_trips_vec13 = 1_000_000;
    assert!(!ids(&inputs).contains(&"B11"));
}

/// B4's v86 clause — formerly bucket B6 — keeps its own bar and its own
/// strictness after the merge, and a row that trips only that clause reports the
/// v86 metric rather than the callout one.
#[test]
fn the_v86_clause_of_b4_is_strict_at_one_hundredth() {
    let mut inputs = edge_inputs();
    inputs.callout_port_v86_served = 10_000_000;
    assert!(!ids(&inputs).contains(&"B4"));
    inputs.callout_port_v86_served = 10_000_001;
    assert!(ids(&inputs).contains(&"B4"));

    // No B6 survives the merge, whatever the counters say.
    assert!(!ids(&inputs).contains(&"B6"));

    let hit = buckets(&inputs)
        .into_iter()
        .find(|hit| hit.id == "B4")
        .unwrap();
    assert_eq!(
        hit.metric, "jit_direct_callout_port_v86_served/instructions",
        "the dominant clause names itself"
    );
    assert!((hit.threshold - B6_PORT_V86_SERVED_PER_INSN_MIN).abs() < 1e-12);
}

/// And a row over the callout bar with no v86 traffic reports the callout
/// metric, so the merged bucket never hides which end to pull.
#[test]
fn the_callout_clause_of_b4_reports_itself_when_it_dominates() {
    let mut inputs = edge_inputs();
    inputs.callouts = 100_000_000;
    inputs.callout_port_v86_served = 0;
    let hit = buckets(&inputs)
        .into_iter()
        .find(|hit| hit.id == "B4")
        .unwrap();
    assert_eq!(
        hit.metric,
        "(callout_executed + step_break + abnormal)/instructions"
    );
    assert!((hit.threshold - B4_CALLOUTS_PER_INSN_MIN).abs() < 1e-12);
}

/// prince sits 3.7x below the v86 bar and must not be pulled into the merged
/// polling bucket by it.
#[test]
fn the_v86_clause_excludes_prince_by_three_point_seven_times() {
    let prince = FIXTURES.iter().find(|f| f.short == "prince-486").unwrap();
    let value = prince.port_v86_served as f64 / prince.instructions as f64;
    let margin = B6_PORT_V86_SERVED_PER_INSN_MIN / value;
    assert!((3.5..4.0).contains(&margin), "prince v86 margin {margin}x");
    assert_eq!(fixture_row(prince).buckets.join("|"), "B1");
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
// Frame evidence: the pixels behind the reboot rule and the flatness rule.
//
// v1 decided both from the frame HASH alone, and one blinking text cursor
// defeated both at once — it manufactured a reboot loop out of a DOS prompt and
// it hid eight idle rows inside the bucketable set by making their window read
// "two distinct pictures". Both rules now read the pixels the archive kept.
// ---------------------------------------------------------------------------

/// A solid frame of one colour.
fn solid(width: usize, height: usize, colour: [u8; 3]) -> FrameImage {
    FrameImage {
        width,
        height,
        rgb: colour
            .iter()
            .copied()
            .cycle()
            .take(width * height * 3)
            .collect(),
    }
}

/// Paint one pixel a different colour.
fn poke(image: &mut FrameImage, x: usize, y: usize, colour: [u8; 3]) {
    let offset = (y * image.width + x) * 3;
    image.rgb[offset..offset + 3].copy_from_slice(&colour);
}

fn ppm_bytes(image: &FrameImage) -> Vec<u8> {
    let mut out = format!("P6\n{} {}\n255\n", image.width, image.height).into_bytes();
    out.extend_from_slice(&image.rgb);
    out
}

#[test]
fn a_screendump_ppm_round_trips_through_the_reader() {
    let mut image = solid(8, 4, [0, 0, 0]);
    poke(&mut image, 3, 2, [255, 128, 64]);
    let read = read_ppm(&ppm_bytes(&image)).expect("parses");
    assert_eq!((read.width, read.height), (8, 4));
    assert_eq!(read.rgb, image.rgb);
}

#[test]
fn a_truncated_or_foreign_ppm_is_refused_rather_than_guessed() {
    assert!(read_ppm(b"P3\n8 4\n255\n").is_none());
    assert!(read_ppm(b"P6\n8 4\n255\n\x00\x00").is_none());
    assert!(read_ppm(b"").is_none());
    // E8 in the stage-1 ledger: the dumper emitted a degenerate 1x1 PPM once.
    // It parses, and it must not be mistaken for anything.
    let tiny = read_ppm(b"P6\n1 1\n255\n\x00\x00\x00").expect("1x1 parses");
    assert_eq!((tiny.width, tiny.height), (1, 1));
    assert!(tiny.banner_digest().is_none());
}

/// **Defect E8, guarded.** A one-pixel frame is vacuously "one colour", so a
/// degenerate capture would otherwise report a blank screen and raise
/// `IDLE-BLIND` on a title whose screen was fine. No Vega mode is smaller than
/// 320x200, so a frame below `MIN_FRAME_PIXELS` is a capture defect and is
/// treated as unreadable evidence rather than as evidence of blankness.
#[test]
fn a_degenerate_capture_is_not_evidence_of_a_blank_screen() {
    let degenerate = solid(1, 1, [0, 0, 0]);
    assert!(degenerate.blank(), "one pixel is vacuously one colour");
    assert!(
        !degenerate.usable(),
        "and it must not count as usable evidence"
    );
    // The smallest mode the Vega BIOS presents does.
    assert!(solid(320, 200, [0, 0, 0]).usable());
}

// -- the boot banner ---------------------------------------------------------

/// The banner region is the TOP TEN TEXT ROWS of the 720x400 boot screen, and
/// nothing below them.
///
/// MEASURED 2026-08-17 by booting `--hdd-folder` with a 200 ms screen dump: the
/// Toka-DOS boot screen paints a fixed ASCII logo across text rows 0-9 (160
/// pixel rows at 16 px per row), then the kernel's build-date box, then the
/// per-game CONFIG.SYS and AUTOEXEC echo. Only the logo is invariant, so only
/// the logo is hashed: the build-date box moves whenever the image is rebuilt
/// and the echo differs per game.
#[test]
fn the_banner_digest_covers_the_top_ten_text_rows_and_nothing_below() {
    let base = solid(BOOT_BANNER_WIDTH, BOOT_BANNER_HEIGHT, [0, 0, 0]);
    let reference = base.banner_digest().expect("720x400 has a digest");

    // A change BELOW the banner region does not move the digest. This is what
    // lets one reference match 14 different games' boot screens.
    let mut below = base.clone();
    poke(&mut below, 0, BOOT_BANNER_ROWS, [255, 255, 255]);
    assert_eq!(below.banner_digest(), Some(reference));

    // A change INSIDE it does.
    let mut inside = base.clone();
    poke(&mut inside, 0, BOOT_BANNER_ROWS - 1, [255, 255, 255]);
    assert_ne!(inside.banner_digest(), Some(reference));
}

/// Only the boot screen's own geometry can carry the banner. A 640x480 graphics
/// frame has no banner digest at all, so no graphics frame can ever be mistaken
/// for a boot screen however its bytes happen to hash.
#[test]
fn only_the_boot_screens_geometry_carries_a_banner_digest() {
    assert!(solid(640, 480, [0, 0, 0]).banner_digest().is_none());
    assert!(solid(320, 200, [0, 0, 0]).banner_digest().is_none());
    assert!(solid(720, 400, [0, 0, 0]).banner_digest().is_some());
}

#[test]
fn boot_banner_entries_count_arrivals_at_the_boot_screen_not_samples_of_it() {
    let banner = |present: bool| if present { "boot" } else { "game" }.to_string();
    let facts = FrameFacts {
        banner: [("boot".to_string(), true), ("game".to_string(), false)]
            .into_iter()
            .collect(),
        ..FrameFacts::default()
    };
    let series = |states: &[bool]| -> Vec<Screen> {
        states
            .iter()
            .enumerate()
            .map(|(index, present)| Screen {
                hash: Some(banner(*present)),
                ..screen(index as u64, "unused", Some("text"))
            })
            .collect()
    };

    // The banner is on screen for the whole run and never leaves: ONE boot. This
    // is `billted` and `rogclon`, two of the eight v1 false positives — their
    // whole run sits at the DOS prompt under the banner.
    assert_eq!(boot_banner_entries(&series(&[true; 6]), &facts), 1);

    // A game that never shows it: no boot observed after the sampler started.
    assert_eq!(boot_banner_entries(&series(&[false; 6]), &facts), 0);

    // Booted, ran, booted again, ran again: TWO arrivals. This is a reboot loop.
    assert_eq!(
        boot_banner_entries(&series(&[true, false, false, true, false]), &facts),
        2
    );

    // Three arrivals.
    assert_eq!(
        boot_banner_entries(&series(&[false, true, false, true, false, true]), &facts),
        3
    );
}

#[test]
fn two_arrivals_at_the_boot_banner_are_a_reboot_loop() {
    let facts = FrameFacts {
        banner: [("boot".to_string(), true), ("game".to_string(), false)]
            .into_iter()
            .collect(),
        ..FrameFacts::default()
    };
    let screens: Vec<Screen> = [true, false, false, true, false]
        .iter()
        .enumerate()
        .map(|(index, boot)| Screen {
            hash: Some(if *boot { "boot" } else { "game" }.to_string()),
            ..screen(index as u64, "unused", Some("text"))
        })
        .collect();
    assert_eq!(
        decide_outcome(
            Some(&ran_profile(31)),
            &screens,
            &facts,
            HostVerdict::default()
        ),
        Outcome::RebootLoop
    );
}

// -- the pixel-delta floor --------------------------------------------------

#[test]
fn differing_pixels_counts_pixels_and_refuses_a_geometry_mismatch() {
    let base = solid(10, 10, [0, 0, 0]);
    let mut one = base.clone();
    poke(&mut one, 5, 5, [1, 0, 0]);
    assert_eq!(base.differing_pixels(&one), Some(1));
    assert_eq!(base.differing_pixels(&base), Some(0));
    assert_eq!(base.differing_pixels(&solid(10, 11, [0, 0, 0])), None);
}

/// **The measurement that sets the floor.**
///
/// MEASURED over all 203 archived stage-1 rows, pairwise between every distinct
/// in-window frame. A blinking text cursor in 720x400 text mode differs by
/// exactly **18 pixels** of 288,000 — the 9x2 underline cell — and that one
/// number accounts for eleven of the corpus's two-frame rows. The widest pair
/// the floor absorbs is TSpaFarm at 464 px (0.161%, 1.55x below the bar); the
/// narrowest it rejects is TGinRum at 1,035 px (0.359%, 1.44x above). The bar
/// sits at 0.25% of the frame, which is five 9x16 character cells.
#[test]
fn the_pixel_delta_floor_sits_between_the_measured_corpus_pairs() {
    let frame = 288_000.0;
    let cursor_blink = 18.0 / frame;
    let widest_absorbed = 464.0 / frame;
    let narrowest_rejected = 1_035.0 / frame;

    assert!(cursor_blink < PIXEL_DELTA_FLOOR);
    assert!(widest_absorbed < PIXEL_DELTA_FLOOR);
    assert!(narrowest_rejected > PIXEL_DELTA_FLOOR);

    let below = PIXEL_DELTA_FLOOR / widest_absorbed;
    let above = narrowest_rejected / PIXEL_DELTA_FLOOR;
    assert!((1.5..1.6).contains(&below), "margin below the bar {below}x");
    assert!((1.4..1.5).contains(&above), "margin above the bar {above}x");

    // And the cursor itself sits 40x below the bar, so no plausible cursor shape
    // comes near it: 18 px against the bar's 720.
    let cursor_margin = PIXEL_DELTA_FLOOR / cursor_blink;
    assert!(
        (39.9..40.1).contains(&cursor_margin),
        "the cursor sits {cursor_margin}x below the bar"
    );
}

fn blink_facts(a: &str, b: &str, differing: usize, frame: usize) -> FrameFacts {
    FrameFacts {
        delta: [(
            (a.to_string(), b.to_string()),
            differing as f64 / frame as f64,
        )]
        .into_iter()
        .collect(),
        ..FrameFacts::default()
    }
}

#[test]
fn a_cursor_blink_pair_is_one_picture_and_a_real_change_is_two() {
    let blink = blink_facts("a", "b", 18, 288_000);
    assert!(blink.same_picture("a", "b"));
    assert!(blink.same_picture("b", "a"), "the relation is symmetric");

    let change = blink_facts("a", "b", 1_035, 288_000);
    assert!(!change.same_picture("a", "b"));

    // An unmeasured pair is never asserted to be the same picture.
    assert!(!FrameFacts::default().same_picture("a", "b"));
    // A frame is always the same picture as itself, measured or not.
    assert!(FrameFacts::default().same_picture("a", "a"));
}

/// Classes must not CHAIN. Two frames that are each within the floor of a middle
/// frame, but far from each other, are two pictures and not one — otherwise a
/// slow pan across 13 samples would collapse into "the picture never changed".
#[test]
fn frame_classes_never_chain_two_far_apart_pictures() {
    let facts = FrameFacts {
        delta: [
            (("a".to_string(), "b".to_string()), 0.0001),
            (("b".to_string(), "c".to_string()), 0.0001),
            (("a".to_string(), "c".to_string()), 0.02),
        ]
        .into_iter()
        .collect(),
        ..FrameFacts::default()
    };
    let screens: Vec<Screen> = ["a", "b", "c"]
        .iter()
        .enumerate()
        .map(|(index, hash)| Screen {
            hash: Some(hash.to_string()),
            ..screen(index as u64, "unused", Some("text"))
        })
        .collect();
    let classes = frame_classes(&screens, &facts);
    assert_eq!(classes[0], classes[1], "a and b are one picture");
    assert_ne!(classes[0], classes[2], "a and c are not");
    assert_eq!(distinct_count(&classes), 2);
}

#[test]
fn identical_hashes_are_one_picture_without_any_pixel_evidence() {
    let screens: Vec<Screen> = (0..4)
        .map(|index| Screen {
            hash: Some("same".to_string()),
            ..screen(index, "unused", Some("text"))
        })
        .collect();
    let classes = frame_classes(&screens, &FrameFacts::default());
    assert_eq!(distinct_count(&classes), 1);
}

/// **The second half of the v1 defect, pinned.** Eight rows sat at a DOS prompt
/// for the whole window with a blinking cursor. Their window held two distinct
/// HASHES, so v1's `distinct <= 1` flatness test failed, the rows were called
/// `RAN`, and they entered the bucketable set — where seven of them fired B2 on
/// boot-phase-shaped counters. With the pixel-delta floor the two hashes are one
/// picture, the window is flat, and a flat TEXT picture is `IDLE-TEXT`.
#[test]
fn a_blinking_cursor_at_a_dos_prompt_is_idle_text_not_a_bucketable_row() {
    let mut profile = ran_profile(31);
    let last = profile.phase_marks.len() - 1;
    profile.phase_marks[last].halted_ticks = profile.phase_marks[last].master_ticks;
    let end = profile.master_ticks;
    // Four in-window samples alternating between the two cursor phases.
    let screens: Vec<Screen> = (0..4)
        .map(|index| Screen {
            master_ticks: end - index * DOOM_HZ,
            hash: Some(if index % 2 == 0 { "on" } else { "off" }.to_string()),
            ..screen(index, "unused", Some("text"))
        })
        .collect();
    let facts = blink_facts("off", "on", 18, 288_000);

    // v1's view: two distinct hashes, therefore not flat, therefore RAN.
    let v1 = screen_window(&screens, &profile, &FrameFacts::default());
    assert_eq!(v1.distinct_in_window, 2);
    assert!(!v1.flat);

    // v2's view: one picture, flat, and excluded from every bucket.
    let v2 = screen_window(&screens, &profile, &facts);
    assert_eq!(v2.distinct_in_window, 2, "the hash count is still reported");
    assert_eq!(v2.distinct_pictures_in_window, 1);
    assert!(v2.flat);

    let row = classify_archive(&Archive {
        short: "prompt".to_string(),
        profile: Some(profile),
        screens,
        frames: facts,
        ..Archive::default()
    });
    assert_eq!(row.outcome, "IDLE-TEXT");
    assert_eq!(row.health, "EXCLUDED");
    assert!(row.buckets.is_empty());
}

// -- blankness --------------------------------------------------------------

/// `IDLE-BLIND` now means the picture really is blank.
///
/// v1 raised it whenever the sample carried no `video_mode` line, which is a
/// fact about the DISPLAY PATH (the Margo framebuffer) and not about the
/// picture. Stage 1 read the flag as "the screen is blank" and needed that
/// signal for real — defect E6 is a title showing a blank text screen while
/// 29.2 billion instructions spin on port reads. The display-path fact keeps its
/// own flag, `NO-MODE-LINE`.
#[test]
fn idle_blind_now_names_a_blank_picture() {
    let profile = ran_profile(31);
    let end = profile.master_ticks;
    let screens: Vec<Screen> = (0..4)
        .map(|index| Screen {
            master_ticks: end - index * DOOM_HZ,
            hash: Some("black".to_string()),
            ..screen(index, "unused", Some("text"))
        })
        .collect();
    let facts = FrameFacts {
        blank: [("black".to_string(), true)].into_iter().collect(),
        ..FrameFacts::default()
    };
    let row = classify_archive(&Archive {
        short: "blank".to_string(),
        profile: Some(profile.clone()),
        screens: screens.clone(),
        frames: facts,
        ..Archive::default()
    });
    assert!(row.flags.iter().any(|flag| flag == "IDLE-BLIND"));
    assert!(!row.flags.iter().any(|flag| flag == "NO-MODE-LINE"));

    // A picture with content in the same display path is not blind.
    let painted = classify_archive(&Archive {
        short: "painted".to_string(),
        profile: Some(profile),
        screens,
        frames: FrameFacts {
            blank: [("black".to_string(), false)].into_iter().collect(),
            ..FrameFacts::default()
        },
        ..Archive::default()
    });
    assert!(!painted.flags.iter().any(|flag| flag == "IDLE-BLIND"));
}

#[test]
fn blankness_is_one_colour_across_the_whole_frame() {
    assert!(solid(8, 8, [0, 0, 0]).blank());
    assert!(
        solid(8, 8, [17, 34, 51]).blank(),
        "one colour, not just black"
    );
    let mut painted = solid(8, 8, [0, 0, 0]);
    poke(&mut painted, 4, 4, [1, 0, 0]);
    assert!(!painted.blank());
}

// -- samples with no frame at all -------------------------------------------

/// The dumper emits a sample whose frame is absent when the guest has not
/// completed one: before the first raster, and for up to a frame period after
/// every mode set. Such a line carries `"presented": false` and a null hash.
///
/// It is NOT an observation of a blank screen; it is the absence of an
/// observation. Before the dumper was fixed it wrote a one-pixel black image
/// instead, and 30 of the archive's frames are that image.
#[test]
fn a_sample_with_no_frame_is_not_an_observation() {
    let line = r#"{"i":3,"master_ticks":100,"guest_ms":1,"display":"vga",
        "video_mode":"text","presented":false,"hash":null,"changed":false,
        "ppm":null,"text_glyphs":null}"#;
    let sample: Screen = serde_json::from_str(&line.replace('\n', "")).expect("parses");
    assert!(!sample.presented);
    assert_eq!(sample.frame_hash(), None);

    // And an archive written before the field existed reads as an observation,
    // because every line it wrote had a frame behind it.
    let old = r#"{"i":0,"master_ticks":1,"guest_ms":0,"display":"vga",
        "video_mode":"text","hash":"abc","changed":true,"ppm":"0000.ppm",
        "text_glyphs":12}"#;
    let sample: Screen = serde_json::from_str(&old.replace('\n', "")).expect("parses");
    assert!(sample.presented);
    assert_eq!(sample.frame_hash(), Some("abc"));
}

fn unobserved(index: u64) -> Screen {
    Screen {
        presented: false,
        hash: None,
        ..screen(index, "unused", Some("text"))
    }
}

/// A gap in the samples must not break a run of banner samples into two
/// arrivals. A reboot loop is two BOOTS, and a sample the dumper could not take
/// is not a departure from the boot screen.
#[test]
fn a_sample_with_no_frame_does_not_manufacture_a_boot_arrival() {
    let facts = FrameFacts {
        banner: [("boot".to_string(), true)].into_iter().collect(),
        ..FrameFacts::default()
    };
    let banner = |index: u64| Screen {
        hash: Some("boot".to_string()),
        ..screen(index, "unused", Some("text"))
    };
    let screens = vec![
        banner(0),
        unobserved(1),
        banner(2),
        unobserved(3),
        banner(4),
    ];
    assert_eq!(
        boot_banner_entries(&screens, &facts),
        1,
        "one boot, sampled across two gaps"
    );
    assert_eq!(
        decide_outcome(
            Some(&ran_profile(31)),
            &screens,
            &facts,
            HostVerdict::default()
        ),
        Outcome::IdleText,
        "and it is certainly not a reboot loop"
    );
}

/// Nor can a gap count as a picture, in either direction: it must not add a
/// distinct picture to a flat window, and it must not fill the three samples
/// flatness needs.
#[test]
fn a_sample_with_no_frame_counts_as_neither_a_picture_nor_a_sample() {
    let profile = ran_profile(31);
    let end = profile.master_ticks;
    let at = |index: u64, sample: Screen| Screen {
        master_ticks: end - index * DOOM_HZ,
        ..sample
    };
    let still = |index: u64| Screen {
        hash: Some("same".to_string()),
        ..screen(index, "unused", Some("text"))
    };

    // Three real samples of one picture, with two gaps mixed in: still flat.
    let screens: Vec<Screen> = vec![
        at(0, still(0)),
        at(1, unobserved(1)),
        at(2, still(2)),
        at(3, unobserved(3)),
        at(4, still(4)),
    ];
    let picture = screen_window(&screens, &profile, &FrameFacts::default());
    assert_eq!(picture.samples_in_window, 3, "gaps are not samples");
    assert_eq!(picture.distinct_pictures_in_window, 1);
    assert!(picture.flat);

    // Two real samples and two gaps: not enough to assert flatness.
    let thin: Vec<Screen> = vec![
        at(0, still(0)),
        at(1, unobserved(1)),
        at(2, still(2)),
        at(3, unobserved(3)),
    ];
    let picture = screen_window(&thin, &profile, &FrameFacts::default());
    assert_eq!(picture.samples_in_window, 2);
    assert!(!picture.flat);
}

/// A row whose frames could not be read says so rather than guessing. Without
/// pixels the delta is unknown, so no pair collapses and the row keeps its v1
/// hash-count behaviour.
#[test]
fn unreadable_frames_are_flagged_and_never_collapsed() {
    let profile = ran_profile(31);
    let end = profile.master_ticks;
    let screens: Vec<Screen> = (0..4)
        .map(|index| Screen {
            master_ticks: end - index * DOOM_HZ,
            hash: Some(if index % 2 == 0 { "on" } else { "off" }.to_string()),
            ..screen(index, "unused", Some("text"))
        })
        .collect();
    let facts = FrameFacts {
        unreadable: ["on".to_string()].into_iter().collect(),
        ..FrameFacts::default()
    };
    let row = classify_archive(&Archive {
        short: "unreadable".to_string(),
        profile: Some(profile),
        screens,
        frames: facts,
        ..Archive::default()
    });
    assert!(row.flags.iter().any(|flag| flag == "FRAMES-UNREADABLE"));
    assert_eq!(row.screens.distinct_pictures_in_window, 2);
    assert!(!row.screens.flat);
}

// ---------------------------------------------------------------------------
// Optional archive-loader cross-check against the original fixture board.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires the archived scoreboard-20260815-181222-armon fixture board"]
fn the_real_fixture_board_still_matches_the_embedded_table() {
    let board = Path::new(".bench/results/scoreboard-20260815-181222-armon/profiles");
    let board = if board.is_dir() {
        board.to_path_buf()
    } else {
        let up = Path::new("../..").join(board);
        assert!(up.is_dir(), "missing fixture board: {}", up.display());
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
        assert_eq!(profile.perf.monitor_trips_vec13, fixture.vec13_trips);
        assert_eq!(
            profile.perf.monitor_resident_core_clocks,
            fixture.monitor_clocks
        );
        assert_eq!(profile.executed_cpu_core_clocks, fixture.core_clocks);
        assert_eq!(profile.perf.decode_misses, fixture.decode_misses);
    }
}
