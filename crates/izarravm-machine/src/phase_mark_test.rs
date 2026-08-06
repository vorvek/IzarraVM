// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Cover for periodic phase marks: the in-run-loop sampler that splits a fixture's run into
//! phases without slicing the host's single `run_until_halt_or_cycles` call.
//!
//! The whole point of the instrument is to measure emulation RATE per phase, so the property that
//! matters is that arming it changes nothing about the run. Two of the three tests here are about
//! that and not about the marks themselves.
//!
//! Mutation record, each verified by hand:
//!
//!  * raising `next_phase_mark_ticks`'s disarmed sentinel below `u64::MAX` fires marks on an
//!    unarmed run and fails `an_unarmed_run_records_no_periodic_marks`;
//!  * NOT COVERED: replacing the catch-up `while` with an `if` in `fire_periodic_phase_mark`
//!    fails NOTHING here, and the reason is worth knowing rather than hiding. The two differ only
//!    when ONE step crosses several intervals, which needs a HLT fast-forward: under a spin loop
//!    both variants fire at most once per batch and are indistinguishable, and under an interval
//!    coarser than a batch no jump happens at all. The fixture below therefore pins the
//!    no-duplicates property but cannot pin the catch-up. Exposing it needs a machine that idles
//!    on HLT with a timer wake, which this fixture is not; `run_until_halt_or_cycles` stops AT a
//!    terminal halt. Left as a known gap rather than a green mutation claim;
//!  * dropping the `cpu_profile: None` in `fire_periodic_phase_mark` fails
//!    `a_periodic_mark_carries_no_cpu_profile`, which is the guard against the sampler's cost
//!    growing across a run and biasing late phases against early ones.

use super::*;

const ROM: usize = BIOS_ROM_SIZE;

/// A machine spinning on `jmp $-2`, so guest time advances steadily and the run is bounded by the
/// cycle budget rather than by anything the guest does.
///
/// A SPIN and not a HLT: `run_until_halt_or_cycles` stops AT the halt, so a parked machine returns
/// having advanced almost no guest time and records no samples at all. That was the first version
/// of this fixture and it failed for a reason that had nothing to do with the code under test.
fn spinning_machine() -> Machine {
    let mut rom = vec![0u8; ROM];
    rom[0] = 0xeb; // jmp $-2
    rom[1] = 0xfe;
    // Reset vector at f000:fff0 far-jumps to f000:0000.
    rom[0xfff0..0xfff5].copy_from_slice(&[0xea, 0x00, 0x00, 0x00, 0xf0]);
    Machine::new(MachineProfile::gsw_386(4, VideoCard::Vega), rom).unwrap()
}

fn periodic(machine: &Machine) -> Vec<&PhaseMark> {
    machine
        .phase_marks()
        .iter()
        .filter(|m| m.id == phase_mark::PERIODIC)
        .collect()
}

/// The disarmed path records nothing, and it is the DEFAULT path: every shipped fixture run takes
/// it. The sentinel is what makes the run loop's check one compare against an already-live value,
/// so this also pins that `u64::MAX` is really the off state rather than a large-but-reachable
/// number.
#[test]
fn an_unarmed_run_records_no_periodic_marks() {
    let mut machine = spinning_machine();
    machine.enable_phase_marks();
    machine.run_until_halt_or_cycles(2_000_000).unwrap();
    assert!(
        periodic(&machine).is_empty(),
        "an unarmed run must record no periodic marks, got {}",
        periodic(&machine).len()
    );
}

