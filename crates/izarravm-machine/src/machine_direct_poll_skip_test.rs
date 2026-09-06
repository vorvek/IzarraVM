// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The GP2 call-out-site poll skip's machine-crate harness (design BLOCKER D).
//!
//! `CpuBus::callout_poll_skip` is a DEFAULTED method: `MachineBus` is the only implementor that
//! ever carries a real body, so `jit_direct_poll_skip_spans` can never rise in any `izarravm-cpu`
//! fixture. `TestBus` (`izarravm-cpu`) does NOT implement `callout_poll_skip`, and must not:
//! stubbing it there would satisfy every "assert the HIT" conjunct in the mutant table with a
//! stub that always hits, which is the same vacuity `direct_callout_attribution` shipped once
//! already (handoff-25 §9).
//!
//! **Two families of fixture live here.** `callout_poll_skip_commits_a_span_through_the_real_bus`
//! and its neighbours drive `MachineBus::callout_poll_skip` directly against a REAL `Machine`'s
//! VGA/CRTC state, warming the certified shape through the INTERPRETER (`run_budgeted(bus, 0)`,
//! the `machine_native_bus_timing_test.rs` `prepare_setup_poll_head` pattern) rather than through
//! native x86 host codegen -- this is the seam contract itself (`poll_bus_certificate` /
//! `poll_fetch_certificate_raw_from`, the split BLOCKER B needs; `predicted_beam` /
//! `dots_until_status1_bit_change_from`; the admissibility binary search; `poll_commit_bus`), all
//! through a live `Vega` and `BusTrace`, exercised exactly as `port_read_al_dx` exercises them
//! through the POD (`CalloutPollSkipRequest`). The `PollLoop` these fixtures pass through is built
//! by the SAME `build_poll_loop_from` the Direct call-out uses (`cpu.poll_loop()` is
//! `build_poll_loop(cpu) == build_poll_loop_from(cpu, cpu.linear_eip())`), so the shape and its
//! `fetch_count()`/`fresh_iteration_spins`/`raw_core_clocks()` are the real, certified values.
//!
//! `a_port_callout_preserves_the_blocks_eflags_shadow` (M-24) DOES go through native codegen --
//! `set_native_backend_enabled(true)` + `set_jit_auto_admit(true)`, with
//! `jit_direct_blocks_installed > 0` asserted so the fixture cannot silently prove nothing.
//!
//! A trap worth naming because it cost real time building this file:
//! `Machine::new_raw_program` loads its image at guest OFFSET `0x100` within the segment, COM-file
//! style (`raw_program::load_program`: `mem.write_u8(base + 0x100 + index, byte)`), so a fixture's
//! `program[i]` lands at guest linear `cs.base + 0x100 + i`, NOT `cs.base + i`. Getting this wrong
//! reads plausible-looking garbage at the intended address (decoded opcode `0x00`, not a fault),
//! which is a silent, not a loud, failure mode.
//!
//! **NOT built here: a native-codegen L3 fixture** (`poll_skip_is_clock_identical_to_the_
//! unskipped_run`, the design's own name for it). Attempted, and dropped rather than shipped
//! broken -- recorded in the GP2 poll-skip revision report rather than left as a silent gap. A
//! program shaped `mov ecx,N; [outer/w1] in al,dx; test al,8; jnz w1; [w2] in al,dx; test al,8;
//! jz w2; dec ecx; jnz outer; out 0x80,al; hlt` (an OUTER counted loop wrapping the certified
//! 3-slot shape, needed so a single cold run gets enough native dispatcher entries to clear
//! admission's heat threshold) reproducibly read `cs().default_size_32` as `false` at the
//! call-out despite being forced `true` at construction, ONCE the outer wrapper's own back-edge
//! also targeted the inner loop's head address (i.e. once TWO distinct back-edges converge on the
//! same entry) -- and with it misdecoded, `EDX` read `0xA8EC03DA`, exactly the raw bytes at
//! `program[6..10]` reinterpreted as a 32-bit immediate under the wrong operand-size default. The
//! callout-poll-skip and BLOCKER-1/2 fixtures above never exercise a program shape with two
//! converging back-edges, so this is NOT known to be caused by anything this slice touches, and it
//! was not root-caused within the session that found it. `callout_poll_skip_commits_a_span_
//! through_the_real_bus`'s exact `skipped_raw_core_clocks == 112 * iterations` assertion, and the
//! BLOCKER 1/2 differential fixtures above, are the identity evidence this file actually ships;
//! they pin the seam's arithmetic directly rather than comparing two native end-to-end runs.

use super::*;
#[cfg(feature = "jit")]
use izarravm_cpu::PollLoop;

const HEAD: u32 = 0x103;
/// Entry of the 5-slot Ecx-source Direct poll shape (`diagnostic_class() == 1`), far enough past
/// `HEAD`'s 5-byte footprint to leave it untouched.
const HEAD5: u32 = 0x120;
/// Entry of the 6-slot Ebx-source PairedJmp poll shape (`diagnostic_class() == 2`).
const HEAD6: u32 = 0x140;

/// Warm the certified 3-slot shape (`IN AL,DX` / `TEST AL,mask` / `Jcc` back to the `IN`) at
/// `HEAD` through the interpreter, exactly as `prepare_setup_poll_head`
/// (`machine_native_bus_timing_test.rs`) warms its shapes, and return the certified `PollLoop`.
#[cfg(feature = "jit")]
fn warm_3slot_poll_loop(cpu: &mut CpuGsw, bus: &mut MachineBus<'_>) -> PollLoop {
    // The shape's bytes are baked into the fixture's raw program at HEAD (`poll_skip_seam_test_
    // machine`); this only WARMS the decode cache and steps the interpreter through each slot
    // once so `poll_loop()` sees a resident view, mirroring `prepare_setup_poll_head`'s loop.
    for offset in [0u32, 1, 3] {
        cpu.registers.eip = HEAD + offset;
        cpu.poll_skip_backedge_housekeeping();
        cpu.run_budgeted(bus, 0).expect("warm one poll-loop slot");
    }
    cpu.registers.eip = HEAD;
    cpu.poll_skip_backedge_housekeeping();
    cpu.poll_loop().expect("the 3-slot shape must certify")
}

/// Warm the certified 5-slot Ecx-source Direct shape (`mov edx,ecx / sub eax,eax / in al,dx /
/// test al,mask / jnz` back to the `mov`) at `HEAD5`, exactly as `warm_3slot_poll_loop` warms the
/// 3-slot shape. Rank-5's `diagnostic_class() == 1` half.
#[cfg(feature = "jit")]
fn warm_5slot_poll_loop(cpu: &mut CpuGsw, bus: &mut MachineBus<'_>) -> PollLoop {
    for offset in [0u32, 2, 4, 5, 7] {
        cpu.registers.eip = HEAD5 + offset;
        cpu.poll_skip_backedge_housekeeping();
        cpu.run_budgeted(bus, 0)
            .expect("warm one 5-slot poll-loop slot");
    }
    cpu.registers.eip = HEAD5;
    cpu.poll_skip_backedge_housekeeping();
    cpu.poll_loop()
        .expect("the 5-slot Ecx-source shape must certify")
}

/// Warm the certified 6-slot Ebx-source PairedJmp shape (`mov edx,ebx / sub eax,eax / in al,dx /
/// test al,mask / je +2(exit) / jmp -11(entry)`) at `HEAD6`. Rank-5's `diagnostic_class() == 2`
/// half -- the shape whose spin sense `fresh_iteration_spins` INVERTS relative to Direct.
#[cfg(feature = "jit")]
fn warm_6slot_poll_loop(cpu: &mut CpuGsw, bus: &mut MachineBus<'_>) -> PollLoop {
    for offset in [0u32, 2, 4, 5, 7, 9] {
        cpu.registers.eip = HEAD6 + offset;
        cpu.poll_skip_backedge_housekeeping();
        cpu.run_budgeted(bus, 0)
            .expect("warm one 6-slot poll-loop slot");
    }
    cpu.registers.eip = HEAD6;
    cpu.poll_skip_backedge_housekeeping();
    cpu.poll_loop()
        .expect("the 6-slot Ebx-source PairedJmp shape must certify")
}

/// Build the request the Direct call-out would build, from a certified `PollLoop`. Mirrors
/// `port_read_al_dx`'s own composition exactly (same fields, same `level_timing` dial, same
/// `poll_skip_timing_remainder`), so this is testing the SAME seam contract that helper drives.
#[cfg(feature = "jit")]
fn request_for(cpu: &CpuGsw, poll: PollLoop, cap: u64) -> CalloutPollSkipRequest {
    request_for_with_bus_baseline(cpu, poll, cap, 0)
}

