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

use super::*;
#[cfg(feature = "jit")]
use izarravm_cpu::PollLoop;

const HEAD: u32 = 0x103;

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

/// Build the request the Direct call-out would build, from a certified `PollLoop`. Mirrors
/// `port_read_al_dx`'s own composition exactly (same fields, same `level_timing` dial, same
/// `poll_skip_timing_remainder`), so this is testing the SAME seam contract that helper drives.
#[cfg(feature = "jit")]
fn request_for(cpu: &CpuGsw, poll: PollLoop, cap: u64) -> CalloutPollSkipRequest {
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
        raw_core_clocks: poll.raw_core_clocks(),
        core_clocks_at_block_entry: cpu.core_clocks_so_far_for_test(),
        prefix_raw: 0,
        core_num,
        core_den,
        timing_rem: cpu.poll_skip_timing_remainder(),
        cap,
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
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = mode;
    let mut machine = Machine::new_raw_program(profile, &program).unwrap();
    let mut cs = machine.cpu.registers.cs();
    cs.default_size_32 = true;
    cs.limit = u32::MAX;
    machine.cpu.registers.set_segment(SegmentIndex::Cs, cs);
    machine.cpu.registers.set_edx(0x03da);
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
            17 * outcome.iterations,
            "mode={mode:?}: the 3-slot shape's raw core charge must be exactly 17 per iteration"
        );
    }
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

/// **M-29, `direct_poll_skip_ships_off_by_default`, through native codegen.** With the knob OFF
/// the certified shape must not skip and every attempt must land in `decline_knob`; the SAME
/// fixture with the knob forced ON must engage. The paired positive arm is what stops this being
/// an absence assertion (design §9's own rule).
#[cfg(feature = "jit")]
#[test]
fn direct_poll_skip_ships_off_by_default_through_native_codegen() {
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
            .poll_bus_certificate(poll)
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
/// must never clobber the block's live EFLAGS shadow (RBP): a block shaped `ADD` / `IN AL,DX`
/// (0x3DA) / `ADC` reads carry out of RBP directly (`emit_carry_alu_preloaded`), so a spurious
/// RBP reload after the port call-out would flip the ADC's carry input to whatever stale value
/// happened to be in memory. This block is NOT a certified poll shape (`ADD`/`IN`/`ADC` matches
/// none of `build_poll_loop_from`'s arms), so there is no HIT conjunct here on purpose -- see the
/// design review's BLOCKER E finding, which this test adopts by omitting it.
/// `mov ecx,200; [head] add al,0xFF; in al,dx; adc al,0x00; dec ecx; jnz head; hlt` at CS:0100.
/// `AL=1` is a FIXED POINT of the correct arithmetic: `ADD AL,0xFF` wraps `1` to `0` with CF=1
/// (sets RBP's carry bit), the port call-out must not touch RBP, and `ADC AL,0` with CF=1 reads
/// AL back to `1` -- every iteration, so 200 trips both warms the block for native admission AND
/// leaves a deterministic expected final AL regardless of exactly how many iterations ran natively
/// versus interpreted before admission. Under the bug (RBP clobbered to a stale CF=0 after the
/// call-out) AL instead DECREMENTS by one every iteration and never re-converges.
const ADD_IN_ADC_PROGRAM: [u8; 13] = [
    0xb9, 0xc8, 0x00, 0x00, 0x00, // mov ecx, 200
    0x04, 0xff, // [head] add al, 0xff
    0xec, // in al, dx
    0x14, 0x00, // adc al, 0
    0x49, // dec ecx
    0x75, 0xf8, // jnz head
];

#[cfg(feature = "jit")]
#[test]
fn a_port_callout_preserves_the_blocks_eflags_shadow() {
    let mut program = ADD_IN_ADC_PROGRAM.to_vec();
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
    machine.cpu.registers.set_eax(1);
    machine.trace.set_tracing_mode(TracingMode::Off);
    machine
        .run_until_halt_or_cycles(2_000_000)
        .expect("the fixture must not stop the machine");
    assert!(machine.cpu.halted, "the ADD/IN/ADC loop did not halt");
    assert!(
        machine.cpu.perf_counters().jit_direct_blocks_installed > 0,
        "the ADD/IN/ADC fixture never installed a native block, so this test proves nothing: {:#?}",
        machine.cpu.perf_counters()
    );
    assert_eq!(
        machine.cpu.registers.eax() & 0xff,
        1,
        "ADC read a clobbered carry: the port call-out reloaded RBP from a stale EFLAGS shadow"
    );
}
