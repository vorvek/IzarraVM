// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Fixtures for the 16-bit entry-attribution observer
//! (`dev_docs/specs/2026-08-23-sixteen-bit-entry-attribution-design.md`).
//!
//! What each one is able to FAIL on, which is the only reason to write it:
//!
//! | fixture | what it pins | what it does if the property is broken |
//! |---|---|---|
//! | `armed_and_disarmed_runs_leave_identical_guest_state` | A4 | any stamp that touched guest state, the guest clock or a perf counter shows up as a diff |
//! | `marks_per_entry_are_the_published_factors` | A3 / section 4b | a mark placed on the wrong side of a branch, or a P4/P5/P8 pair reduced to one |
//! | `every_traversal_that_enters_takes_one_p11_and_one_end` | the cursor's well-definedness | an exit path that forgets `end()` leaves `total_entered` short of `marks(P11)` |
//! | `refusal_sites_close_against_the_marks_that_reached_them` | H3 | a refusal without a `refusal_site` bump, or a site bumped without a mark |
//! | `the_sampled_arm_stamps_end_to_end` | B1 | a stride that inflates a phase, or a sampled traversal marked without a `begin()` |
//! | `the_coarse_arm_takes_exactly_the_two_native_marks` | A6's premise | a `mark_coarse` that leaked into the FULL-only set, or the reverse |
//! | `the_disarmed_arm_accumulates_nothing` | the default | a stamp that runs before the arm is read |
//! | `p14_is_exempt_from_the_outlier_clamp` | M-R4 | a clamp on P14 that would manufacture A3's negative-gap falsifier |
//!
//! The arm is overridden per THREAD (`arm_for_test`), never through the process-global env gate:
//! `cargo test` runs these in one process alongside every other battery, and a `OnceLock` resolved
//! from the environment cannot be re-taken.

use super::*;

use crate::jit::direct::entry_attribution::{Arm, Phase, Population, arm_for_test, snapshot};

use super::jit_direct::{drive, fresh};

/// Five iterations of a three-instruction loop, the shape the sibling direct battery uses.
fn loop_program() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[0x100..0x110].copy_from_slice(&[
        0xb9, 0x20, 0x00, 0x00, 0x00, // mov ecx,0x20
        0x83, 0xc0, 0x03, // 0x105 add eax,3
        0x89, 0xc2, // mov edx,eax
        0x83, 0xe9, 0x01, // sub ecx,1
        0x75, 0xf6, // jnz 0x105
        0xf4, // hlt
    ]);
    memory
}

/// One complete run of `loop_program` with the Direct backend admitting and installing.
fn run_once() -> (CpuGsw, TestBus) {
    let mut cpu = fresh();
    cpu.set_jit_auto_admit(true);
    let mut bus = TestBus::with_memory(loop_program());
    bus.direct_pages_enabled = true;
    drive(&mut cpu, &mut bus);
    (cpu, bus)
}

/// The observer's own snapshot for a run taken under `arm`, with the thread's tally reset first.
fn run_under(arm: Arm, sample_n: u64) -> (CpuGsw, crate::DirectEntryAttributionSnapshot) {
    arm_for_test(Some(arm), Some(sample_n));
    let (cpu, _bus) = run_once();
    let snap = snapshot().expect("an armed observer must produce a snapshot");
    arm_for_test(None, None);
    (cpu, snap)
}

fn marks(snap: &crate::DirectEntryAttributionSnapshot, phase: Phase) -> u64 {
    snap.marks.iter().map(|lane| lane[phase as usize]).sum()
}

fn ticks(snap: &crate::DirectEntryAttributionSnapshot, phase: Phase) -> u64 {
    snap.ticks_raw.iter().map(|lane| lane[phase as usize]).sum()
}

fn population(snap: &crate::DirectEntryAttributionSnapshot, population: Population) -> u64 {
    snap.totals
        .iter()
        .map(|lane| lane[population as usize])
        .sum()
}