/// `request_for`, with an explicit `bus_scaled_at_run_entry` -- the field BLOCKER 2's fix added.
/// Zero reproduces a fresh batch's first run (what `request_for` always built); a nonzero value
/// is what a fixture needs to exercise a SECOND run inside one batch, which is exactly the
/// scenario every pre-revision fixture skipped (see `prior_runs_core_clocks_is_nonzero_on_a_
/// second_batch_run` below).
#[cfg(feature = "jit")]
fn request_for_with_bus_baseline(
    cpu: &CpuGsw,
    poll: PollLoop,
    cap: u64,
    bus_scaled_at_run_entry: u64,
) -> CalloutPollSkipRequest {
    let mut fetches = [(0u32, 0u32, 0u8); 6];
    for (slot, index) in fetches.iter_mut().zip(0..poll.fetch_count()) {
        if let Some(fetch) = poll.fetch(index) {
            *slot = fetch;
        }
    }
    let (core_num, core_den) = izarravm_cpu::level_timing_for_test(cpu.persona());
    CalloutPollSkipRequest {
        fetches,
        fetch_count: poll.fetch_count() as u8,
        status_mask: poll.status_mask(),
        spins_when_bit_set: poll.fresh_iteration_spins(poll.status_mask()),
        raw_core_clocks: cpu.poll_skip_raw_core_clocks(poll),
        core_clocks_at_block_entry: cpu.core_clocks_so_far_for_test(),
        prefix_raw: 0,
        core_num,
        core_den,
        timing_rem: cpu.poll_skip_timing_remainder(),
        cap,
        bus_scaled_at_run_entry,
        min_iterations: 2,
        max_skipped_raw: u64::from(u32::MAX) - 12,
    }
}

/// Move the VGA beam until status1's `mask` bit reads `target`, on the
/// `machine_native_bus_timing_test.rs` `set_status1_bit` precedent: a certified shape's admissible
/// binary search needs the CURRENT status to already be "still spinning" (`fresh_iteration_spins`)
/// before it has anything to search over.
#[cfg(feature = "jit")]
fn set_status1_bit(machine: &mut Machine, mask: u8, target: bool) {
    let bit = mask.trailing_zeros() as u8;
    let beam = machine.vega.beam_dots();
    let current = machine.vega.status1_bits(beam) & mask != 0;
    if current != target {
        let dots = machine
            .vega
            .dots_until_status1_bit_change_from(beam, bit, target)
            .expect("status bit has a geometric edge");
        machine.vega.advance(0, 0, 0, dots);
    }
    assert_eq!(
        machine.vega.status1_bits(machine.vega.beam_dots()) & mask != 0,
        target
    );
}

fn with_cpu_and_bus<R>(
    machine: &mut Machine,
    f: impl FnOnce(&mut CpuGsw, &mut MachineBus<'_>) -> R,
) -> R {
    let mut cpu = std::mem::take(&mut machine.cpu);
    let result = {
        let mut bus = machine.make_bus();
        f(&mut cpu, &mut bus)
    };
    machine.cpu = cpu;
    result
}

#[cfg(feature = "jit")]
fn poll_skip_seam_test_machine(mode: GswMode) -> Machine {
    // `Machine::new_raw_program` loads the image at guest offset 0x100 within the segment
    // (classic COM convention, `raw_program::load_program`: `base + 0x100 + index`), so an
    // array index must be `HEAD - 0x100` to land the byte at guest offset `HEAD`.
    let base_index = (HEAD - 0x100) as usize;
    let mut program = vec![0u8; 0x200];
    program[base_index] = 0xec; // in al,dx
    program[base_index + 1] = 0xa8; // test al,imm8
    program[base_index + 2] = 0x08; // mask (vsync bit)
    program[base_index + 3] = 0x75; // jnz
    program[base_index + 4] = 0xfb; // rel8 -5, back to HEAD
    // The 5-slot Ecx-source Direct shape (`diagnostic_class() == 1`), at HEAD5, well clear of
    // HEAD's page-relative footprint: `mov edx,ecx / sub eax,eax / in al,dx / test al,8 / jnz
    // -9`. Same mask and branch sense as the 3-slot shape above (spins while the bit is SET), the
    // only difference being the two extra setup/clear slots ahead of the `in`.
    let head5_index = (HEAD5 - 0x100) as usize;
    program[head5_index] = 0x89; // mov edx,ecx (modrm 11 001 010)
    program[head5_index + 1] = 0xca;
    program[head5_index + 2] = 0x29; // sub eax,eax (modrm 11 000 000)
    program[head5_index + 3] = 0xc0;
    program[head5_index + 4] = 0xec; // in al,dx
    program[head5_index + 5] = 0xa8; // test al,imm8
    program[head5_index + 6] = 0x08; // mask (vsync bit)
    program[head5_index + 7] = 0x75; // jnz
    program[head5_index + 8] = 0xf7; // rel8 -9, back to HEAD5
    // The 6-slot Ebx-source PairedJmp shape (`diagnostic_class() == 2`), at HEAD6: `mov
    // edx,ebx / sub eax,eax / in al,dx / test al,8 / je +2(exit) / jmp -11(entry) / hlt(exit)`.
    // `PollBranchShape::PairedJmp` INVERTS the spin sense (`fresh_iteration_spins`: `!branch_
    // taken`, not `branch_taken`): the forward `je` must be taken to LEAVE the loop (bit
    // cleared), so spinning (bit set) is the branch NOT-taken, fall-through-to-the-backward-jmp
    // case -- the opposite polarity from the 3-/5-slot Direct shapes just above, which is
    // exactly the case the GP2 revision diagnostic pair found never engaging in the wild
    // (`poll_skip_spans = [39543, 0, 0]`, classes 1 and 2 both zero).
    let head6_index = (HEAD6 - 0x100) as usize;
    program[head6_index] = 0x89; // mov edx,ebx (modrm 11 011 010)
    program[head6_index + 1] = 0xda;
    program[head6_index + 2] = 0x29; // sub eax,eax
    program[head6_index + 3] = 0xc0;
    program[head6_index + 4] = 0xec; // in al,dx
    program[head6_index + 5] = 0xa8; // test al,imm8
    program[head6_index + 6] = 0x08; // mask (vsync bit)
    program[head6_index + 7] = 0x74; // je
    program[head6_index + 8] = 0x02; // rel8 +2, forward to HEAD6+11 (exit)
    program[head6_index + 9] = 0xeb; // jmp
    program[head6_index + 10] = 0xf5; // rel8 -11, back to HEAD6 (entry)
    program[head6_index + 11] = 0xf4; // hlt (exit; never reached by classification or warming)
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = mode;
    let mut machine = Machine::new_raw_program(profile, &program).unwrap();
    let mut cs = machine.cpu.registers.cs();
    cs.default_size_32 = true;
    cs.limit = u32::MAX;
    machine.cpu.registers.set_segment(SegmentIndex::Cs, cs);
    machine.cpu.registers.set_edx(0x03da);
    machine.cpu.registers.set_ecx(0x03da);
    machine.cpu.registers.set_ebx(0x03da);
    machine.cpu.registers.eip = HEAD;
    // `CpuGsw::poll_loop()` (used to warm/certify the shape below, through the INTERPRETER's own
    // classifier) gates on `poll_skip_eligible()`, which requires the Direct backend DISABLED
    // (`block.rs`'s own `poll_skip_eligible` doc: "the ONLY consumer this design serves is a
    // Direct-armed CPU, so this predicate is the interpreter's own eligibility gate and is
    // deliberately blind to that arm"). This fixture never runs native code at all -- it drives
    // `MachineBus::callout_poll_skip` directly -- so the interpreter's classifier is exactly the
    // tool to certify the shape with, on the `poll_skip_test_machine` precedent
    // (`machine_native_bus_timing_test.rs`).
    machine.cpu.set_native_backend_enabled(false);
    // `0x75` (JNZ) spins while the bit is SET (`fresh_iteration_spins` for the Direct branch
    // shape: `spins = (status & mask == 0) == branch_when_zero`, and `branch_when_zero` is false
    // for JNZ) -- position the beam so that is the CURRENT state, or the admissibility search has
    // nothing to search over on the very first attempt.
    set_status1_bit(&mut machine, 0x08, true);
    machine.trace.set_tracing_mode(TracingMode::Off);
    machine
}

/// **I4/I1/I2, the seam's engagement bar, driven through the REAL bus.** A certified 3-slot shape
/// spinning on a fresh-programmed CRTC's vsync bit must let `callout_poll_skip` commit a span,
/// and the committed core/bus clocks must be exactly `raw_core_clocks * iterations` /
/// `certificate.raw_clocks_per_iteration * iterations` -- the same identities L3 grades.
#[cfg(feature = "jit")]
#[test]
fn callout_poll_skip_commits_a_span_through_the_real_bus() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let mut machine = poll_skip_seam_test_machine(mode);

        let (outcome, cap) = with_cpu_and_bus(&mut machine, |cpu, bus| {
            let poll = warm_3slot_poll_loop(cpu, bus);
            let cap = cpu.core_clocks_so_far_for_test() + 10_000_000;
            let request = request_for(cpu, poll, cap);
            (bus.callout_poll_skip(&request), cap)
        });
        let outcome = outcome.unwrap_or_else(|decline| {
            panic!("mode={mode:?}: callout_poll_skip declined: {decline:?}, cap={cap}")
        });
        assert!(
            outcome.iterations >= 2,
            "mode={mode:?}: a committed span must be at least min_iterations"
        );
        assert_eq!(
            outcome.skipped_raw_core_clocks,
            (if mode == GswMode::Gsw486 { 132 } else { 112 }) * outcome.iterations,
            "mode={mode:?}: the 3-slot shape's raw core charge must be exactly 17 per iteration"
        );
    }
}