/// Armed, the series is regular in MASTER TICKS.
///
/// Master ticks and not `elapsed_clocks`: both are guest-driven, but `elapsed_clocks` changes unit
/// across a live CPU-mode change (see `Machine::elapsed_clocks`), so a series triggered on it
/// would silently stop being equally spaced. This asserts the spacing that choice buys.
#[test]
fn periodic_marks_are_evenly_spaced_in_master_ticks() {
    let mut machine = spinning_machine();
    let clock = machine.profile().cpu.clock_rate();
    let per_ms = clock.clocks_for_fraction_floor(1, 1000);
    machine.arm_periodic_phase_marks(per_ms, per_ms * 40);
    machine.run_until_halt_or_cycles(per_ms * 40).unwrap();

    let marks = periodic(&machine);
    assert!(
        marks.len() >= 8,
        "a 40 ms budget at a 1 ms interval must sample repeatedly, got {}",
        marks.len()
    );
    // Spacing is approximately one interval, and it is NOT bounded below by the interval.
    //
    // The deadline advances by exactly `interval` on every fire, but a mark lands at the first
    // batch boundary at or AFTER its deadline, so each carries an overshoot in [0, batch_len).
    // Spacing is therefore `interval + (overshoot(k+1) - overshoot(k))`, which falls BELOW the
    // interval whenever the overshoot shrinks. Asserting `delta >= interval` claimed an exactness
    // the design never offered and failed here at 6,454,500 against 6,600,000.
    //
    // What is real: batches are far shorter than a sampling interval, so the deviation is small.
    // A half-interval band catches a broken trigger (bunched or sparse) without asserting the
    // false property.
    let interval = machine.periodic_phase_mark_interval_for_test();
    for pair in marks.windows(2) {
        let delta = pair[1].master_ticks - pair[0].master_ticks;
        assert!(
            delta >= interval / 2 && delta <= interval * 2,
            "sample spacing {delta} is not within half an interval of {interval}"
        );
    }
}

/// No two samples ever share a `master_ticks`.
///
/// That is the property a consumer needs: entries at the same guest instant separated by no wall
/// read as infinite rate to anything that divides. It is weaker than "a multi-interval jump fires
/// exactly once" -- see the module note on why that stronger claim has no cover here.
#[test]
fn a_jump_past_several_intervals_fires_one_mark() {
    let mut machine = spinning_machine();
    let clock = machine.profile().cpu.clock_rate();
    // An interval far finer than a HLT step, so the fast-forward is guaranteed to cross many.
    let tiny = clock.clocks_for_fraction_floor(1, 1_000_000).max(1);
    machine.arm_periodic_phase_marks(tiny, tiny * 1000);
    machine
        .run_until_halt_or_cycles(clock.clocks_for_fraction_floor(1, 100))
        .unwrap();

    let marks = periodic(&machine);
    for pair in marks.windows(2) {
        assert!(
            pair[1].master_ticks > pair[0].master_ticks,
            "two samples share master_ticks {}, so a jump fired more than once",
            pair[0].master_ticks
        );
    }
}

/// A periodic mark carries NO cpu profile, whatever the profiler is doing.
///
/// This is the load-bearing one. `note_phase_mark` takes a full `profile_snapshot` when profiling
/// is armed, and that snapshot sorts an untruncated map of every sampled address, which only grows
/// across a run. Since `wall` is sampled first, mark k's cost is charged to interval k and grows
/// monotonically, so late intervals would look slower than they are. On a fixture that loads and
/// then renders, that is the instrument manufacturing the very knee it exists to find.
#[test]
fn a_periodic_mark_carries_no_cpu_profile() {
    let mut machine = spinning_machine();
    machine.enable_host_profiling(1);
    let clock = machine.profile().cpu.clock_rate();
    let per_ms = clock.clocks_for_fraction_floor(1, 1000);
    machine.arm_periodic_phase_marks(per_ms, per_ms * 20);
    machine.run_until_halt_or_cycles(per_ms * 20).unwrap();

    let marks = periodic(&machine);
    assert!(!marks.is_empty(), "the fixture must record samples");
    for mark in marks {
        assert!(
            mark.cpu_profile.is_none(),
            "a periodic sample must not carry a cpu profile even with profiling armed"
        );
        assert!(
            mark.machine_phases.phases.is_empty(),
            "a periodic sample must not allocate the per-mark host-phase vec"
        );
    }
}