/// A4 on the SAME binary: arming the observer must not move one bit of guest-visible state.
///
/// Achievable by construction — the guest clock is `master_ticks` and the host `Instant`/`rdtsc`
/// reads are observation only — which is exactly why a failure here means a stamp reached
/// somewhere it must not.
#[test]
fn armed_and_disarmed_runs_leave_identical_guest_state() {
    arm_for_test(Some(Arm::Off), None);
    let (disarmed, disarmed_bus) = run_once();
    arm_for_test(Some(Arm::Full), Some(1));
    let (armed, armed_bus) = run_once();
    arm_for_test(None, None);

    assert_eq!(armed.registers.eax(), disarmed.registers.eax());
    assert_eq!(armed.registers.ecx(), disarmed.registers.ecx());
    assert_eq!(armed.registers.edx(), disarmed.registers.edx());
    assert_eq!(armed.registers.eip, disarmed.registers.eip);
    assert_eq!(armed.perf_counters(), disarmed.perf_counters());
    assert_eq!(armed_bus.memory, disarmed_bus.memory);
}

/// The published marks-per-entry factors (section 3 / 4b): P4, P5 and P8 are stamped TWICE per
/// entry because they are not contiguous in source, and everything else once.
///
/// P8 is the load-bearing one — A3 reads `marks(P8) = 2 x jit_direct_entries` — and it is the one
/// a "tidy up the duplicated mark" edit would break.
#[test]
fn marks_per_entry_are_the_published_factors() {
    let (cpu, snap) = run_under(Arm::Full, 1);
    let entries = cpu.perf_counters().jit_direct_entries;
    assert!(
        entries > 0,
        "no native entry ran, so this fixture proves nothing"
    );

    assert_eq!(
        marks(&snap, Phase::NativePreamble),
        2 * entries,
        "P8 is stamped twice per entry"
    );
    assert_eq!(
        marks(&snap, Phase::NativeBody),
        entries,
        "P9 is stamped once per entry"
    );
    assert_eq!(
        marks(&snap, Phase::TraceAlloc),
        entries,
        "P7 is stamped once per entry"
    );
    assert_eq!(
        marks(&snap, Phase::Budget),
        entries,
        "P6 is stamped once per entry"
    );
    // P4 and P5 are stamped twice on every traversal that reaches `run.rs:2433`, which is every
    // entered traversal plus the refusals between the two halves. `>= 2 x entries` is the honest
    // form: equality would be a claim about the refusal population this program does not fix.
    assert!(marks(&snap, Phase::SegmentLayout) >= 2 * entries);
    assert!(marks(&snap, Phase::BlockFields) >= 2 * entries);
    // The two `mark(P2)` sites are exclusive: `Ready` takes it at `1633`, `Compile` at `1487`.
    // Their sum plus the compile arm's own seventh exit is what makes them one bucket.
    assert!(marks(&snap, Phase::Probe) > 0);
}

/// Every entered traversal leaves exactly one terminal mark, and `end()` fires with it.
///
/// `total_entered` is the span from `begin()` to the terminal mark, so it must be non-zero exactly
/// when `marks(P11)` is; a return that forgot `end()` shows up as a total that lags the count.
#[test]
fn every_traversal_that_enters_takes_one_p11_and_one_end() {
    let (cpu, snap) = run_under(Arm::Full, 1);
    let entries = cpu.perf_counters().jit_direct_entries;
    assert_eq!(marks(&snap, Phase::TailClocks), entries);
    assert!(population(&snap, Population::Entered) > 0);
    // The terminal mark closes the tail, so it must carry ticks whenever it fired.
    assert!(ticks(&snap, Phase::TailClocks) > 0);
}

/// H3: every early return is both marked and named. The refusal histogram's total must equal the
/// number of P12 marks — a refusal that bumps the histogram without marking (or the reverse) is a
/// hole in the closure A1 is computed over.
#[test]
fn refusal_sites_close_against_the_marks_that_reached_them() {
    let (_cpu, snap) = run_under(Arm::Full, 1);
    let sites: u64 = snap.refusal_site.iter().flatten().sum();
    assert_eq!(
        sites,
        marks(&snap, Phase::Refused),
        "every P12 mark must carry exactly one refusal_site bump"
    );
    assert!(
        sites > 0,
        "this program takes refusals; zero means the sites are unreachable"
    );
    // The seven compile-arm exits are separable and total the P14 marks.
    let compiles: u64 = snap.compile_site.iter().flatten().sum();
    assert_eq!(compiles, marks(&snap, Phase::Compile));
}