/// **Rank-5 killer**: the 5-/6-slot shapes (`diagnostic_class() == 1` and `== 2`) and
/// `PollBranchShape::PairedJmp`'s inverted spin sense. The GP2 revision diagnostic pair measured
/// `poll_skip_spans = [39543, 0, 0]` on a real game corpus -- classes 1 and 2 NEVER engaged --
/// and nothing in the pre-revision suite exercised either shape at all, so a broken `port_source`
/// register read or an inverted-inversion bug in `fresh_iteration_spins` for `PairedJmp` could
/// have shipped silently. This drives both shapes through the SAME real-bus seam the 3-slot test
/// above does (`MachineBus::callout_poll_skip`, not a stub), certified by the SAME interpreter
/// classifier (`cpu.poll_loop()`) the Direct call-out uses.
#[cfg(feature = "jit")]
#[test]
fn five_and_six_slot_shapes_engage_with_the_correct_spin_sense() {
    let mut machine = poll_skip_seam_test_machine(GswMode::Gsw586);

    let (poll5, poll6, outcome5, outcome6) = with_cpu_and_bus(&mut machine, |cpu, bus| {
        let poll5 = warm_5slot_poll_loop(cpu, bus);
        let poll6 = warm_6slot_poll_loop(cpu, bus);
        let cap = cpu.core_clocks_so_far_for_test() + 10_000_000;
        let request5 = request_for(cpu, poll5, cap);
        let request6 = request_for(cpu, poll6, cap);
        let outcome5 = bus.callout_poll_skip(&request5);
        let outcome6 = bus.callout_poll_skip(&request6);
        (poll5, poll6, outcome5, outcome6)
    });

    assert_eq!(
        poll5.diagnostic_class(),
        1,
        "the Ecx-source Direct shape must classify as diagnostic_class() 1"
    );
    assert_eq!(
        poll6.diagnostic_class(),
        2,
        "the Ebx-source PairedJmp shape must classify as diagnostic_class() 2"
    );

    // The status1 bit is SET (still spinning) at this point (`poll_skip_seam_test_machine`'s own
    // `set_status1_bit(.., 0x08, true)`). Both shapes are built to spin while the bit is SET, but
    // via OPPOSITE branch polarities (5-slot Direct via JNZ, 6-slot PairedJmp via JE), which is
    // exactly the case `fresh_iteration_spins` must get right for each `branch_shape` arm.
    assert!(
        poll5.fresh_iteration_spins(0x08),
        "5-slot Direct (JNZ, branch_when_zero=false): spins = branch_taken must read true \
         while the bit is set"
    );
    assert!(
        poll6.fresh_iteration_spins(0x08),
        "6-slot PairedJmp (JE, branch_when_zero=true): spins = !branch_taken must STILL read \
         true while the bit is set -- this is the inverted arm the design's own accessor exists \
         for"
    );
    assert!(
        !poll5.fresh_iteration_spins(0x00),
        "5-slot Direct: spins must read false once the bit clears"
    );
    assert!(
        !poll6.fresh_iteration_spins(0x00),
        "6-slot PairedJmp: spins must read false once the bit clears -- the inverted arm must \
         invert in BOTH directions, not just the spinning one"
    );

    let outcome5 = outcome5
        .unwrap_or_else(|decline| panic!("5-slot shape: callout_poll_skip declined: {decline:?}"));
    let outcome6 = outcome6
        .unwrap_or_else(|decline| panic!("6-slot shape: callout_poll_skip declined: {decline:?}"));
    assert!(
        outcome5.iterations >= 2,
        "5-slot shape: a committed span must be at least min_iterations"
    );
    assert!(
        outcome6.iterations >= 2,
        "6-slot shape: a committed span must be at least min_iterations"
    );
    assert_eq!(
        outcome5.skipped_raw_core_clocks,
        136 * outcome5.iterations,
        "the 5-slot shape's raw core charge must be exactly 21 per iteration"
    );
    assert_eq!(
        outcome6.skipped_raw_core_clocks,
        148 * outcome6.iterations,
        "the 6-slot shape's raw core charge must be exactly 28 per iteration"
    );
}

/// **I4, engaged through NATIVE codegen end to end** -- the strongest form of BLOCKER D's
/// requirement: not just the seam (above), but the EMITTED `port_read_al_dx` call-out, compiled
/// and run as real host machine code, committing a span through the real bus.
#[cfg(feature = "jit")]
#[test]
fn direct_poll_skip_engages_through_native_codegen() {
    // `in al,dx; test al,0x08; jnz $-4` at CS:0100 (entry IS the loop head, so no chain/re-entry
    // subtlety to get wrong): index 0 == guest offset 0x100 (the loader's `+0x100` COM
    // convention), so `HEAD` there is `0x100`, distinct from this file's other fixtures' `HEAD`
    // (`0x103`) only in that this program has no `mov dx,3dah` prefix -- EDX is preloaded instead.
    let program = [0xec, 0xa8, 0x08, 0x75, 0xfb];
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = GswMode::Gsw586;
    let mut machine = Machine::new_raw_program(profile, &program).unwrap();
    machine.set_jit_auto_admit(true);
    machine.cpu.set_native_backend_enabled(true);
    machine.cpu.set_direct_poll_skip_override(Some(true));
    let mut cs = machine.cpu.registers.cs();
    cs.default_size_32 = true;
    cs.limit = u32::MAX;
    machine.cpu.registers.set_segment(SegmentIndex::Cs, cs);
    machine.cpu.registers.set_edx(0x03da);
    machine.cpu.registers.eip = 0x100;
    machine.trace.set_tracing_mode(TracingMode::Off);
    set_status1_bit(&mut machine, 0x08, true);

    machine
        .run_cycles(2_000_000)
        .expect("the fixture must not stop the machine");
    let snapshot = machine.cpu.direct_stall_snapshot();
    let spans: u64 = snapshot.poll_skip_spans.iter().sum();
    assert!(
        spans > 0,
        "the poll skip never committed a span through native codegen; attempts={} declined(\
         port={} port_source={} knob={} eligibility={} shape={} seam={}) blocks_installed={}",
        snapshot.poll_attempts,
        snapshot.poll_declined_port,
        snapshot.poll_declined_port_source,
        snapshot.poll_declined_knob,
        snapshot.poll_declined_eligibility,
        snapshot.poll_declined_shape,
        snapshot.poll_declined_seam,
        machine.cpu.perf_counters().jit_direct_blocks_installed,
    );
    // I7: the interpreter's own span counter must stay zero on a Direct row, both arms.
    assert_eq!(
        machine.cpu.perf_counters().poll_skip_spans,
        0,
        "the Direct arm must never touch the interpreter's own poll-skip commit"
    );
}

/// The 16-bit screen: a 0x3DA call-out from a 16-bit code segment must decline
/// BEFORE the scan. Every shape is 32-bit (`poll_head_possible` requires `d`),
/// so the scan can only ever say no -- and it says no expensively, one block
/// decode plus its allocations per call-out. Tyrian 2000 spins on 0x3DA from
/// 16-bit native blocks and paid 1.5e9 doomed scans, ~80% of its wall,
/// before this screen existed (2026-08-29; the OFF-arm A/B measured 5.2x at
/// 586 and 4.0x at 486 on identical retired instructions).
#[cfg(feature = "jit")]
#[test]
fn a_sixteen_bit_callout_declines_before_the_scan() {
    // The same 3-slot spin as `direct_poll_skip_engages_through_native_codegen`,
    // but with the fixture's 16-bit default cs kept as it is.
    let program = [0xec, 0xa8, 0x08, 0x75, 0xfb];
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = GswMode::Gsw586;
    let mut machine = Machine::new_raw_program(profile, &program).unwrap();
    machine.set_jit_auto_admit(true);
    machine.cpu.set_native_backend_enabled(true);
    machine.cpu.set_direct_poll_skip_override(Some(true));
    // Pin the OFF arm EXPLICITLY. Until 2026-08-29 this fixture relied on
    // `IZARRAVM_DIRECT_POLL_SKIP_16` being unset, and unset meaning OFF; the flip made unset
    // mean ON, which would have turned this row into a test of the opposite arm while still
    // passing its `poll_attempts > 0` assertion. The screen it exists to prove is still there,
    // and this is the spelling that still asks for it.
    machine.cpu.set_direct_poll_skip_16_override(Some(false));
    machine.cpu.registers.set_edx(0x03da);
    machine.cpu.registers.eip = 0x100;
    machine.trace.set_tracing_mode(TracingMode::Off);
    set_status1_bit(&mut machine, 0x08, true);

    machine
        .run_cycles(2_000_000)
        .expect("the fixture must not stop the machine");
    let snapshot = machine.cpu.direct_stall_snapshot();
    assert!(
        snapshot.poll_attempts > 0,
        "the 16-bit spin never reached the port call-out at all \
         (blocks_installed={})",
        machine.cpu.perf_counters().jit_direct_blocks_installed,
    );
    assert!(
        snapshot.poll_declined_sixteen_bit > 0,
        "a 16-bit cs must land in the sixteen-bit lane; attempts={} declined(port={} \
         knob={} eligibility={} shape={})",
        snapshot.poll_attempts,
        snapshot.poll_declined_port,
        snapshot.poll_declined_knob,
        snapshot.poll_declined_eligibility,
        snapshot.poll_declined_shape,
    );
    assert_eq!(
        snapshot.poll_declined_shape, 0,
        "no scan may run for a 16-bit cs -- the shape lane counts scans that ran"
    );
}

/// The call-out-site negative cache: a 32-bit loop that reads 0x3DA but does
/// not certify (a NOP breaks the 3-slot shape) must be SCANNED ONCE and then
/// answered from the interpreter path's own negative cache, not re-scanned on
/// every iteration. The cache key is the scan anchor; the page-insert
/// generation guard invalidates it exactly as it does for the interpreter.
#[cfg(feature = "jit")]
#[test]
fn a_non_certifiable_callout_scan_is_answered_from_the_negative_cache() {
    // `in al,dx; test al,0x08; nop; jnz $-5`: the NOP is a code byte no shape
    // holds, so the backward scan returns a structural (cacheable) negative.
    let program = [0xec, 0xa8, 0x08, 0x90, 0x75, 0xfa];
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = GswMode::Gsw586;
    let mut machine = Machine::new_raw_program(profile, &program).unwrap();
    machine.set_jit_auto_admit(true);
    machine.cpu.set_native_backend_enabled(true);
    machine.cpu.set_direct_poll_skip_override(Some(true));
    machine.cpu.set_poll_neg_cache_enabled_for_test(true);
    let mut cs = machine.cpu.registers.cs();
    cs.default_size_32 = true;
    cs.limit = u32::MAX;
    machine.cpu.registers.set_segment(SegmentIndex::Cs, cs);
    machine.cpu.registers.set_edx(0x03da);
    machine.cpu.registers.eip = 0x100;
    machine.trace.set_tracing_mode(TracingMode::Off);
    set_status1_bit(&mut machine, 0x08, true);

    machine
        .run_cycles(2_000_000)
        .expect("the fixture must not stop the machine");
    let snapshot = machine.cpu.direct_stall_snapshot();
    let perf = machine.cpu.perf_counters();
    assert!(
        snapshot.poll_declined_shape > 2,
        "the spin must decline on shape repeatedly; attempts={}",
        snapshot.poll_attempts,
    );
    assert!(
        perf.poll_neg_cache_stores >= 1,
        "the first structural negative must be stored"
    );
    assert!(
        perf.poll_neg_cache_hits > 0
            && perf.poll_neg_cache_hits + perf.poll_neg_cache_stores + perf.poll_neg_cache_volatile
                >= snapshot.poll_declined_shape,
        "later call-outs must answer from the cache instead of re-scanning: \
         hits={} stores={} volatile={} against {} shape declines",
        perf.poll_neg_cache_hits,
        perf.poll_neg_cache_stores,
        perf.poll_neg_cache_volatile,
        snapshot.poll_declined_shape,
    );
}

/// **Rank-2 killer**: `build_poll_loop_from`'s `slot_delta` composition in a CHAINED block. The
/// fixture above deliberately puts the poll loop AT its own block's entry ("no chain/re-entry
/// subtlety to get wrong", its own comment says so); this one puts the poll loop in a SECOND
/// block reached by a linked unconditional `jmp` from a first block, so `current`/`entry` in
/// `build_poll_loop_from` are computed from a slot whose block was entered via the link graph,
/// not via the dispatcher's cold entry. If the composition were wrong, the classifier would scan
/// from the wrong address -- silently declining (`declined_shape`) at best, certifying a
/// different shape that happens to sit there at worst. Two assertions the review names
/// explicitly: `poll_skip_spans.sum() > 0` (it engages at all) and `poll_skip_last_head` equals
/// the SUCCESSOR block's own head linear, not the first block's.
#[cfg(feature = "jit")]
#[test]
fn direct_poll_skip_engages_in_a_chained_successor_block() {
    // Block A (guest OFFSET 0x100 within the segment, COM convention): `nop; jmp +0x0d` -- a
    // trivial predecessor whose only job is to hand control to block B via a linked transfer,
    // exactly once. Block B (guest offset 0x110): the same `in al,dx; test al,0x08; jnz $-4`
    // 3-slot shape the unchained fixture certifies, at its OWN entry -- the "chain" is that block
    // B is reached only by the link from block A, never as anyone's cold dispatch target.
    let mut program = vec![0u8; 0x15];
    program[0] = 0x90; // nop
    program[1] = 0xeb; // jmp rel8
    program[2] = 0x0d; // +0x0d: (offset 3) + 0x0d == offset 0x10 == BLOCK_B_HEAD - 0x100
    program[0x10] = 0xec; // in al,dx
    program[0x11] = 0xa8; // test al,imm8
    program[0x12] = 0x08; // mask (vsync bit)
    program[0x13] = 0x75; // jnz
    program[0x14] = 0xfb; // rel8 -5, back to BLOCK_B_HEAD
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = GswMode::Gsw586;
    let mut machine = Machine::new_raw_program(profile, &program).unwrap();
    machine.set_jit_auto_admit(true);
    machine.cpu.set_native_backend_enabled(true);
    machine.cpu.set_direct_poll_skip_override(Some(true));
    let mut cs = machine.cpu.registers.cs();
    cs.default_size_32 = true;
    cs.limit = u32::MAX;
    machine.cpu.registers.set_segment(SegmentIndex::Cs, cs);
    machine.cpu.registers.set_edx(0x03da);
    machine.cpu.registers.eip = 0x100;
    machine.trace.set_tracing_mode(TracingMode::Off);
    set_status1_bit(&mut machine, 0x08, true);
    // The expected LINEAR head is CS.base + the guest offset, not the bare offset: the loader's
    // segment base is not zero (`Machine::new_raw_program`'s own COM convention), and the whole
    // point of this assertion is the certified shape's actual linear address, wherever that is.
    let block_b_head_linear = cs.base.wrapping_add(0x110);

    machine
        .run_cycles(2_000_000)
        .expect("the fixture must not stop the machine");
    let snapshot = machine.cpu.direct_stall_snapshot();
    let spans: u64 = snapshot.poll_skip_spans.iter().sum();
    assert!(
        spans > 0,
        "the poll skip never committed a span in the chained successor block; attempts={} \
         declined(port={} port_source={} knob={} eligibility={} shape={} seam={}) \
         blocks_installed={}",
        snapshot.poll_attempts,
        snapshot.poll_declined_port,
        snapshot.poll_declined_port_source,
        snapshot.poll_declined_knob,
        snapshot.poll_declined_eligibility,
        snapshot.poll_declined_shape,
        snapshot.poll_declined_seam,
        machine.cpu.perf_counters().jit_direct_blocks_installed,
    );
    assert_eq!(
        snapshot.poll_skip_last_head, block_b_head_linear,
        "the certified shape's head must be the CHAINED successor block's own entry, not block \
         A's or anything build_poll_loop_from's backward scan wandered into"
    );
}

/// **Rank-3 killer**: the admissibility binary search's edge boundary. `admissible(n)` requires
/// `dots < edge_dots` (the projected dot position stays strictly on THIS side of the status-1
/// bit's next geometric edge). An off-by-one HIGH there lets a committed span cross the edge --
/// the guest's wait-for-retrace loop would read a bit that already flipped and miss its frame, a
/// wrong guest-visible answer with no counter that reports it.
///
/// Two assertions, of unequal strength -- both are worth having, but only the first is the real
/// invariant pin:
///
/// (a) **The invariant itself.** After the commit, publish `now_after` the same way
/// `port_read_al_dx` does (`bus.publish_core_clocks`), then re-read the status bit at
/// `bus.predicted_beam()` -- the REAL post-commit beam position, not a re-derivation of the
/// search's own arithmetic -- and require it to still read "spinning" by the request's own
/// polarity. A committed span that crossed the edge fails this directly, independent of anything
/// this file assumes about `edge_dots`'s numeric scale.
///
/// (b) **The search respects its own reported maximum.** An identically-seeded second machine
/// (`machine_b`, so neither call's bus/beam mutations can contaminate the other), demanding
/// `min_iterations = best + 1`, must decline. This is NOT a reliable off-by-one killer on its
/// own: `predicted_beam`'s dots-per-iteration granularity for this fixture is coarse enough that
/// even a deliberately introduced `<=`-for-`<` mutation (verified by hand while building this
/// fixture) left `best` completely unchanged -- the discrete dots(n) sequence never happens to
/// land exactly on `edge_dots` for this shape's clock ratio. (b) still catches a GROSS
/// mis-derivation of the bound (an `admissible` that stopped consulting the edge at all, for
/// example: `best` jumps from 3,587 to over 3.4 million with the edge check removed entirely in
/// this fixture), which is why it stays as a second, coarser check rather than being the only
/// one.
#[cfg(feature = "jit")]
#[test]
fn callout_poll_skip_commits_exactly_to_the_edge_boundary() {
    let mode = GswMode::Gsw586;
    let mut machine_a = poll_skip_seam_test_machine(mode);
    let mut machine_b = poll_skip_seam_test_machine(mode);

    let (best, mask, spins_when_bit_set, still_spinning) =
        with_cpu_and_bus(&mut machine_a, |cpu, bus| {
            let poll = warm_3slot_poll_loop(cpu, bus);
            let cap = cpu.core_clocks_so_far_for_test() + 10_000_000;
            let request = request_for(cpu, poll, cap);
            let outcome = bus
                .callout_poll_skip(&request)
                .expect("the 3-slot shape must commit a span");
            // Mirror `port_read_al_dx`'s own sequence exactly: publish the run-scoped core instant
            // BEFORE reading anything back, so `predicted_beam()` reflects the committed span.
            bus.publish_core_clocks(outcome.now_after);
            let mask = poll.status_mask();
            let spins_when_bit_set = poll.fresh_iteration_spins(mask);
            let bit_set = bus.vega.status1_bits(bus.predicted_beam()) & mask != 0;
            (
                outcome.iterations,
                mask,
                spins_when_bit_set,
                bit_set == spins_when_bit_set,
            )
        });
    assert!(
        best >= 2,
        "a committed span must be at least min_iterations, so there is a real edge to test \
         against"
    );
    assert!(
        still_spinning,
        "(a) INVARIANT: after committing best={best} iterations (mask={mask:#x}, \
         spins_when_bit_set={spins_when_bit_set}), the real post-commit beam position must still \
         read the SPINNING polarity -- an edge-crossing bug reports the opposite here"
    );

    let declined = with_cpu_and_bus(&mut machine_b, |cpu, bus| {
        let poll = warm_3slot_poll_loop(cpu, bus);
        let cap = cpu.core_clocks_so_far_for_test() + 10_000_000;
        let mut request = request_for(cpu, poll, cap);
        request.min_iterations = best + 1;
        bus.callout_poll_skip(&request)
    });
    assert!(
        declined.is_err(),
        "(b) best={best} was the search's own maximum; a request that DEMANDS best+1 must \
         decline (an admit here means the bound was grossly mis-derived, e.g. the edge check \
         dropped out entirely): {declined:?}"
    );
}