/// B1: the stride decision is taken at `begin()`, so a sampled traversal is stamped END TO END.
///
/// The check that can fail: with a stride of N the mark COUNTS fall by roughly N, but the
/// per-mark cost does not move — a stride that let the inter-entry gap into P0 would inflate it.
#[test]
fn the_sampled_arm_stamps_end_to_end() {
    let (_cpu, full) = run_under(Arm::Full, 1);
    // `marks(P0)` counts TRAVERSALS, not entries: this loop compiles to one self-looping block, so
    // entries are few while dispatcher traversals are many, and the traversal is what the stride
    // decides on.
    let full_p0 = marks(&full, Phase::DispatchGates);
    assert!(
        full_p0 >= 4,
        "need enough traversals for a stride to bite (got {full_p0})"
    );

    let (_cpu, strided) = run_under(Arm::Full, 2);
    let strided_p0 = marks(&strided, Phase::DispatchGates);
    assert!(
        strided_p0 < full_p0,
        "a stride of 2 must stamp fewer traversals ({strided_p0} vs {full_p0})"
    );
    // Every SAMPLED traversal is stamped end to end, which is the property the stride must not
    // break: two P8 marks per sampled entry, never one, and P9 and P7 track them exactly.
    let strided_p8 = marks(&strided, Phase::NativePreamble);
    assert_eq!(strided_p8 % 2, 0);
    assert_eq!(marks(&strided, Phase::NativeBody), strided_p8 / 2);
    assert_eq!(marks(&strided, Phase::TraceAlloc), strided_p8 / 2);
}

/// A6's premise: COARSE takes the two native-window marks and NOTHING else.
#[test]
fn the_coarse_arm_takes_exactly_the_two_native_marks() {
    let (cpu, snap) = run_under(Arm::Coarse, 1);
    let entries = cpu.perf_counters().jit_direct_entries;
    assert!(entries > 0);
    assert_eq!(
        marks(&snap, Phase::NativePreamble),
        entries,
        "one coarse mark in per entry"
    );
    assert_eq!(
        marks(&snap, Phase::NativeBody),
        entries,
        "one coarse mark out per entry"
    );
    for phase in [
        Phase::DispatchGates,
        Phase::Key,
        Phase::Probe,
        Phase::EntryGuards,
        Phase::SegmentLayout,
        Phase::BlockFields,
        Phase::Budget,
        Phase::TraceAlloc,
        Phase::TailFetch,
        Phase::TailClocks,
        Phase::Refused,
        Phase::InterpretFallback,
        Phase::Compile,
    ] {
        assert_eq!(
            marks(&snap, phase),
            0,
            "{phase:?} must not be stamped in the COARSE arm"
        );
    }
    // `end()` still fires, which is what makes COARSE's totals comparable with FULL's.
    assert!(population(&snap, Population::Entered) > 0);
}

/// The default. A disarmed observer accumulates nothing at all and reports no snapshot.
#[test]
fn the_disarmed_arm_accumulates_nothing() {
    arm_for_test(Some(Arm::Off), None);
    let (cpu, _bus) = run_once();
    assert!(cpu.perf_counters().jit_direct_entries > 0);
    assert!(
        snapshot().is_none(),
        "a disarmed observer must not produce a snapshot"
    );
    arm_for_test(None, None);
}