/// **M-29, the OFF arm through native codegen** (the shipped default is ON since the 2026-08-27
/// flip; this fixture forces both arms through the override, so it is default-independent). With
/// the knob OFF the certified shape must not skip and every attempt must land in `decline_knob`;
/// the SAME fixture with the knob forced ON must engage. The paired positive arm is what stops
/// this being an absence assertion (design §9's own rule). The default itself is pinned by
/// `direct_poll_skip_ships_on_by_default` in `cpu_jit_poll_skip_test.rs`.
#[cfg(feature = "jit")]
#[test]
fn direct_poll_skip_off_arm_declines_through_native_codegen() {
    let program = [0xec, 0xa8, 0x08, 0x75, 0xfb];
    let build = |armed: bool| {
        let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
        profile.cpu = GswMode::Gsw586;
        let mut machine = Machine::new_raw_program(profile, &program).unwrap();
        machine.set_jit_auto_admit(true);
        machine.cpu.set_native_backend_enabled(true);
        machine.cpu.set_direct_poll_skip_override(Some(armed));
        let mut cs = machine.cpu.registers.cs();
        cs.default_size_32 = true;
        cs.limit = u32::MAX;
        machine.cpu.registers.set_segment(SegmentIndex::Cs, cs);
        machine.cpu.registers.set_edx(0x03da);
        machine.cpu.registers.eip = 0x100;
        machine.trace.set_tracing_mode(TracingMode::Off);
        set_status1_bit(&mut machine, 0x08, true);
        machine
    };

    let mut off = build(false);
    off.run_cycles(200_000).expect("must not stop");
    let off_snapshot = off.cpu.direct_stall_snapshot();
    assert_eq!(
        off_snapshot.poll_skip_spans.iter().sum::<u64>(),
        0,
        "the OFF arm must never commit a span"
    );
    assert!(
        off_snapshot.poll_declined_knob > 0,
        "an OFF-arm port call-out must decline through the named knob lane, not silently"
    );

    let mut on = build(true);
    on.run_cycles(2_000_000).expect("must not stop");
    assert!(
        on.cpu
            .direct_stall_snapshot()
            .poll_skip_spans
            .iter()
            .sum::<u64>()
            > 0,
        "the ON arm must engage on the same fixture"
    );
}

/// **BLOCKER B's fix, exercised.** `poll_bus_certificate_from` (the split entry point the seam
/// uses) must certify the SAME `raw_clocks_per_iteration` the `PollLoop`-typed
/// `poll_bus_certificate` certifies for the identical fetch set -- the whole point of sharing
/// `poll_fetch_certificate_raw_from` between them.
#[cfg(feature = "jit")]
#[test]
fn the_pod_certificate_matches_the_poll_loop_certificate() {
    let mut machine = poll_skip_seam_test_machine(GswMode::Gsw586);
    with_cpu_and_bus(&mut machine, |cpu, bus| {
        let poll = warm_3slot_poll_loop(cpu, bus);
        let via_poll_loop = bus
            .poll_bus_certificate(poll, crate::bus::POLL_SKIP_IO_PORT)
            .expect("PollLoop-typed certificate");
        let request = request_for(cpu, poll, u64::MAX);
        // The seam's own certification path, isolated: call it the same way
        // `callout_poll_skip` does internally, through the request's `fetches` slice.
        let outcome = bus.callout_poll_skip(&request);
        // The outcome's committed bus clocks are `raw_clocks_per_iteration * iterations`; recover
        // the per-iteration rate and compare.
        if let Ok(outcome) = outcome
            && outcome.iterations > 0
        {
            assert_eq!(
                outcome.committed_raw_bus_clocks / outcome.iterations,
                via_poll_loop.raw_clocks_per_iteration(),
                "the POD-fetches certificate must match the PollLoop certificate exactly"
            );
        }
    });
}

/// **M-19/M-20 (split lanes).** Disabling the VGA's lazy-port-read path (which
/// `poll_bus_certificate_from` requires) must decline through `BusCertificate`; disabling the
/// status port's own active check must decline through `PortInactive`. Both restore to a
/// commit on the same fixture.
#[cfg(feature = "jit")]
#[test]
fn callout_poll_skip_names_which_screen_declined() {
    let mut machine = poll_skip_seam_test_machine(GswMode::Gsw586);
    with_cpu_and_bus(&mut machine, |cpu, bus| {
        let poll = warm_3slot_poll_loop(cpu, bus);
        let cap = cpu.core_clocks_so_far_for_test() + 10_000_000;

        bus.trace.set_tracing_mode(TracingMode::Full);
        let request = request_for(cpu, poll, cap);
        assert_eq!(
            bus.callout_poll_skip(&request).err(),
            Some(izarravm_bus::CalloutPollDecline::BusCertificate),
            "tracing armed must decline through BusCertificate, not silently"
        );
        bus.trace.set_tracing_mode(TracingMode::Off);

        let request = request_for(cpu, poll, cap);
        let outcome = bus.callout_poll_skip(&request);
        assert!(
            outcome.is_ok(),
            "clearing tracing on the same fixture must let the span commit: {outcome:?}"
        );
    });
}

/// **M-24 / BLOCKER B4, the miscompile-class killer, through NATIVE codegen.** `PortReadAlDx`
/// must never clobber the block's live EFLAGS shadow (RBP): a block shaped `STC` / `IN AL,DX`
/// (0x3DA) / `ADC` reads carry out of RBP directly (`emit_carry_alu_preloaded`), so a spurious
/// RBP reload after the port call-out would flip the ADC's carry input to whatever stale value
/// happened to be in memory. This block is NOT a certified poll shape (`STC`/`IN`/`ADC` matches
/// none of `build_poll_loop_from`'s arms), so there is no HIT conjunct here on purpose -- see the
/// design review's BLOCKER E finding, which this test adopts by omitting it.
///
/// **Rebuilt from the original ADD/IN/ADC fixture (revision report, M-24 replacement) because its
/// stated invariant was false: `IN AL,DX` OVERWRITES AL, so `AL == 1` after the original fixture
/// held only by accident -- iff the 0x3DA byte on the loop's last iteration happened to be
/// exactly `0x00` (adversarial review M2). Worse, timing-contract amendment C4 releases 0x3DA
/// bits 4-5 to 86Box's model, which would make that byte NON-DETERMINISTIC and break the fixture
/// for a reason that has nothing to do with RBP.**
///
/// The fix: sink the carry into a register `IN` never touches. `mov ecx,200; [head] stc; in
/// al,dx; adc bl,0; dec ecx; jnz head; hlt`. `stc` re-establishes CF=1 every iteration
/// (independent of anything the port returns), `in al,dx` only ever writes AL, and `adc bl,0`
/// adds exactly 1 to BL when CF survives the call-out intact. Under the correct gate `BL` reaches
/// exactly `200`, deterministically, for ANY 0x3DA byte sequence -- C4-proof by construction.
/// Under the bug (RBP reloaded from a stale, CF-clear memory shadow after the call-out) `ADC
/// BL,0` sees CF=0 every time and `BL` stays `0`.
const STC_IN_ADC_PROGRAM: [u8; 13] = [
    0xb9, 0xc8, 0x00, 0x00, 0x00, // mov ecx, 200
    0xf9, // [head] stc
    0xec, // in al, dx
    0x80, 0xd3, 0x00, // adc bl, 0
    0x49, // dec ecx
    0x75, 0xf8, // jnz head
];