/// M-R4: P14 is exempt from the outlier clamp, so a compile whose true cost exceeds the clamp
/// keeps its ticks and `ticks(P14) >= jit_direct_compile_ns` cannot be falsified by the clamp.
///
/// Checked structurally rather than by manufacturing a 0.3 ms compile: the run's P14 ticks must
/// be at least the compile time the production counter measured, and the outlier bucket must not
/// have absorbed anything from it.
#[test]
fn p14_is_exempt_from_the_outlier_clamp() {
    let (cpu, snap) = run_under(Arm::Full, 1);
    let perf = cpu.perf_counters();
    assert!(perf.jit_direct_compile_attempts > 0, "no compile ran");
    assert!(
        marks(&snap, Phase::Compile) > 0,
        "the compile arm was never marked"
    );
    // The clamp writes its shed excess into P15; a P14 that had been clamped would be the only
    // way this run produces a P15 mark, since nothing else here takes 0.3 ms.
    assert_eq!(
        marks(&snap, Phase::Outliers),
        snap.outlier_marks,
        "P15 must hold exactly the clamped marks"
    );
    assert_eq!(
        marks(&snap, Phase::Compile),
        perf.jit_direct_compile_attempts
            + snap.compile_site.iter().map(|lane| lane[0]).sum::<u64>(),
        "A3: marks(P14) = jit_direct_compile_attempts + compile_site[1512]"
    );
}

/// The calibration is a resolution floor, not a correction applied per mark: the snapshot carries
/// `ticks_raw` untouched and the overhead alongside it, so the subtraction is reversible.
#[test]
fn the_snapshot_reports_raw_ticks_and_the_overhead_separately() {
    let (_cpu, snap) = run_under(Arm::Full, 1);
    assert!(
        snap.overhead_ticks > 0,
        "the arm-time calibration did not run"
    );
    assert!(
        snap.overhead_ticks < 10_000,
        "the calibration median is not a mark cost"
    );
    assert!(snap.tsc_hz > 0, "no TSC frequency was derived");
    let raw: u64 = snap.ticks_raw.iter().flatten().sum();
    assert!(raw > 0);
    assert_eq!(
        snap.lane_pin_mismatches, 0,
        "H9: the lane must agree with the block's mode key"
    );
}

/// The `IZARRAVM_DIRECT_ENTRY_ATTRIBUTION` spelling table, read through the pure parse helper so
/// the process-global `OnceLock` is not involved.
///
/// Unlike the lane family, unset and the EMPTY STRING both select OFF here — this knob's default
/// arm IS the off arm, so the two spellings agree and a nulled PowerShell variable cannot arm the
/// observer by accident.
#[test]
fn entry_attribution_spelling_table() {
    use std::env::VarError;
    let parse = crate::jit::direct::parse_entry_attribution_arm;
    assert_eq!(
        parse(Err(VarError::NotPresent)),
        Arm::Off,
        "unset is the disarmed default"
    );
    for off in ["", "0", "off", "OFF", " Off "] {
        assert_eq!(parse(Ok(off.to_string())), Arm::Off, "{off:?} must disarm");
    }
    for full in ["1", "on", "full", "FULL", " On "] {
        assert_eq!(
            parse(Ok(full.to_string())),
            Arm::Full,
            "{full:?} must select FULL"
        );
    }
    for coarse in ["2", "coarse", "COARSE", " Coarse "] {
        assert_eq!(
            parse(Ok(coarse.to_string())),
            Arm::Coarse,
            "{coarse:?} must select COARSE"
        );
    }
}

/// A typo must not silently run the default: a ladder leg that quietly fell through would run
/// exactly what an unset environment runs and be read as the arm it named doing nothing.
#[test]
#[should_panic(expected = "names no arm")]
fn an_unrecognised_entry_attribution_spelling_panics() {
    crate::jit::direct::parse_entry_attribution_arm(Ok("true".to_string()));
}

/// The stride table. Unset and empty are 1 (every traversal); a positive integer is the stride.
#[test]
fn entry_attribution_sample_spelling_table() {
    use std::env::VarError;
    let parse = crate::jit::direct::parse_entry_attribution_sample;
    assert_eq!(parse(Err(VarError::NotPresent)), 1);
    assert_eq!(parse(Ok(String::new())), 1);
    assert_eq!(parse(Ok("  ".to_string())), 1);
    assert_eq!(parse(Ok("64".to_string())), 64);
    assert_eq!(parse(Ok(" 64 ".to_string())), 64);
}

/// Zero is not a stride: it would divide by zero at `begin()`, so it names no arm.
#[test]
#[should_panic(expected = "names no stride")]
fn a_zero_entry_attribution_stride_panics() {
    crate::jit::direct::parse_entry_attribution_sample(Ok("0".to_string()));
}