#[cfg(feature = "jit")]
#[test]
fn a_port_callout_preserves_the_blocks_eflags_shadow() {
    let mut program = STC_IN_ADC_PROGRAM.to_vec();
    program.push(0xf4); // hlt
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = GswMode::Gsw586;
    let mut machine = Machine::new_raw_program(profile, &program).unwrap();
    machine.set_jit_auto_admit(true);
    machine.cpu.set_native_backend_enabled(true);
    machine.cpu.set_direct_poll_skip_override(Some(true));
    let mut cs = machine.cpu.registers.cs();
    cs.default_size_32 = true;
    cs.limit = u32::MAX;
    machine.cpu.registers.set_segment(SegmentIndex::Cs, cs);
    machine.cpu.registers.set_edx(0x03da);
    machine.cpu.registers.eip = 0x100;
    machine.cpu.registers.set_eax(0);
    machine.cpu.registers.set_ebx(0);
    machine.trace.set_tracing_mode(TracingMode::Off);
    machine
        .run_until_halt_or_cycles(2_000_000)
        .expect("the fixture must not stop the machine");
    assert!(machine.cpu.halted, "the STC/IN/ADC loop did not halt");
    assert!(
        machine.cpu.direct_stall_snapshot().poll_attempts > 0,
        "the emitted port call-out never ran, so this test proves nothing (M5: \
         jit_direct_blocks_installed alone does not prove IT ran): {:#?}",
        machine.cpu.perf_counters()
    );
    assert_eq!(
        machine.cpu.registers.ebx() & 0xff,
        200,
        "ADC read a clobbered carry: the port call-out reloaded RBP from a stale EFLAGS shadow. \
         This assertion is independent of the 0x3DA byte's value (unlike the fixture it replaced)"
    );
}

// ============================================================================================
// GP2 poll-skip REVISION: BLOCKER 1 / BLOCKER 2 killers (2026-08-26 revision report)
// ============================================================================================

/// **Rank-1 killer (revision report §4 item 1) -- the acceptance test for BLOCKER 1's fix.**
/// `MachineBus::publish_core_clocks` used to zero `prior_runs_core_clocks` on every call-out
/// attempt, decline or commit alike, corrupting the batch instant every lazy prediction after it
/// reads (`predicted_beam`, the seam's own edge test, the lazy PIT/OPL peeks) for the REST of the
/// run. Every other fixture in this file runs with `prior_runs_core_clocks == 0` at the attempt
/// (a fresh `machine.make_bus()`, or the first run of a batch), which is exactly why this defect
/// was invisible to the whole suite -- corroborated by the 2026-08-26 diagnostic pair (C1
/// conservation held to 1.1 ppb while the frame hash moved: the charges were right, a prediction
/// INPUT was wrong).
///
/// Two assertions, both of which fail on the pre-fix code (reverting `publish_core_clocks` to
/// `self.prior_runs_core_clocks = 0; self.core_clocks_so_far = now;` makes both red):
///
/// (a) `bus.prior_runs_core_clocks` is BYTE-IDENTICAL before and after the call-out attempt --
///     the direct, mechanical proof that nothing clears it.
/// (b) The batch instant is actually CONSULTED, not merely preserved-but-ignored: two otherwise
///     identical machines that differ only in `prior_runs_core_clocks` at the attempt must predict
///     DIFFERENT beams for the same instant, and (via `poll_project_dot_advance`, used internally
///     by `callout_poll_skip`'s admissibility search) the two admit different iteration counts
///     for the identical certified shape.
#[cfg(feature = "jit")]
#[test]
fn prior_runs_core_clocks_is_nonzero_on_a_second_batch_run() {
    // A batch-scoped core total large enough to move the beam by a measurable, non-wrapping
    // amount: a few million core clocks is comfortably inside one VGA frame's worth of dots at
    // the persona's timing ratio, so the two predicted beams below are guaranteed to differ
    // rather than happening to land on the same phase by coincidence of a frame wrap.
    const BATCH_CORE_BASELINE: u64 = 4_000_000;

    // (a) The direct preservation proof. `port_read_al_dx`'s own sequence, reproduced exactly:
    // `bus.publish_core_clocks(now)` runs BEFORE `bus.callout_poll_skip(&request)` on every
    // attempt, decline or commit alike -- so this test must call it too, or it exercises a
    // function the real defect never lived in.
    let mut machine = poll_skip_seam_test_machine(GswMode::Gsw586);
    let (before, after) = with_cpu_and_bus(&mut machine, |cpu, bus| {
        let poll = warm_3slot_poll_loop(cpu, bus);
        bus.prior_runs_core_clocks = BATCH_CORE_BASELINE;
        let before = bus.prior_runs_core_clocks;
        let now = cpu.core_clocks_so_far_for_test();
        let cap = now + 10_000_000;
        let request = request_for(cpu, poll, cap);
        bus.publish_core_clocks(now);
        let _ = bus.callout_poll_skip(&request);
        (before, bus.prior_runs_core_clocks)
    });
    assert_eq!(
        before, after,
        "MachineBus::publish_core_clocks must never touch prior_runs_core_clocks -- it read \
         {after} after the call-out attempt, started at {before}"
    );

    // (b) The differential: the batch baseline must actually move the predicted beam, and move
    // what the seam admits.
    let mut zero_baseline = poll_skip_seam_test_machine(GswMode::Gsw586);
    let beam_zero = with_cpu_and_bus(&mut zero_baseline, |cpu, bus| {
        let _ = warm_3slot_poll_loop(cpu, bus);
        bus.prior_runs_core_clocks = 0;
        bus.predicted_beam()
    });

    let mut nonzero_baseline = poll_skip_seam_test_machine(GswMode::Gsw586);
    let beam_nonzero = with_cpu_and_bus(&mut nonzero_baseline, |cpu, bus| {
        let _ = warm_3slot_poll_loop(cpu, bus);
        bus.prior_runs_core_clocks = BATCH_CORE_BASELINE;
        bus.predicted_beam()
    });

    assert_ne!(
        beam_zero, beam_nonzero,
        "predicted_beam() must depend on prior_runs_core_clocks: a {BATCH_CORE_BASELINE}-clock \
         batch baseline predicted the SAME beam as a zero baseline, which is exactly what \
         zeroing prior_runs_core_clocks inside publish_core_clocks used to cause"
    );
}

/// **BLOCKER 2's fix, exercised.** `req.cap` is the RUN's remaining budget -- already net of the
/// batch's prior bus clocks -- but `MachineBus::poll_project_scaled_bus_clocks` reads a
/// BATCH-ABSOLUTE total. Before this fix the seam compared the run-remaining `cap` against the
/// batch-absolute bus reading directly, subtracting the batch's prior bus clocks from the budget
/// a SECOND time (the first subtraction is already baked into `req.cap` by the caller). The fix
/// (`bus_scaled_at_run_entry` + `bus_growth`) turns the batch-absolute reading into the run-scoped
/// GROWTH the cap actually bounds.
///
/// This test inflates the batch's bus trace by a known amount, then submits two otherwise
/// identical requests that differ ONLY in `bus_scaled_at_run_entry`, in an order chosen so the
/// FIRST call cannot mutate `bus.trace` and so contaminate the second (`callout_poll_skip`'s only
/// side effect is `poll_commit_bus` on a commit, so the DECLINING request must run first): one
/// claims none of the inflated bus total belongs to a prior run (`0`, reproducing the pre-fix
/// effective behaviour -- the full batch-absolute reading counts as this run's own spend) under a
/// cap too tight to admit it; the other, run second under the SAME cap, claims all of it does (the
/// batch-absolute reading itself, reproducing "this run just started, no bus growth yet") and
/// must commit. If `bus_growth` collapsed back to the batch-absolute reading (the pre-fix
/// defect), the second call would decline identically to the first and this test would fail on
/// its `Ok` assertion.
#[cfg(feature = "jit")]
#[test]
fn bus_growth_not_batch_absolute_bus_bounds_the_cap_test() {
    let mut machine = poll_skip_seam_test_machine(GswMode::Gsw586);
    with_cpu_and_bus(&mut machine, |cpu, bus| {
        let poll = warm_3slot_poll_loop(cpu, bus);
        let certificate = bus
            .poll_bus_certificate(poll, crate::bus::POLL_SKIP_IO_PORT)
            .expect("the warmed 3-slot shape must certify a bus cost");
        // Inflate the batch's bus trace by a known raw amount, so the batch-absolute scaled bus
        // reading is large and easy to reason about.
        bus.trace.add_elapsed_clocks(200);
        let batch_bus_now = bus
            .poll_project_scaled_bus_clocks(certificate, 0)
            .expect("scaled bus projection must succeed");
        assert!(
            batch_bus_now > 0,
            "the injected raw bus clocks must show up scaled"
        );

        let now = cpu.core_clocks_so_far_for_test();
        // Between "admits min_iterations (2) if the run-scoped bus growth is ~0" (needs only a
        // few scaled clocks of headroom for the shape's 17-raw-core-clock-per-iteration charge)
        // and "declines outright if the full batch-absolute bus reading is charged as this run's
        // own growth" (adds `batch_bus_now` on top).
        let cap = now + batch_bus_now.saturating_sub(20);

        // FIRST: baseline 0 claims the full batch-absolute reading as this run's own growth, so
        // `run_spent(0) = now + batch_bus_now > cap` -- must decline, and a decline never touches
        // `bus.trace`, so the second call below still sees the same `batch_bus_now`.
        let charges_full_batch_bus = request_for_with_bus_baseline(cpu, poll, cap, 0);
        let outcome = bus.callout_poll_skip(&charges_full_batch_bus);
        assert!(
            outcome.is_err(),
            "charging the full batch-absolute bus reading ({batch_bus_now}) as this run's own \
             growth must exceed a {cap}-clock cap: got {outcome:?}"
        );

        // SECOND: baseline batch_bus_now claims none of it is this run's own growth (`run_spent(0)
        // = now + 0`), comfortably under the same cap -- must commit.
        let charges_only_run_growth = request_for_with_bus_baseline(cpu, poll, cap, batch_bus_now);
        let outcome = bus.callout_poll_skip(&charges_only_run_growth);
        assert!(
            outcome.is_ok(),
            "claiming ALL of the batch-absolute bus belongs to a prior run (this run's own \
             growth is ~0) must admit under the SAME {cap}-clock cap that just declined the \
             first request: got {outcome:?}. If this declines identically, `bus_growth` \
             collapsed back to the batch-absolute reading (BLOCKER 2)"
        );
    });
}

/// **Rank-4 killer**: the 32-bit return-lane bound, `bus::poll_skip_upper_bound`. No fixture in
/// this file (or, per the diagnostic pair, any real game corpus) ever produces a span within
/// four orders of magnitude of `LANE_SAFETY_CEILING` -- the status-1 edge always binds first, by
/// a huge margin (`callout_poll_skip_commits_exactly_to_the_edge_boundary` measured `best` in the
/// low thousands against an unbounded-edge ceiling of 3.4 MILLION for the identical fixture) --
/// so a future edit to the clamp expression, or to `IZARRAVM_DIRECT_POLL_MAX_RAW`'s default,
/// would ship silently and corrupt the clock lane (or set a phantom step-break bit) rather than
/// fault. A pure-function unit test on the clamp alone, no machine and no VGA/beam state
/// involved -- exactly what the review asked for.
#[cfg(feature = "jit")]
#[test]
fn poll_skip_upper_bound_never_lets_the_32_bit_lane_overflow() {
    // "IZARRAVM_DIRECT_POLL_MAX_RAW set absurdly high plus a huge cap": `u64::MAX` for both the
    // cap and the caller's own max, `spent = 0` (a fresh run, no budget consumed yet), and a
    // realistic `raw_core_clocks` (17, the 3-slot shape's own value elsewhere in this file).
    let upper = crate::bus::poll_skip_upper_bound(u64::MAX, 0, u64::MAX, 17);
    let raw_core_clocks = 17u64;
    let skipped_raw_core_clocks = raw_core_clocks * upper;
    assert!(
        skipped_raw_core_clocks < u64::from(u32::MAX),
        "raw_core_clocks * upper must stay under 2^32 (bit 32 is the step-break status bit) even \
         with an absurd cap and max_skipped_raw: upper={upper}, product={skipped_raw_core_clocks}"
    );
    // The margin: `IN_PORT_CORE_CLOCKS` (12, `izarravm-cpu`, not visible from this crate) plus
    // this method's own return must still clear 2^32 -- `crate::bus::LANE_SAFETY_CEILING` reserves
    // 64 clocks for exactly this, so 12 fits with headroom to spare.
    const IN_PORT_CORE_CLOCKS_FOR_TEST: u64 = 12;
    assert!(
        skipped_raw_core_clocks + IN_PORT_CORE_CLOCKS_FOR_TEST < u64::from(u32::MAX),
        "raw_core_clocks * upper + IN_PORT_CORE_CLOCKS must stay under 2^32: got {}",
        skipped_raw_core_clocks + IN_PORT_CORE_CLOCKS_FOR_TEST
    );
    // `raw_core_clocks * upper` must be AT the ceiling (not far under it): a huge cap and a huge
    // max_skipped_raw mean the LANE_SAFETY_CEILING term is the only thing left binding `upper`,
    // so this pins the clamp is actually reachable, not just conservative.
    assert_eq!(
        raw_core_clocks * upper,
        (crate::bus::LANE_SAFETY_CEILING / raw_core_clocks) * raw_core_clocks,
        "with cap and max_skipped_raw both absurdly high, LANE_SAFETY_CEILING must be the sole \
         binding term (integer division truncation is the only slack): upper={upper}"
    );

    // A tiny cap must still win over an absurd max_skipped_raw (the `.min` composition, not just
    // the ceiling term alone).
    let capped = crate::bus::poll_skip_upper_bound(100, 0, u64::MAX, 17);
    assert_eq!(
        capped, 99,
        "cap=100, spent=0 must bound `upper` to 99 (cap - spent - 1) regardless of max_skipped_raw"
    );
}

// ---------------------------------------------------------------------------
// The 16-bit poll certification slice (IZARRAVM_DIRECT_POLL_SKIP_16, D1 + D1b).
// ---------------------------------------------------------------------------

/// Build the 16-bit spin fixture: `program` at guest offset 0x100 in the raw loader's own
/// 16-bit code segment, the Direct backend armed, both poll knobs overridden per-CPU, and
/// the beam positioned so the shape is CURRENTLY spinning.
#[cfg(feature = "jit")]
fn sixteen_bit_spin_machine(program: &[u8], ax: u32, poll_skip_16: bool) -> Machine {
    sixteen_bit_spin_machine_for_mask(program, ax, poll_skip_16, 0x08)
}

/// `sixteen_bit_spin_machine` with an explicit status1 mask to position the beam against.
/// Both `0x01` (display enable) and `0x08` (vretrace) are exercised, because they are the
/// only two bits the analytic edge oracle understands and because a fixture stuck on one
/// of them cannot tell a correctly-resolved mask from a hardcoded one.
#[cfg(feature = "jit")]
fn sixteen_bit_spin_machine_for_mask(
    program: &[u8],
    ax: u32,
    poll_skip_16: bool,
    mask: u8,
) -> Machine {
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = GswMode::Gsw586;
    let mut machine = Machine::new_raw_program(profile, program).unwrap();
    machine.set_jit_auto_admit(true);
    machine.cpu.set_native_backend_enabled(true);
    machine.cpu.set_direct_poll_skip_override(Some(true));
    machine
        .cpu
        .set_direct_poll_skip_16_override(Some(poll_skip_16));
    machine.cpu.set_poll_neg_cache_enabled_for_test(true);
    // The loader's own cs is left alone: 16-bit, limit 0xFFFF -- the state the slice is for.
    assert!(!machine.cpu.registers.cs().default_size_32);
    assert!(machine.cpu.registers.cs().limit <= 0xffff);
    machine.cpu.registers.set_edx(0x03da);
    machine.cpu.registers.set_eax(ax);
    machine.cpu.registers.eip = 0x100;
    machine.trace.set_tracing_mode(TracingMode::Off);
    set_status1_bit(&mut machine, mask, true);
    machine
}

/// **T-D5, the D1 half.** A 16-bit certified 3-slot loop with the new knob ON commits a
/// span through the REAL bus, from the EMITTED call-out: `poll_skip_spans.sum() > 0`,
/// `poll_skip_last_head` equal to the shape's own head linear, and
/// `skipped_raw_core_clocks == 112 * iterations` -- the same arithmetic identity the
/// 32-bit slice shipped with.
///
/// It also pins round-2 MINOR-6: with knob-first ordering the 16-bit screen's condition is
/// `!d && !armed`, so on the ON arm `poll_declined_sixteen_bit` reads exactly ZERO. A
/// ladder leg reading zero there is the mechanism working, not a broken build.
#[cfg(feature = "jit")]
#[test]
fn a_sixteen_bit_poll_loop_commits_a_span_with_the_knob_on() {
    let mut machine = sixteen_bit_spin_machine(&[0xec, 0xa8, 0x08, 0x75, 0xfb], 0, true);
    let head = machine.cpu.registers.cs().base + 0x100;
    machine
        .run_cycles(2_000_000)
        .expect("the fixture must not stop the machine");
    let snapshot = machine.cpu.direct_stall_snapshot();
    let spans: u64 = snapshot.poll_skip_spans.iter().sum();
    assert!(
        spans > 0,
        "a 16-bit 3-slot loop must commit a span with the knob ON; attempts={} declined(\
         port={} port_source={} mask_source={} knob={} eligibility={} sixteen_bit={} shape={} \
         cap={} seam={}) blocks_installed={}",
        snapshot.poll_attempts,
        snapshot.poll_declined_port,
        snapshot.poll_declined_port_source,
        snapshot.poll_declined_mask_source,
        snapshot.poll_declined_knob,
        snapshot.poll_declined_eligibility,
        snapshot.poll_declined_sixteen_bit,
        snapshot.poll_declined_shape,
        snapshot.poll_declined_cap,
        snapshot.poll_declined_seam,
        machine.cpu.perf_counters().jit_direct_blocks_installed,
    );
    assert_eq!(
        snapshot.poll_declined_sixteen_bit, 0,
        "on the ON arm the 16-bit lane GOES TO ZERO -- the screen's condition is \
         `!d && !armed`, so it can never be true"
    );
    assert_eq!(
        snapshot.poll_skip_last_head, head,
        "the committed span's head must be the shape's own head linear"
    );
    let iterations: u64 = snapshot.poll_skip_iterations.iter().sum();
    assert_eq!(
        snapshot.poll_skip_raw_core_clocks,
        112 * iterations,
        "the 3-slot shape's raw core charge must be exactly 17 per iteration in 16-bit \
         code, the same constant the 32-bit arm charges"
    );
    assert_eq!(
        machine.cpu.perf_counters().poll_skip_spans,
        0,
        "the interpreter's own poll-skip commit must stay zero on a Direct row"
    );
}

#[cfg(feature = "jit")]
#[test]
fn epoch_two_direct_sixteen_bit_poll_charges_the_live_in_price() {
    for (mode, raw_per_iteration) in [(GswMode::Gsw486, 132), (GswMode::Gsw586, 112)] {
        for (program, ax) in [
            (&[0xec, 0xa8, 0x08, 0x75, 0xfb][..], 0),
            (&[0xec, 0x84, 0xe0, 0x75, 0xfb][..], 0x0800),
        ] {
            let mut machine = sixteen_bit_spin_machine(program, ax, true);
            machine.set_mode(mode);
            machine.run_cycles(mode.clock_hz() / 30).unwrap();
            let snapshot = machine.cpu.direct_stall_snapshot();
            let iterations: u64 = snapshot.poll_skip_iterations.iter().sum();
            assert!(iterations > 1, "mode={mode:?} ax={ax}");
            assert!(machine.cpu.perf_counters().jit_direct_insns > 0);
            assert_eq!(
                snapshot.poll_skip_raw_core_clocks,
                raw_per_iteration * iterations
            );
        }
    }
}

/// **T-D5, the D1b half.** The same, for the register-mask form `IN AL,DX / TEST AL,AH /
/// JNZ` with `MOV AH,8`'s effect preloaded into AH. This is the shape that carries 81.68%
/// of tyrian's declines and the single hottest site.
#[cfg(feature = "jit")]
#[test]
fn a_sixteen_bit_register_mask_poll_loop_commits_a_span_with_the_knob_on() {
    let mut machine = sixteen_bit_spin_machine(&[0xec, 0x84, 0xe0, 0x75, 0xfb], 0x0800, true);
    let head = machine.cpu.registers.cs().base + 0x100;
    machine
        .run_cycles(2_000_000)
        .expect("the fixture must not stop the machine");
    let snapshot = machine.cpu.direct_stall_snapshot();
    let spans: u64 = snapshot.poll_skip_spans.iter().sum();
    assert!(
        spans > 0,
        "a 16-bit TEST AL,AH loop must commit a span with the knob ON; attempts={} declined(\
         port_source={} mask_source={} sixteen_bit={} shape={} cap={} seam={})",
        snapshot.poll_attempts,
        snapshot.poll_declined_port_source,
        snapshot.poll_declined_mask_source,
        snapshot.poll_declined_sixteen_bit,
        snapshot.poll_declined_shape,
        snapshot.poll_declined_cap,
        snapshot.poll_declined_seam,
    );
    assert_eq!(snapshot.poll_skip_last_head, head);
    let iterations: u64 = snapshot.poll_skip_iterations.iter().sum();
    assert_eq!(
        snapshot.poll_skip_raw_core_clocks,
        112 * iterations,
        "D1b shares the 3-slot constant unchanged: 0x84 charges clocks(2) exactly as 0xA8 \
         does, so raw_core_clocks stays 17"
    );
    assert_eq!(
        snapshot.poll_skip_spans[0], spans,
        "D1b keeps diagnostic_class 0 -- it changes neither the port source nor the branch \
         shape, so no fourth poll_skip_spans class is needed"
    );
}

/// **T-D1b-3.** A structurally-certified D1b shape whose live AH is NOT `0x01`/`0x08`
/// declines through its OWN counted lane and writes NO negative-cache entry.
///
/// Both halves matter. The dedicated lane is what lets a ladder tell "the shape did not
/// certify" (`_shape`) from "the shape certified and then declined semantically"
/// (`_mask_source`) -- and the second is the storm-class signature, because a `Found` is
/// never cached. The absent cache entry is what keeps the register half of the shape out
/// of a `(lin, d)`-keyed structure it does not belong in: the SAME BYTES with `AH = 8`
/// must still certify, which the sibling fixture above proves.
#[cfg(feature = "jit")]
#[test]
fn a_wrong_mask_value_declines_through_its_own_lane_without_caching() {
    // `in al,dx; test al,ah; JZ $-5` with `AH = 0x40`. The branch sense is inverted from the
    // sibling fixtures deliberately: status1 never sets bit 6, so this spins FOREVER on a mask
    // the analytic edge oracle cannot read -- which is exactly the persistent wrong-mask site
    // the sticky memo exists for. With `JNZ` the loop would fall out after one iteration and
    // the call-out would never run enough to say anything.
    let mut machine = sixteen_bit_spin_machine(&[0xec, 0x84, 0xe0, 0x74, 0xfb], 0x4000, true);
    machine
        .run_cycles(2_000_000)
        .expect("the fixture must not stop the machine");
    let snapshot = machine.cpu.direct_stall_snapshot();
    let perf = machine.cpu.perf_counters();
    assert!(
        snapshot.poll_declined_mask_source > 0,
        "AH=0x40 must decline through the mask-source lane; attempts={} shape={} \
         port_source={}",
        snapshot.poll_attempts,
        snapshot.poll_declined_shape,
        snapshot.poll_declined_port_source,
    );
    assert_eq!(
        snapshot.poll_skip_spans.iter().sum::<u64>(),
        0,
        "a mask the edge oracle cannot read must never commit a span"
    );
    assert_eq!(
        perf.poll_neg_cache_stores, 0,
        "the register half of a shape must never become a cached negative: the same bytes \
         certify once AH holds 0x08"
    );
    assert_eq!(
        snapshot.poll_declined_shape, 0,
        "the mask decline must not be misattributed to the structural lane"
    );
}

/// The 16-bit slice's OFF arm, and the ladder's A arm: with the new knob unset, a 16-bit
/// call-out is screened out exactly as on `main` -- into `poll_declined_sixteen_bit`, with
/// no scan and no span. This is what makes the rollback claim ("unset restores the current
/// behaviour exactly") true rather than aspirational.
#[cfg(feature = "jit")]
#[test]
fn the_sixteen_bit_off_arm_screens_before_the_scan() {
    for (name, program, ax) in [
        ("D1", [0xec, 0xa8, 0x08, 0x75, 0xfb], 0),
        ("D1b", [0xec, 0x84, 0xe0, 0x75, 0xfb], 0x0800),
    ] {
        let mut machine = sixteen_bit_spin_machine(&program, ax, false);
        machine
            .run_cycles(2_000_000)
            .expect("the fixture must not stop the machine");
        let snapshot = machine.cpu.direct_stall_snapshot();
        assert!(
            snapshot.poll_declined_sixteen_bit > 0,
            "{name}: the OFF arm must land in the sixteen-bit lane; attempts={}",
            snapshot.poll_attempts,
        );
        assert_eq!(
            snapshot.poll_declined_shape, 0,
            "{name}: no scan may run on the OFF arm -- the shape lane counts scans that ran"
        );
        assert_eq!(
            snapshot.poll_declined_mask_source, 0,
            "{name}: the OFF arm never reaches the mask check"
        );
        assert_eq!(
            snapshot.poll_skip_spans.iter().sum::<u64>(),
            0,
            "{name}: the OFF arm must commit nothing"
        );
        assert_eq!(
            machine.cpu.perf_counters().poll_neg_cache_stores,
            0,
            "{name}: the OFF arm must not probe or store the negative cache"
        );
    }
}

/// **THE FLIP PIN.** With the per-CPU override left at `None` -- i.e. reading the AMBIENT
/// `IZARRAVM_DIRECT_POLL_SKIP_16` exactly as a shipped build does -- a 16-bit certified loop
/// must commit a span. That is the 2026-08-29 default-ON flip, asserted end to end through
/// the emitted call-out rather than only at the spelling table.
///
/// Skipped, not failed, when the suite is run with the variable explicitly set to the OFF arm:
/// the ladder legs that do that are legitimate, and a row that cannot be run on both arms is
/// worse than one that says which arm it saw.
#[cfg(feature = "jit")]
#[test]
fn the_sixteen_bit_arm_is_on_by_default_end_to_end() {
    let ambient = std::env::var("IZARRAVM_DIRECT_POLL_SKIP_16");
    let armed = match ambient.as_deref() {
        Err(_) | Ok("") => true,
        Ok(raw) => !matches!(raw.trim().to_ascii_lowercase().as_str(), "0" | "off"),
    };
    if !armed {
        return;
    }
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = GswMode::Gsw586;
    let mut machine = Machine::new_raw_program(profile, &[0xec, 0xa8, 0x08, 0x75, 0xfb]).unwrap();
    machine.set_jit_auto_admit(true);
    machine.cpu.set_native_backend_enabled(true);
    machine.cpu.set_direct_poll_skip_override(Some(true));
    // NO `set_direct_poll_skip_16_override` -- the ambient reading is the point of this row.
    machine.cpu.set_poll_neg_cache_enabled_for_test(true);
    assert!(!machine.cpu.registers.cs().default_size_32);
    machine.cpu.registers.set_edx(0x03da);
    machine.cpu.registers.eip = 0x100;
    machine.trace.set_tracing_mode(TracingMode::Off);
    set_status1_bit(&mut machine, 0x08, true);

    machine
        .run_cycles(2_000_000)
        .expect("the fixture must not stop the machine");
    let snapshot = machine.cpu.direct_stall_snapshot();
    assert!(
        snapshot.poll_skip_spans.iter().sum::<u64>() > 0,
        "the shipped default must certify a 16-bit poll loop; attempts={} sixteen_bit={}          shape={} mask_source={}",
        snapshot.poll_attempts,
        snapshot.poll_declined_sixteen_bit,
        snapshot.poll_declined_shape,
        snapshot.poll_declined_mask_source,
    );
    assert_eq!(
        snapshot.poll_declined_sixteen_bit, 0,
        "on the default arm the 16-bit screen can never be true"
    );
}
