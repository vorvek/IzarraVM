// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn reset_state_starts_at_386_reset_vector() {
    let cpu = CpuGsw::default();

    assert_eq!(cpu.registers.cs().selector, 0xf000);
    assert_eq!(cpu.registers.cs().base, 0xffff_0000);
    assert_eq!(cpu.registers.eip, 0xfff0);
    assert_eq!(cpu.linear_eip(), 0xffff_fff0);
}

#[test]
fn core_clocks_so_far_is_zero_for_an_in_as_the_runs_first_instruction_in_the_accurate_class() {
    // In the Accurate 386 class `block_continuable` never admits
    // `DecodeGroup::PortIo`, so an IN can only
    // ever be `run_straight_line`'s FIRST instruction there, never a
    // continuation -- every port access still sets `io_touched` unconditionally
    // in the Accurate class's read_io dispatch, ending the run right after it
    // runs. This test pins core_clocks_so_far == 0 for that first-instruction
    // position, explicitly on I386 so it does not silently start exercising the
    // Approximate-class continuation path if the CPU's default level ever
    // changes. See the sibling test for the Approximate-class continuation case.
    let code = [0xec]; // in al,dx
    let (mut cpu, memory) = real_mode_cpu(&code, 32);
    cpu.set_mode(GswMode::Gsw386);
    let mut bus = TestBus::with_memory(memory);

    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();

    assert_eq!(
        bus.last_read_io_core_clocks_so_far,
        Some(0),
        "an IN that is the run's first (and, in the Accurate class, only \
             possible) instruction position sees core_clocks_so_far == 0"
    );
    assert!(
        outcome.core_clocks > 0,
        "the IN itself still charges clocks"
    );
}

#[test]
fn out_and_outs_forward_the_live_run_offset_to_the_bus() {
    let (mut out_cpu, out_memory) = real_mode_cpu(&[0xee], 32);
    out_cpu.write_gpr16(2, 0x300);
    let mut out_bus = TestBus::with_memory(out_memory);
    let out = out_cpu.decode(&mut out_bus).unwrap();
    out_cpu.core_clocks_so_far = 123;
    out_cpu.execute_decoded(&out, &mut out_bus).unwrap();
    assert_eq!(out_bus.last_write_io_core_clocks_so_far, Some(123));

    let (mut outs_cpu, mut outs_memory) = real_mode_cpu(&[0x6e], 32);
    outs_memory[0x10] = 0x5a;
    outs_cpu.write_gpr16(2, 0x300);
    outs_cpu.write_gpr16(6, 0x10);
    let mut outs_bus = TestBus::with_memory(outs_memory);
    let outs = outs_cpu.decode(&mut outs_bus).unwrap();
    outs_cpu.core_clocks_so_far = 456;
    outs_cpu.execute_decoded(&outs, &mut outs_bus).unwrap();
    assert_eq!(outs_bus.last_write_io_core_clocks_so_far, Some(456));
}

#[test]
fn core_clocks_so_far_tracks_the_running_total_for_an_in_reached_as_an_approximate_class_continuation()
 {
    // In the Approximate class (I486/I586), `block_continuable`
    // admits the IN forms (0xe4/0xe5/0xec/0xed), so an IN reached as a
    // continuation (not the run's first instruction) must see
    // core_clocks_so_far equal to the running total of every prior
    // instruction in the run, exactly like the Group/DataMove continuation
    // case pinned in `core_clocks_so_far_tracks_run_straight_lines_total_before_each_continuation`.
    // Eight INCs then an IN: the IN's core_clocks_so_far must equal the eight
    // INCs' combined charge. Eight (not two, unlike the sibling Accurate-class
    // test) because I586's `level_timing` factor is (1, 12) -- a single cheap
    // INC can legitimately round to 0 charged clocks under the fractional
    // remainder carry (see `scale_clocks`'s doc comment), so a short run risks
    // a degenerate all-zero total that cannot distinguish "tracks the running
    // total" from "always reads 0". Eight instructions guarantees the carry
    // has produced a nonzero total well before the IN.
    let code = [0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0xec]; // inc ax x8; in al,dx
    let (mut cpu, memory) = real_mode_cpu(&code, 32);
    // real_mode_cpu's default level (CpuGsw::default()) is already I586
    // (Approximate); set it explicitly so this test does not silently change
    // meaning if the default ever moves.
    cpu.set_mode(GswMode::Gsw586);
    let mut bus = TestBus::with_memory(memory);
    // Warm the decode cache one instruction at a time via single-step `cycle`
    // (not `run_straight_line`): once the IN is continuable, a warm-up call
    // to `run_straight_line` may itself chain multiple instructions per call,
    // so the number of `run_straight_line` calls needed to warm exactly 9
    // addresses is no longer deterministic. `cycle` always decodes and
    // advances exactly one instruction per call, so 9 calls warms exactly
    // addresses 0..9 regardless of continuability.
    for _ in 0..9 {
        let _ = cpu.cycle(&mut bus).unwrap();
    }
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0);
    cpu.reset_perf_counters();
    // The warm-up's IN (address 8) set TestBus::io_touched, which never
    // self-clears (unlike the real machine batch loop, which opens each batch
    // with a fresh false). Clear it here so the measurement run below is not
    // ended by stale warm-up state on its very first instruction.
    bus.io_touched = false;

    // Independently capture "the eight INCs' combined charge" the same way
    // the sibling Group-continuation test does: clone the warmed-up CPU (so
    // its `timing_rem` fractional-clock carry matches) and single-step eight
    // INCs on a clone bus.
    let eight_incs_total = {
        let mut solo = cpu.clone();
        let mut solo_bus = TestBus::with_memory(vec![0x40; 8]);
        let mut total = 0u32;
        for _ in 0..8 {
            total += solo.cycle(&mut solo_bus).unwrap().core_clocks;
        }
        total
    };
    assert!(
        eight_incs_total > 0,
        "sanity: eight INCs must have produced a nonzero charge under the \
             remainder carry, or this test cannot distinguish the running total \
             from a degenerate always-0 read"
    );

    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();

    assert_eq!(
        cpu.perf_counters().straight_line_runs,
        1,
        "one chained run: eight INCs then the IN, all continuable in the \
             Approximate class"
    );
    assert_eq!(
        bus.last_read_io_core_clocks_so_far,
        Some(eight_incs_total.into()),
        "the IN reached as the run's ninth instruction (a continuation) must \
             see core_clocks_so_far equal to the eight INCs' combined charge, not 0"
    );
    assert!(
        outcome.core_clocks > eight_incs_total,
        "the IN's own charge must be included in the run total"
    );
}

#[test]
fn poll_loop_with_test_imm_chains_end_to_end_in_the_approximate_class() {
    // The canonical vretrace poll idiom: IN; TEST AL,imm8; JZ back; (JMP back,
    // unreachable here since AL reads 0 so ZF is always set). With 0xa8
    // admitted alongside the IN forms in the Approximate class, the WHOLE
    // loop must chain as one run_straight_line call up to the clock cap --
    // no run restart per iteration. The bus models the machine's lazy
    // status-port path (lazy_io_reads: reads do not set io_touched), since
    // chaining across the IN is only reachable when the port read is lazy.
    let code = [
        0xEC, // 0: in al, dx (TestBus returns 0 -> AL = 0)
        0xA8, 0x08, // 1: test al, 0x08 (AL=0 -> ZF set)
        0x74, 0xFB, // 3: jz -5 -> back to 0 (always taken)
        0xEB, 0xF9, // 5: jmp -7 -> back to 0 (unreachable, decode fodder only)
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 32);
    cpu.set_mode(GswMode::Gsw586);
    let mut bus = TestBus::with_memory(memory);
    bus.lazy_io_reads = true;
    // Warm the decode cache: one single-step per loop instruction (IN, TEST,
    // JZ -- the JMP is unreachable and irrelevant to the chain).
    for _ in 0..3 {
        let _ = cpu.cycle(&mut bus).unwrap();
    }
    assert_eq!(cpu.registers.eip, 0, "warm-up looped back to the IN");
    cpu.reset_perf_counters();

    // A finite cap: the loop never exits on its own, so the ONLY clean end
    // for a fully-chained run is the cap. Big enough for many iterations.
    let outcome = cpu.run_straight_line(&mut bus, 1_000).unwrap();

    let p = cpu.perf_counters();
    assert_eq!(
        p.straight_line_runs, 1,
        "the whole poll loop must chain inside ONE run_straight_line call"
    );
    assert_eq!(
        p.brk_cap, 1,
        "the run must end on the clock cap, not on a step break or a \
             non-continuable terminator (brk_step={}, brk_branch={})",
        p.brk_step, p.brk_decode_or_branch
    );
    assert!(
        p.instructions > 100,
        "hundreds of poll iterations must fit under the cap once the loop \
             chains (saw {} instructions)",
        p.instructions
    );
    assert!(
        bus.last_read_io_core_clocks_so_far.unwrap() > 0,
        "a late-iteration IN reached as a continuation must see the running \
             (nonzero) core-clock total, proving the INs chained mid-run"
    );
    assert!(
        u64::from(outcome.core_clocks) >= 1_000,
        "the chained run must have consumed the whole cap"
    );
}

#[test]
fn poll_loop_test_imm_still_terminates_the_run_in_the_accurate_class() {
    // The complementary Accurate-class pin: at I386 neither the IN (0xec)
    // nor the TEST (0xa8) is continuable, so even with the bus's lazy-read
    // knob on (no io_touched step break at all), the same poll loop must
    // stop at the first continuation attempt: the run is exactly the one IN,
    // ended by TEST's non-admission. This is the byte-identical run-shape
    // guarantee for 286/386.
    let code = [
        0xEC, // 0: in al, dx
        0xA8, 0x08, // 1: test al, 0x08
        0x74, 0xFB, // 3: jz -5 -> back to 0
        0xEB, 0xF9, // 5: jmp -7 -> back to 0
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 32);
    cpu.set_mode(GswMode::Gsw386);
    let mut bus = TestBus::with_memory(memory);
    bus.lazy_io_reads = true;
    for _ in 0..3 {
        let _ = cpu.cycle(&mut bus).unwrap();
    }
    assert_eq!(cpu.registers.eip, 0, "warm-up looped back to the IN");
    cpu.reset_perf_counters();

    let _ = cpu.run_straight_line(&mut bus, 1_000).unwrap();

    let p = cpu.perf_counters();
    assert_eq!(
        p.straight_line_runs, 1,
        "one run_straight_line call was made"
    );
    assert_eq!(
        p.instructions, 1,
        "the Accurate class must retire exactly the IN and stop at the \
             non-continuable TEST (no io_touched break was available to end it, \
             so this pins the admission gate itself)"
    );
    assert_eq!(
        p.brk_decode_or_branch, 1,
        "the run must end on the continuation-admission check, not a step \
             break (brk_step={})",
        p.brk_step
    );
}

#[test]
fn in_stays_a_run_terminator_not_a_continuation_in_the_accurate_class() {
    // Pins the IN half of the Approximate-class admission gate, which the
    // sibling poll-loop test cannot: there the run ends at the TEST before
    // any continuation attempt ever reaches an IN, so deleting the level
    // gate from the PortIo arm alone would not fail it (the spec review
    // proved the earlier Accurate-class test -- a single IN at eip 0,
    // trivially the run's first instruction -- pinned nothing). Here two
    // continuable INCs precede the IN: at I386 the run must chain the INCs
    // and stop at the continuation-admission check BEFORE the IN executes,
    // observable as read_io never having been called during the run. The
    // bus's lazy-read knob is on, so no io_touched step break could end the
    // run in the gate's place. Mutation-verified: with the level gate
    // removed from the PortIo arm the IN chains and read_io fires, failing
    // the None assertion; with the gate intact it passes.
    let code = [0x40, 0x40, 0xec]; // inc ax; inc ax; in al,dx
    let (mut cpu, memory) = real_mode_cpu(&code, 32);
    cpu.set_mode(GswMode::Gsw386);
    let mut bus = TestBus::with_memory(memory);
    bus.lazy_io_reads = true;
    // Warm all three decode-cache lines via single-steps (the IN included,
    // so its cached `continuable` flag is what gates the measured run).
    for _ in 0..3 {
        let _ = cpu.cycle(&mut bus).unwrap();
    }
    cpu.registers.eip = 0;
    cpu.reset_perf_counters();
    // The warm-up executed the IN once; clear its trace so the assertion
    // below observes only the measured run.
    bus.last_read_io_core_clocks_so_far = None;

    let _ = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();

    let p = cpu.perf_counters();
    assert_eq!(
        p.instructions, 2,
        "the Accurate class must retire exactly the two INCs and stop at \
             the non-continuable IN"
    );
    assert_eq!(
        bus.last_read_io_core_clocks_so_far, None,
        "read_io must NOT have been called: the run stopped BEFORE the IN, \
             at the continuation-admission check"
    );
    assert_eq!(
        p.brk_decode_or_branch, 1,
        "the run must end on the continuation-admission check, not a step \
             break (brk_step={})",
        p.brk_step
    );
}

#[test]
fn core_clocks_so_far_tracks_run_straight_lines_total_before_each_continuation() {
    // Directly checks the CpuGsw field set to
    // run_straight_line's running `total` before every continuation dispatch,
    // read by read_io) using a continuable instruction group (INC, DataMove/
    // Alu-adjacent -- specifically Group) as the observation point, since
    // PortIo itself cannot reach the continuation path (see the sibling test).
    // Two INCs then a third INC: after the run, core_clocks_so_far must equal
    // whatever `total` was immediately before the LAST instruction executed
    // (i.e. the first two INCs' combined charge), proving the field tracks
    // the running total across continuations, not just "always 0" by
    // accident of PortIo's continuability gate.
    let code = [0x40, 0x40, 0x40]; // inc ax; inc ax; inc ax
    let (mut cpu, memory) = real_mode_cpu(&code, 32);
    cpu.set_mode(GswMode::Gsw386);
    let mut bus = TestBus::with_memory(memory);
    // Warm the decode cache one instruction at a time (INC is continuable, so
    // once warm all three chain in a single run_straight_line call).
    for _ in 0..3 {
        let _ = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    }
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0);
    cpu.reset_perf_counters();

    // Independently capture "the first two INCs' combined charge" by cloning
    // the CPU right here (so its warmed-up `timing_rem` fractional-clock
    // carry, accumulated over the 3 warm-up runs, matches exactly) and
    // driving the clone through two `cycle()` single-steps.
    let (two_incs_total, three_incs_total) = {
        let mut solo = cpu.clone();
        let mut solo_bus = TestBus::with_memory(vec![0x40, 0x40, 0x40]);
        let a = solo.cycle(&mut solo_bus).unwrap().core_clocks;
        let b = solo.cycle(&mut solo_bus).unwrap().core_clocks;
        let c = solo.cycle(&mut solo_bus).unwrap().core_clocks;
        (a + b, a + b + c)
    };

    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();

    assert_eq!(
        cpu.perf_counters().straight_line_runs,
        1,
        "one chained run: three INCs, no port access to break it early"
    );
    assert_eq!(cpu.read_reg16(Reg16::Ax), 3, "all three INCs retired");
    // core_clocks_so_far was set to `total` right before the run's LAST
    // continuation (the third INC) dispatched, so it must equal exactly the
    // first two INCs' combined charge, independently measured above.
    assert_eq!(cpu.core_clocks_so_far, u64::from(two_incs_total));
    assert_eq!(outcome.core_clocks, three_incs_total);
}

#[test]
fn register_aliasing_updates_low_parts() {
    let mut cpu = CpuGsw::default();
    cpu.registers.set_eax(0x1234_5678);

    cpu.write_reg16(Reg16::Ax, 0xabcd);
    cpu.write_gpr8(4, 0xef);

    assert_eq!(cpu.registers.eax(), 0x1234_efcd);
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0xefcd);
}

#[test]
fn operand_prefix_allows_32bit_mov_in_real_mode() {
    let mut memory = vec![0; 32];
    memory[0..6].copy_from_slice(&[0x66, 0xb8, 0x78, 0x56, 0x34, 0x12]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eax(), 0x1234_5678);
    assert_eq!(cpu.registers.eip, 6);
}

#[test]
fn modrm_direct_address_can_store_ax() {
    let mut memory = vec![0; 1024];
    memory[0..5].copy_from_slice(&[0x89, 0x06, 0x00, 0x02, 0xf4]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x4f56);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(
        u16::from_le_bytes([bus.memory[0x200], bus.memory[0x201]]),
        0x4f56
    );
}

#[test]
fn perf_counters_track_decode_hits_and_run_breaks() {
    // A tight loop: 0: inc ax (40); 1: inc ax (40); 2: jmp $-4 (EB FC) -> 0.
    let mut memory = vec![0u8; 1024];
    memory[0..4].copy_from_slice(&[0x40, 0x40, 0xeb, 0xfc]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);

    // Six single steps run the 3-instruction body twice (inc, inc, jmp).
    for _ in 0..6 {
        cpu.cycle(&mut bus).unwrap();
    }
    let p = cpu.perf_counters();
    assert_eq!(p.instructions, 6, "six instructions retired");
    // The three unique linear addresses decode once; the loop's second pass is
    // served from the decode cache, so misses stay at 3 (a 50% hit rate). This is
    // the assertion that fails if the decode cache (or the miss counter) breaks.
    assert_eq!(
        p.decode_misses, 3,
        "only the first pass decodes; the loop re-hits"
    );

    // On the now-warm cache a straight-line run executes the two cached `inc`s and the cached
    // backward JMP repeatedly until the batch cap fires.
    cpu.reset_perf_counters();
    assert_eq!(
        cpu.perf_counters().instructions,
        0,
        "reset zeroes the counters"
    );
    let _ = cpu.run_straight_line(&mut bus, 10_000).unwrap();
    let p = cpu.perf_counters();
    assert_eq!(p.straight_line_runs, 1, "one run");
    assert!(
        p.instructions >= 1,
        "the run retired at least the first instruction"
    );
    assert_eq!(p.brk_decode_or_branch, 0, "the cached JMP stayed in-run");
    assert_eq!(p.brk_cap, 1, "the run ended at the clock cap");
    assert_eq!(
        p.brk_step + p.brk_interrupt + p.brk_halt,
        0,
        "no other break reason fired"
    );
}

// Standalone setup for `seam_counters_bound_probes_and_are_deterministic`, mirroring the tight
// loop and warm-cache-then-run_straight_line shape of `perf_counters_track_decode_hits_and_run_breaks`
// above. Not extracted FROM that test's body: the sibling test interleaves counter assertions
// between its two phases against one long-lived `cpu`/`bus` pair, so factoring out a shared
// helper that both tests call would either change the sibling's structure or force this helper
// to hand back the `cpu`/`bus` (defeating the point of a return-a-perf-snapshot helper). This
// function reproduces the same code bytes, same warm-up count, and same run_straight_line cap,
// so both tests exercise the identical continuation-loop shape.
fn run_fixture_and_return_perf() -> PerfCounters {
    // A tight loop: 0: inc ax (40); 1: inc ax (40); 2: jmp $-4 (EB FC) -> 0.
    let mut memory = vec![0u8; 1024];
    memory[0..4].copy_from_slice(&[0x40, 0x40, 0xeb, 0xfc]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);

    // Two passes through the 3-instruction body warm the decode cache, exactly like the sibling
    // test, so the straight-line run below hits cache on every continuation.
    for _ in 0..6 {
        cpu.cycle(&mut bus).unwrap();
    }
    cpu.reset_perf_counters();
    let _ = cpu.run_straight_line(&mut bus, 10_000).unwrap();
    cpu.perf_counters().clone()
}

#[test]
fn seam_counters_bound_probes_and_are_deterministic() {
    // Mirror the setup of perf_counters_track_decode_hits_and_run_breaks: same
    // harness, same straight-line program shape (several continuable instructions
    // ending in a run break). Run the program twice on two identically
    // constructed CPUs.
    //
    // Invariants (provable without pinning the exact probe count):
    //  1. Every continuation is preceded by exactly one decode-cache probe, so
    //     probes >= instructions - straight_line_runs (the first instruction of
    //     each run is not a continuation) and probes <= instructions +
    //     straight_line_runs (at most one extra break-detecting probe per run).
    //  2. The counter is deterministic: both CPUs report the same value.
    //  3. Declines never exceed probes.
    let p1 = run_fixture_and_return_perf();
    let p2 = run_fixture_and_return_perf();
    let floor = p1.instructions - p1.straight_line_runs;
    let ceiling = p1.instructions + p1.straight_line_runs;
    assert!(
        p1.decode_probes >= floor,
        "probes {} < floor {}",
        p1.decode_probes,
        floor
    );
    assert!(
        p1.decode_probes <= ceiling,
        "probes {} > ceiling {}",
        p1.decode_probes,
        ceiling
    );
    assert!(p1.decode_probes > 0);
    assert!(p1.jit_direct_dispatch_declines <= p1.decode_probes);
    assert_eq!(p1.decode_probes, p2.decode_probes);
    assert_eq!(
        p1.jit_direct_dispatch_declines,
        p2.jit_direct_dispatch_declines
    );
}

fn profile_test_cpu(code: &[u8]) -> (CpuGsw, TestBus) {
    let mut memory = vec![0u8; 1024];
    memory[..code.len()].copy_from_slice(code);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    (cpu, TestBus::with_memory(memory))
}

fn profile_bucket<'a>(snapshot: &'a CpuProfileSnapshot, name: &str) -> &'a CpuProfileBucket {
    snapshot
        .groups
        .iter()
        .find(|bucket| bucket.name == name)
        .expect("profile bucket exists")
}

fn profile_opcode(snapshot: &CpuProfileSnapshot, opcode: u16) -> &CpuOpcodeProfileBucket {
    snapshot
        .opcodes
        .iter()
        .find(|bucket| bucket.opcode == opcode)
        .expect("profile opcode bucket exists")
}

#[test]
fn cpu_profile_splits_x87_escape_forms_by_modrm() {
    let (mut cpu, mut bus) = profile_test_cpu(&[0xd9, 0xc0, 0xd9, 0xc8]);
    cpu.enable_profiling(1);

    cpu.cycle_no_interrupt_check(&mut bus).unwrap();
    cpu.cycle_no_interrupt_check(&mut bus).unwrap();

    let snapshot = cpu.profile_snapshot();
    let fld = cpu.decode_cache.get(0, false).unwrap();
    let fxch = cpu.decode_cache.get(2, false).unwrap();
    assert_eq!(
        profile_opcode(&snapshot, cpu_profile_opcode(&fld)).instructions,
        1
    );
    assert_eq!(
        profile_opcode(&snapshot, cpu_profile_opcode(&fxch)).instructions,
        1
    );
}

#[test]
fn cpu_profile_disabled_records_no_groups() {
    let (mut cpu, mut bus) = profile_test_cpu(&[0x40]); // inc ax

    cpu.cycle_no_interrupt_check(&mut bus).unwrap();

    let snapshot = cpu.profile_snapshot();
    assert!(
        snapshot.groups.iter().all(|bucket| bucket.instructions == 0
            && bucket.guest_core_clocks == 0
            && bucket.samples == 0
            && bucket.sample_wall_ns == 0),
        "profiling must be inert until explicitly enabled"
    );
    assert!(
        snapshot.opcodes.is_empty(),
        "opcode profiling must be inert until explicitly enabled"
    );
}

#[test]
fn cpu_profile_records_decode_groups() {
    let code = [
        0x05, 0x01, 0x00, // add ax,1        (alu)
        0x8b, 0xc0, // mov ax,ax       (data_move)
        0xd9, 0xe8, // fld1            (fpu)
    ];
    let (mut cpu, mut bus) = profile_test_cpu(&code);
    cpu.enable_profiling(1);

    for _ in 0..3 {
        cpu.cycle_no_interrupt_check(&mut bus).unwrap();
    }

    let snapshot = cpu.profile_snapshot();
    for name in ["alu", "data_move", "fpu"] {
        let bucket = profile_bucket(&snapshot, name);
        assert_eq!(bucket.instructions, 1, "{name} instruction count");
        assert_eq!(bucket.samples, 1, "{name} sampled every instruction");
    }
    for opcode in [0x05, 0x8b] {
        let bucket = profile_opcode(&snapshot, opcode);
        assert_eq!(bucket.instructions, 1, "opcode {opcode:#x} count");
        assert_eq!(bucket.samples, 1, "opcode {opcode:#x} samples");
    }
    let fld1 = cpu.decode_cache.get(5, false).unwrap();
    let opcode = cpu_profile_opcode(&fld1);
    let bucket = profile_opcode(&snapshot, opcode);
    assert_eq!(bucket.instructions, 1, "opcode {opcode:#x} count");
    assert_eq!(bucket.samples, 1, "opcode {opcode:#x} samples");
}

#[test]
fn cpu_profile_sample_stride_is_deterministic() {
    let (mut cpu, mut bus) = profile_test_cpu(&[0x40, 0x40, 0x40, 0x40]); // inc ax x4
    cpu.enable_profiling(2);

    for _ in 0..4 {
        cpu.cycle_no_interrupt_check(&mut bus).unwrap();
    }

    let snapshot = cpu.profile_snapshot();
    let bucket = profile_bucket(&snapshot, "flags_misc");
    assert_eq!(bucket.instructions, 4);
    assert_eq!(bucket.samples, 2);
    let opcode = profile_opcode(&snapshot, 0x40);
    assert_eq!(opcode.instructions, 4);
    assert_eq!(opcode.samples, 2);
}

#[test]
fn cpu_profile_opcode_counts_register_and_memory_forms() {
    let code = [
        0x8b, 0xc0, // mov ax, ax
        0x8b, 0x06, 0x20, 0x00, // mov ax, [0x0020]
    ];
    let (mut cpu, mut bus) = profile_test_cpu(&code);
    cpu.enable_profiling(1);

    for _ in 0..2 {
        cpu.cycle_no_interrupt_check(&mut bus).unwrap();
    }

    let snapshot = cpu.profile_snapshot();
    let opcode = profile_opcode(&snapshot, 0x8b);
    assert_eq!(opcode.instructions, 2);
    assert_eq!(opcode.samples, 2);
    assert_eq!(opcode.register_instructions, 1);
    assert_eq!(opcode.memory_instructions, 1);
    assert_eq!(opcode.register_samples, 1);
    assert_eq!(opcode.memory_samples, 1);
}

#[test]
fn moffs_loads_al_from_direct_offset() {
    // mov al, [0x0200] (0xa0 0x00 0x02). Byte form ignores the operand-size
    // prefix and touches only AL. It must not disturb flags.
    let mut memory = vec![0; 1024];
    memory[0..3].copy_from_slice(&[0xa0, 0x00, 0x02]);
    memory[0x200] = 0x7e;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x11ff);
    let flags_before = cpu.registers.eflags;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    // AL replaced, AH preserved, instruction is three bytes long.
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x117e);
    assert_eq!(cpu.registers.eip, 3);
    assert_eq!(cpu.registers.eflags, flags_before);
}

#[test]
fn moffs_stores_al_to_direct_offset() {
    // mov [0x0200], al (0xa2 0x00 0x02). Byte form writes only one byte and
    // leaves flags alone.
    let mut memory = vec![0; 1024];
    memory[0..3].copy_from_slice(&[0xa2, 0x00, 0x02]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x22a5);
    let flags_before = cpu.registers.eflags;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(bus.memory[0x200], 0xa5);
    // The neighbouring byte is untouched by a byte store.
    assert_eq!(bus.memory[0x201], 0x00);
    assert_eq!(cpu.registers.eip, 3);
    assert_eq!(cpu.registers.eflags, flags_before);
}

#[test]
fn page_translation_reads_identity_mapped_memory() {
    let mut memory = vec![0; 0x4000];
    memory[0x1000..0x1004].copy_from_slice(&0x0000_2003u32.to_le_bytes());
    memory[0x2000..0x2004].copy_from_slice(&0x0000_3003u32.to_le_bytes());
    memory[0x3000] = 0x90;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.control.cr3 = 0x1000;
    cpu.control.cr0 |= CR0_PG;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 1);
}

const CROSS_PAGE_LINEAR: u32 = 0x8fff;
const CROSS_PAGE_PDE: usize = 0x1000;
const CROSS_PAGE_PTE8: usize = 0x2020;
const CROSS_PAGE_PTE9: usize = 0x2024;
const CROSS_PAGE_FIRST_FRAME: usize = 0x5000;
const CROSS_PAGE_SECOND_FRAME: usize = 0x7000;

fn cross_page_paging_cpu(user: bool, wp: bool) -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    if wp {
        cpu.control.cr0 |= CR0_WP;
    }
    cpu.control.cr3 = 0x1000;
    cpu.cpl = if user { 3 } else { 0 };
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister::flat(
            if user { 0x0b } else { 0x08 },
            if user { 0xfb } else { 0x9b },
        ),
    );
    cpu.registers.set_segment(
        SegmentIndex::Ds,
        SegmentRegister::flat(
            if user { 0x13 } else { 0x10 },
            if user { 0xf3 } else { 0x93 },
        ),
    );
    cpu
}

fn cross_page_paging_bus(first_pte: u32, second_pte: u32) -> TestBus {
    let mut memory = vec![0; 0x8000];
    memory[CROSS_PAGE_PDE..CROSS_PAGE_PDE + 4].copy_from_slice(&0x2007u32.to_le_bytes());
    memory[CROSS_PAGE_PTE8..CROSS_PAGE_PTE8 + 4].copy_from_slice(&first_pte.to_le_bytes());
    memory[CROSS_PAGE_PTE9..CROSS_PAGE_PTE9 + 4].copy_from_slice(&second_pte.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus
}

fn paging_entry(memory: &[u8], address: usize) -> u32 {
    u32::from_le_bytes(memory[address..address + 4].try_into().unwrap())
}

fn data_cycle_shape(bus: &TestBus, kind: BusAccessKind) -> Vec<(u32, BusWidth, u32)> {
    bus.trace
        .cycles()
        .iter()
        .filter(|cycle| cycle.kind == kind)
        .map(|cycle| (cycle.address, cycle.width, cycle.clocks))
        .collect()
}

#[test]
fn paged_cross_page_dword_uses_both_noncontiguous_frames_and_sets_ad_bits() {
    let mut cpu = cross_page_paging_cpu(false, true);
    let mut bus = cross_page_paging_bus(0x5007, 0x7007);
    bus.memory[CROSS_PAGE_FIRST_FRAME + 0x0fff] = 0x11;
    bus.memory[CROSS_PAGE_SECOND_FRAME..CROSS_PAGE_SECOND_FRAME + 3]
        .copy_from_slice(&[0x22, 0x33, 0x44]);
    bus.memory[0x6000..0x6003].copy_from_slice(&[0xaa, 0xbb, 0xcc]);

    let value = cpu
        .read_memory_sized(
            &mut bus,
            SegmentIndex::Ds,
            CROSS_PAGE_LINEAR,
            OperandSize::Dword,
            BusAccessKind::DataRead,
        )
        .unwrap();

    assert_eq!(value, 0x4433_2211);
    assert_eq!(paging_entry(&bus.memory, CROSS_PAGE_PDE) & 0x20, 0x20);
    assert_eq!(paging_entry(&bus.memory, CROSS_PAGE_PTE8) & 0x60, 0x20);
    assert_eq!(paging_entry(&bus.memory, CROSS_PAGE_PTE9) & 0x60, 0x20);

    cpu.write_memory_sized(
        &mut bus,
        SegmentIndex::Ds,
        CROSS_PAGE_LINEAR,
        OperandSize::Dword,
        0xa1b2_c3d4,
        BusAccessKind::DataWrite,
    )
    .unwrap();

    assert_eq!(bus.memory[CROSS_PAGE_FIRST_FRAME + 0x0fff], 0xd4);
    assert_eq!(
        &bus.memory[CROSS_PAGE_SECOND_FRAME..CROSS_PAGE_SECOND_FRAME + 3],
        &[0xc3, 0xb2, 0xa1]
    );
    assert_eq!(&bus.memory[0x6000..0x6003], &[0xaa, 0xbb, 0xcc]);
    assert_eq!(paging_entry(&bus.memory, CROSS_PAGE_PTE8) & 0x60, 0x60);
    assert_eq!(paging_entry(&bus.memory, CROSS_PAGE_PTE9) & 0x60, 0x60);
}

#[test]
fn paged_cross_page_dword_keeps_aligned_bus_fragment_widths() {
    let mut word_cpu = cross_page_paging_cpu(false, true);
    let mut word_bus = cross_page_paging_bus(0x5007, 0x7007);
    word_bus.direct_page_clocks = true;
    word_bus.memory[CROSS_PAGE_FIRST_FRAME + 0x0fff] = 1;
    word_bus.memory[CROSS_PAGE_SECOND_FRAME] = 2;
    assert_eq!(
        word_cpu
            .read_memory_sized(
                &mut word_bus,
                SegmentIndex::Ds,
                CROSS_PAGE_LINEAR,
                OperandSize::Word,
                BusAccessKind::DataRead,
            )
            .unwrap(),
        0x0201
    );
    assert_eq!(
        data_cycle_shape(&word_bus, BusAccessKind::DataRead),
        vec![(0x5fff, BusWidth::Byte, 2), (0x7000, BusWidth::Byte, 2)]
    );

    let mut fffe_cpu = cross_page_paging_cpu(false, true);
    let mut fffe_bus = cross_page_paging_bus(0x5007, 0x7007);
    fffe_bus.direct_page_clocks = true;
    fffe_bus.memory[CROSS_PAGE_FIRST_FRAME + 0x0ffe..CROSS_PAGE_FIRST_FRAME + 0x1000]
        .copy_from_slice(&[1, 2]);
    fffe_bus.memory[CROSS_PAGE_SECOND_FRAME..CROSS_PAGE_SECOND_FRAME + 2].copy_from_slice(&[3, 4]);
    assert_eq!(
        fffe_cpu
            .read_memory_sized(
                &mut fffe_bus,
                SegmentIndex::Ds,
                0x8ffe,
                OperandSize::Dword,
                BusAccessKind::DataRead,
            )
            .unwrap(),
        0x0403_0201
    );
    assert_eq!(
        data_cycle_shape(&fffe_bus, BusAccessKind::DataRead),
        vec![(0x5ffe, BusWidth::Word, 3), (0x7000, BusWidth::Word, 3)]
    );

    let mut ffff_cpu = cross_page_paging_cpu(false, true);
    let mut ffff_bus = cross_page_paging_bus(0x5007, 0x7007);
    ffff_bus.direct_page_clocks = true;
    ffff_bus.memory[CROSS_PAGE_FIRST_FRAME + 0x0fff] = 1;
    ffff_bus.memory[CROSS_PAGE_SECOND_FRAME..CROSS_PAGE_SECOND_FRAME + 3]
        .copy_from_slice(&[2, 3, 4]);
    assert_eq!(
        ffff_cpu
            .read_memory_sized(
                &mut ffff_bus,
                SegmentIndex::Ds,
                CROSS_PAGE_LINEAR,
                OperandSize::Dword,
                BusAccessKind::DataRead,
            )
            .unwrap(),
        0x0403_0201
    );
    assert_eq!(
        data_cycle_shape(&ffff_bus, BusAccessKind::DataRead),
        vec![
            (0x5fff, BusWidth::Byte, 2),
            (0x7000, BusWidth::Word, 3),
            (0x7002, BusWidth::Byte, 2)
        ]
    );
    ffff_cpu
        .write_memory_sized(
            &mut ffff_bus,
            SegmentIndex::Ds,
            CROSS_PAGE_LINEAR,
            OperandSize::Dword,
            0xaabb_ccdd,
            BusAccessKind::DataWrite,
        )
        .unwrap();
    assert_eq!(
        data_cycle_shape(&ffff_bus, BusAccessKind::DataWrite),
        vec![
            (0x5fff, BusWidth::Byte, 2),
            (0x7000, BusWidth::Word, 3),
            (0x7002, BusWidth::Byte, 2)
        ]
    );
}

#[test]
fn paged_cross_page_missing_second_page_faults_at_its_first_byte() {
    let mut read_cpu = cross_page_paging_cpu(false, true);
    let mut read_bus = cross_page_paging_bus(0x5007, 0);
    read_bus.memory[CROSS_PAGE_FIRST_FRAME + 0x0fff] = 0x5a;
    read_cpu.registers.set_eax(0xdead_beef);
    let registers = read_cpu.registers.clone();

    let read_fault = read_cpu.read_memory_sized(
        &mut read_bus,
        SegmentIndex::Ds,
        CROSS_PAGE_LINEAR,
        OperandSize::Dword,
        BusAccessKind::DataRead,
    );

    assert!(matches!(
        read_fault,
        Err(InternalFault::Exception {
            vector: 14,
            error_code: Some(0)
        })
    ));
    assert_eq!(read_cpu.control.cr2, 0x9000);
    assert_eq!(read_cpu.registers, registers);
    assert_eq!(paging_entry(&read_bus.memory, CROSS_PAGE_PTE8) & 0x60, 0x20);
    assert_eq!(paging_entry(&read_bus.memory, CROSS_PAGE_PTE9), 0);

    let mut write_cpu = cross_page_paging_cpu(false, true);
    let mut write_bus = cross_page_paging_bus(0x5007, 0);
    write_bus.memory[CROSS_PAGE_FIRST_FRAME + 0x0fff] = 0xaa;
    let write_fault = write_cpu.write_memory_sized(
        &mut write_bus,
        SegmentIndex::Ds,
        CROSS_PAGE_LINEAR,
        OperandSize::Dword,
        0x4433_2211,
        BusAccessKind::DataWrite,
    );

    assert!(matches!(
        write_fault,
        Err(InternalFault::Exception {
            vector: 14,
            error_code: Some(2)
        })
    ));
    assert_eq!(write_cpu.control.cr2, 0x9000);
    assert_eq!(write_bus.memory[CROSS_PAGE_FIRST_FRAME + 0x0fff], 0x11);
    assert_eq!(
        paging_entry(&write_bus.memory, CROSS_PAGE_PTE8) & 0x60,
        0x60
    );
    assert_eq!(paging_entry(&write_bus.memory, CROSS_PAGE_PTE9), 0);
}

#[test]
fn paged_cross_page_second_page_enforces_user_and_wp_permissions() {
    let mut user_cpu = cross_page_paging_cpu(true, false);
    let mut user_bus = cross_page_paging_bus(0x5007, 0x7005);
    user_bus.memory[CROSS_PAGE_FIRST_FRAME + 0x0fff] = 0xaa;
    let user_fault = user_cpu.write_memory_sized(
        &mut user_bus,
        SegmentIndex::Ds,
        CROSS_PAGE_LINEAR,
        OperandSize::Dword,
        0x4433_2211,
        BusAccessKind::DataWrite,
    );
    assert!(matches!(
        user_fault,
        Err(InternalFault::Exception {
            vector: 14,
            error_code: Some(7)
        })
    ));
    assert_eq!(user_cpu.control.cr2, 0x9000);
    assert_eq!(user_bus.memory[CROSS_PAGE_FIRST_FRAME + 0x0fff], 0x11);
    assert_eq!(paging_entry(&user_bus.memory, CROSS_PAGE_PTE8) & 0x60, 0x60);
    assert_eq!(paging_entry(&user_bus.memory, CROSS_PAGE_PTE9) & 0x60, 0);

    let mut wp_cpu = cross_page_paging_cpu(false, true);
    let mut wp_bus = cross_page_paging_bus(0x5003, 0x7001);
    let wp_fault = wp_cpu.write_memory_sized(
        &mut wp_bus,
        SegmentIndex::Ds,
        CROSS_PAGE_LINEAR,
        OperandSize::Dword,
        0x8877_6655,
        BusAccessKind::DataWrite,
    );
    assert!(matches!(
        wp_fault,
        Err(InternalFault::Exception {
            vector: 14,
            error_code: Some(3)
        })
    ));
    assert_eq!(wp_cpu.control.cr2, 0x9000);
    assert_eq!(wp_bus.memory[CROSS_PAGE_FIRST_FRAME + 0x0fff], 0x55);
    assert_eq!(paging_entry(&wp_bus.memory, CROSS_PAGE_PTE9) & 0x60, 0);

    let mut no_wp_cpu = cross_page_paging_cpu(false, false);
    let mut no_wp_bus = cross_page_paging_bus(0x5003, 0x7001);
    no_wp_cpu
        .write_memory_sized(
            &mut no_wp_bus,
            SegmentIndex::Ds,
            CROSS_PAGE_LINEAR,
            OperandSize::Dword,
            0xccbb_aa99,
            BusAccessKind::DataWrite,
        )
        .unwrap();
    assert_eq!(no_wp_bus.memory[CROSS_PAGE_FIRST_FRAME + 0x0fff], 0x99);
    assert_eq!(
        &no_wp_bus.memory[CROSS_PAGE_SECOND_FRAME..CROSS_PAGE_SECOND_FRAME + 3],
        &[0xaa, 0xbb, 0xcc]
    );
    assert_eq!(
        paging_entry(&no_wp_bus.memory, CROSS_PAGE_PTE9) & 0x60,
        0x60
    );
}

#[test]
fn user_mode_paging_respects_the_supervisor_bit() {
    // PD at 0x1000, PT at 0x2000. Linear 0x3000 maps to a present, writable,
    // supervisor (U/S=0) page at frame 0x5000.
    let mut memory = vec![0; 0x6000];
    memory[0x1000..0x1004].copy_from_slice(&0x0000_2007u32.to_le_bytes()); // PDE: PT, present+rw+user
    memory[0x200c..0x2010].copy_from_slice(&0x0000_5003u32.to_le_bytes()); // PTE[3]: frame, present+rw, U/S=0
    let mut cpu = CpuGsw::default();
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.control.cr3 = 0x1000;
    let flat_cs = |rpl| SegmentRegister {
        selector: rpl,
        base: 0,
        limit: 0xffff_ffff,
        access: 0x9b,
        default_size_32: false,
    };
    let mut bus = TestBus::with_memory(memory);

    // CPL 3: a user read of the supervisor page faults with #PF, error code
    // present|user (0b101 = 0x5), and cr2 set to the faulting linear address.
    cpu.registers.set_segment(SegmentIndex::Cs, flat_cs(0x0003));
    cpu.cpl = 3; // this test flips CS directly, so seed the cached CPL to match
    let faulted = cpu.translate_linear(&mut bus, 0x3000, false);
    assert!(
        matches!(
            faulted,
            Err(InternalFault::Exception {
                vector: 14,
                error_code: Some(0x5)
            })
        ),
        "{faulted:?}"
    );
    assert_eq!(cpu.control.cr2, 0x3000);

    // CPL 0: a 386 has no CR0.WP, so supervisor reaches the same page fine.
    cpu.registers.set_segment(SegmentIndex::Cs, flat_cs(0x0000));
    cpu.cpl = 0;
    assert_eq!(
        cpu.translate_linear(&mut bus, 0x3000, false).unwrap(),
        0x5000
    );
}

#[test]
fn v86_paging_is_always_user_regardless_of_cs_low_bits() {
    // 386 PRM 5-24 / 15-6: a V86 task always executes at CPL 3, so paging
    // privilege (PRM ch5's U/S check) must classify every V86 access as user --
    // independent of the V86 CS selector's low two bits, which are NOT an RPL (a
    // V86 CS is a real-mode-style segment, not a descriptor selector; see
    // `current_privilege_level`'s doc comment). A monitor that maps its own
    // pages supervisor-only (U/S=0) must be unreachable from V86 even when the
    // guest's CS happens to read a multiple of 4 (RPL bits 00).
    //
    // Same page tables as `user_mode_paging_respects_the_supervisor_bit`: PD at
    // 0x1000, PT at 0x2000, linear 0x3000 -> present/writable/supervisor-only
    // frame 0x5000.
    let mut memory = vec![0; 0x6000];
    memory[0x1000..0x1004].copy_from_slice(&0x0000_2007u32.to_le_bytes()); // PDE: PT, present+rw+user
    memory[0x200c..0x2010].copy_from_slice(&0x0000_5003u32.to_le_bytes()); // PTE[3]: frame, present+rw, U/S=0
    let mut cpu = CpuGsw::default();
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.control.cr3 = 0x1000;
    let mut bus = TestBus::with_memory(memory);

    // Enter V86 with a real-mode-style CS whose low bits are 0, not 3 -- the
    // exact case a live `CS.selector & 3` formula would misclassify as
    // supervisor. `current_privilege_level` must still answer 3 here because
    // `self.cpl` is the transition-pinned cache, not a live read of CS.
    cpu.registers.eflags |= FLAG_VM;
    cpu.load_segment_real(SegmentIndex::Cs, 0xF000); // selector low bits == 0b00
    cpu.cpl = 3; // what every real V86 transition (IRET/task-switch) sets
    assert!(cpu.is_v86_mode());
    assert_eq!(
        cpu.registers.cs().selector & 3,
        0,
        "CS RPL bits are 00, not 11"
    );

    let faulted = cpu.translate_linear(&mut bus, 0x3000, false);
    assert!(
        matches!(
            faulted,
            Err(InternalFault::Exception {
                vector: 14,
                error_code: Some(0x5)
            })
        ),
        "a V86 access to a supervisor-only page must #PF like any other user access: {faulted:?}"
    );
    assert_eq!(cpu.control.cr2, 0x3000);

    // Same V86 task, a user-accessible page (frame 0x4000 via PTE[2], U/S=1):
    // translation succeeds, proving the fault above was the supervisor bit and
    // not some unrelated V86 restriction.
    let mut memory = bus.memory;
    memory[0x2008..0x200c].copy_from_slice(&0x0000_4007u32.to_le_bytes());
    bus = TestBus::with_memory(memory);
    assert_eq!(
        cpu.translate_linear(&mut bus, 0x2000, false).unwrap(),
        0x4000
    );
}

// Paged-mode fetch throughput; the case the TLB targets. Run with:
// cargo test --release -p izarravm-cpu -- --ignored --nocapture tlb_paged
#[test]
#[ignore]
fn tlb_paged_fetch_throughput() {
    let mut memory = vec![0u8; 0x10000];
    memory[0..3].copy_from_slice(&[0xfa, 0xeb, 0xfe]); // cli; jmp $
    memory[0x1000..0x1004].copy_from_slice(&0x0000_2007u32.to_le_bytes()); // PDE[0] -> PT
    for i in 0..16u32 {
        let off = 0x2000 + (i as usize) * 4;
        memory[off..off + 4].copy_from_slice(&((i << 12) | 0x007).to_le_bytes()); // identity PTEs
    }
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.control.cr3 = 0x1000;
    cpu.control.cr0 |= CR0_PG;
    let mut bus = TestBus::with_memory(memory);

    let iters = 50_000_000u64;
    let t = std::time::Instant::now();
    for _ in 0..iters {
        cpu.cycle(&mut bus).unwrap();
    }
    let secs = t.elapsed().as_secs_f64();
    println!(
        "tlb_paged_fetch_throughput: {iters} paged instructions in {secs:.3}s = {:.1} M instr/s",
        iters as f64 / secs / 1.0e6
    );
}

#[test]
fn tlb_caches_translations_and_is_non_snooping_until_flushed() {
    // PD at 0x1000, PT at 0x2000. Linear 0x3000 -> present+rw+user frame 0x5000.
    let mut memory = vec![0; 0x7000];
    memory[0x1000..0x1004].copy_from_slice(&0x0000_2007u32.to_le_bytes()); // PDE[0]
    memory[0x200c..0x2010].copy_from_slice(&0x0000_5007u32.to_le_bytes()); // PTE[3]
    let mut cpu = CpuGsw::default();
    cpu.control.cr0 |= CR0_PG;
    cpu.control.cr3 = 0x1000;
    let mut bus = TestBus::with_memory(memory);

    // First translation walks the table and fills the TLB.
    assert_eq!(
        cpu.translate_linear(&mut bus, 0x3000, false).unwrap(),
        0x5000
    );

    // Repoint the PTE to frame 0x6000 in memory with no INVLPG / CR3 reload.
    bus.memory[0x200c..0x2010].copy_from_slice(&0x0000_6007u32.to_le_bytes());

    // Real x86 TLBs do not snoop page-table writes: the stale cached frame is
    // returned until an explicit flush -- the faithful behavior a guest relies
    // on (it must INVLPG / reload CR3 after editing a PTE).
    assert_eq!(
        cpu.translate_linear(&mut bus, 0x3000, false).unwrap(),
        0x5000
    );

    // After a flush the next access re-walks and sees the new mapping.
    cpu.tlb.flush();
    assert_eq!(
        cpu.translate_linear(&mut bus, 0x3000, false).unwrap(),
        0x6000
    );
}

#[test]
fn cr0_wp_gates_supervisor_writes_to_read_only_pages() {
    // PD at 0x1000, PT at 0x2000. Linear 0x3000 maps to a present, read-only
    // (R/W=0), supervisor (U/S=0) page at frame 0x5000.
    let mut memory = vec![0; 0x6000];
    memory[0x1000..0x1004].copy_from_slice(&0x0000_2001u32.to_le_bytes()); // PDE: PT, present, R/W=0, U/S=0
    memory[0x200c..0x2010].copy_from_slice(&0x0000_5001u32.to_le_bytes()); // PTE[3]: frame, present, R/W=0, U/S=0
    let mut cpu = CpuGsw::default();
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.control.cr3 = 0x1000;
    // Supervisor: CPL 0.
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x0000,
            base: 0,
            limit: 0xffff_ffff,
            access: 0x9b,
            default_size_32: false,
        },
    );
    let mut bus = TestBus::with_memory(memory);

    // WP clear (the 386 default): a supervisor write to the read-only page
    // succeeds and resolves to the mapped frame.
    assert_eq!(cpu.control.cr0 & CR0_WP, 0);
    assert_eq!(
        cpu.translate_linear(&mut bus, 0x3000, true).unwrap(),
        0x5000
    );

    // A supervisor read always passes regardless of WP.
    assert_eq!(
        cpu.translate_linear(&mut bus, 0x3000, false).unwrap(),
        0x5000
    );

    // WP set (the 486 feature): the same supervisor write now faults #PF with
    // error code present|write (bits 0 and 1 -> 0b011 = 0x3); the U/S bit is 0
    // because the access is supervisor, and cr2 holds the faulting address.
    cpu.control.cr0 |= CR0_WP;
    let faulted = cpu.translate_linear(&mut bus, 0x3000, true);
    assert!(
        matches!(
            faulted,
            Err(InternalFault::Exception {
                vector: 14,
                error_code: Some(0x3)
            })
        ),
        "{faulted:?}"
    );
    assert_eq!(cpu.control.cr2, 0x3000);

    // A supervisor read is unaffected by WP and still resolves.
    assert_eq!(
        cpu.translate_linear(&mut bus, 0x3000, false).unwrap(),
        0x5000
    );
}

#[test]
fn stosb_writes_al_to_es_di() {
    let mut memory = vec![0; 1024];
    memory[0] = 0xaa;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_edi(0x200);
    cpu.write_gpr8(0, b'S');
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(bus.memory[0x200], b'S');
    assert_eq!(cpu.registers.edi(), 0x201);
}

#[test]
fn rep_stosb_fills_es_di() {
    // rep stosb (0xf3 0xaa), cx=3, al=0xee. Fills 3 bytes at es:di, cx -> 0, di += 3.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0xf3, 0xaa]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_DF, false);
    cpu.write_gpr8(0, 0xee);
    cpu.registers.set_edi(0x300);
    cpu.registers.set_ecx(3);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(&bus.memory[0x300..0x303], &[0xee, 0xee, 0xee]);
    assert_eq!(cpu.registers.edi(), 0x303);
    assert_eq!(cpu.registers.ecx(), 0);
}

#[test]
fn lodsw_loads_ax_and_advances_si() {
    // lodsw (0xad). [ds:si]=0x1234 (LE) -> ax; si += 2.
    let mut memory = vec![0; 1024];
    memory[0] = 0xad;
    memory[0x100..0x102].copy_from_slice(&0x1234u16.to_le_bytes());
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_DF, false);
    cpu.registers.set_esi(0x100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x1234);
    assert_eq!(cpu.registers.esi(), 0x102);
}

#[test]
fn out_dx_al_uses_dx_port() {
    let mut memory = vec![0; 16];
    memory[0..6].copy_from_slice(&[0xba, 0xf8, 0x03, 0xb0, b'X', 0xee]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();
    cpu.cycle(&mut bus).unwrap();
    cpu.cycle(&mut bus).unwrap();

    assert!(
        bus.trace
            .cycles()
            .iter()
            .any(|cycle| { cycle.kind == BusAccessKind::IoWrite && cycle.address == 0x03f8 })
    );
}

#[test]
fn test_byte_sets_sign_flag() {
    // test al, al with al = 0x80  (0x84 modrm 0xc0). SF must reflect bit 7.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0x84, 0xc0]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr8(0, 0x80);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_SF));
    assert!(!cpu.flag(FLAG_ZF));
    assert!(!cpu.flag(FLAG_CF));
}

#[test]
fn test_word_immediate_group_f7() {
    // test bx, 0x0001  (0xf7 /0, modrm 0xc3, imm 0x0001). bx=0x0002 -> ZF set.
    let mut memory = vec![0; 16];
    memory[0..4].copy_from_slice(&[0xf7, 0xc3, 0x01, 0x00]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x0002);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_ZF));
}

#[test]
fn group81_add_memory_with_displacement_and_immediate() {
    // add word [bx+0x10], 0x0102  (0x81 /0, modrm 0x47, disp 0x10, imm 0x0102)
    let mut memory = vec![0; 1024];
    memory[0..6].copy_from_slice(&[0x81, 0x47, 0x10, 0x02, 0x01, 0xf4]);
    memory[0x210..0x212].copy_from_slice(&0x0003u16.to_le_bytes());
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x200);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(
        u16::from_le_bytes([bus.memory[0x210], bus.memory[0x211]]),
        0x0105
    );
    assert_eq!(cpu.registers.eip, 5); // opcode + modrm + disp8 + imm16
}

#[test]
fn group83_sign_extends_immediate() {
    // sub bx, -1  (0x83 /5, modrm 0xeb, imm 0xff -> -1)
    let mut memory = vec![0; 16];
    memory[0..3].copy_from_slice(&[0x83, 0xeb, 0xff]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x0005);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x0006); // 5 - (-1) = 6
}

#[test]
fn add_rm_reg_byte_writes_memory_with_displacement() {
    // add [bx+0x10], al   (opcode 0x00, modrm 0x47, disp 0x10)
    let mut memory = vec![0; 1024];
    memory[0..4].copy_from_slice(&[0x00, 0x47, 0x10, 0xf4]);
    memory[0x210] = 0x01; // [bx+0x10] initial
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x200);
    cpu.write_gpr8(0, 0x05); // al
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(bus.memory[0x210], 0x06);
    assert_eq!(cpu.registers.eip, 3); // opcode + modrm + disp8, no double-fetch
}

#[test]
fn sub_reg_rm_sets_flags() {
    // sub al, bl  (opcode 0x2a, modrm 0xc3)
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0x2a, 0xc3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr8(0, 0x05); // al
    cpu.write_gpr8(3, 0x05); // bl
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr8(0), 0x00);
    assert!(cpu.flag(FLAG_ZF));
    assert!(!cpu.flag(FLAG_CF));
}

#[test]
fn cmp_does_not_write_back() {
    // cmp al, 0x10 is form via 0x3c (AL, imm8)
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0x3c, 0x10]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr8(0, 0x10);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr8(0), 0x10); // unchanged
    assert!(cpu.flag(FLAG_ZF));
}

#[test]
fn alu_add_byte_sets_carry_zero_and_aux() {
    let mut cpu = CpuGsw::default();
    let result = cpu.alu(0, 0xff, 0x01, BusWidth::Byte);
    assert_eq!(result, 0x00);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_ZF));
    assert!(cpu.flag(FLAG_AF));
    assert!(!cpu.flag(FLAG_OF));
}

#[test]
fn alu_adc_uses_carry_in() {
    let mut cpu = CpuGsw::default();
    cpu.set_flag(FLAG_CF, true);
    let result = cpu.alu(2, 0x01, 0x01, BusWidth::Word); // ADC 1,1 with CF=1 -> 3
    assert_eq!(result, 0x0003);
    assert!(!cpu.flag(FLAG_CF));
}

#[test]
fn alu_sub_byte_sets_borrow_and_sign() {
    let mut cpu = CpuGsw::default();
    let result = cpu.alu(5, 0x00, 0x01, BusWidth::Byte); // 0 - 1 = 0xff
    assert_eq!(result, 0xff);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_SF));
    assert!(cpu.flag(FLAG_AF));
}

#[test]
fn alu_sbb_uses_borrow_in() {
    let mut cpu = CpuGsw::default();
    cpu.set_flag(FLAG_CF, true);
    let result = cpu.alu(3, 0x05, 0x02, BusWidth::Word); // 5 - 2 - 1 = 2
    assert_eq!(result, 0x0002);
    assert!(!cpu.flag(FLAG_CF));
}

#[test]
fn alu_logic_clears_carry_overflow_leaves_aux() {
    let mut cpu = CpuGsw::default();
    cpu.set_flag(FLAG_AF, true);
    let result = cpu.alu(4, 0xf0, 0x0f, BusWidth::Byte); // AND -> 0
    assert_eq!(result, 0x00);
    assert!(!cpu.flag(FLAG_CF));
    assert!(!cpu.flag(FLAG_OF));
    assert!(cpu.flag(FLAG_ZF));
    assert!(cpu.flag(FLAG_AF)); // AND leaves AF untouched (undefined)
}

#[test]
fn alu_add_byte_overflow_without_carry() {
    let mut cpu = CpuGsw::default();
    let result = cpu.alu(0, 0x7f, 0x01, BusWidth::Byte); // 127 + 1 -> 0x80
    assert_eq!(result, 0x80);
    assert!(cpu.flag(FLAG_OF)); // signed overflow, isolated from carry
    assert!(!cpu.flag(FLAG_CF)); // no unsigned carry
    assert!(cpu.flag(FLAG_SF));
    assert!(cpu.flag(FLAG_AF));
}

#[test]
fn alu_sbb_borrow_in_with_max_subtrahend() {
    let mut cpu = CpuGsw::default();
    cpu.set_flag(FLAG_CF, true); // borrow in
    let result = cpu.alu(3, 0x00, 0xff, BusWidth::Byte); // 0 - 0xff - 1
    assert_eq!(result, 0x00);
    assert!(cpu.flag(FLAG_CF)); // b + borrow must not wrap to 0 and clear CF
    assert!(cpu.flag(FLAG_ZF));
}

#[test]
fn alu_parity_uses_low_byte_only() {
    let mut cpu = CpuGsw::default();
    let result = cpu.alu(0, 0x00ff, 0x0001, BusWidth::Word); // -> 0x0100
    assert_eq!(result, 0x0100);
    assert!(cpu.flag(FLAG_PF)); // low byte 0x00 is even parity; full word would be odd
}

#[test]
fn alu_sign_flag_word_uses_bit15() {
    let mut cpu = CpuGsw::default();
    let result = cpu.alu(0, 0x8000, 0x0000, BusWidth::Word);
    assert_eq!(result, 0x8000);
    assert!(cpu.flag(FLAG_SF));
}

#[test]
fn inc_reg_preserves_carry_flag() {
    // inc ax (0x40) with CF set: AX increments, CF stays set, AF set by 0xff+1.
    let mut memory = vec![0; 16];
    memory[0] = 0x40;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_CF, true);
    cpu.write_reg16(Reg16::Ax, 0x00ff);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0100);
    assert!(cpu.flag(FLAG_CF)); // INC must not touch CF
    assert!(cpu.flag(FLAG_AF));
}

#[test]
fn dec_reg_sets_zero_and_keeps_carry_clear() {
    // dec ax (0x48) with CF clear: AX -> 0, ZF set, CF still clear.
    let mut memory = vec![0; 16];
    memory[0] = 0x48;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_CF, false);
    cpu.write_reg16(Reg16::Ax, 0x0001);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0000);
    assert!(cpu.flag(FLAG_ZF));
    assert!(!cpu.flag(FLAG_CF));
}

#[test]
fn inc_word_memory_via_ff_group() {
    // inc word [bx]  (0xff /0, modrm 0x07). 0x00ff -> 0x0100.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0xff, 0x07]);
    memory[0x200..0x202].copy_from_slice(&0x00ffu16.to_le_bytes());
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x200);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(
        u16::from_le_bytes([bus.memory[0x200], bus.memory[0x201]]),
        0x0100
    );
    assert!(!cpu.flag(FLAG_ZF));
}

#[test]
fn call_near_indirect_register_pushes_return_and_jumps() {
    // call ax  (0xff /2, modrm 0xd0). Pushes return eip (2), jumps to ax.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0xff, 0xd0]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Sp, 0x100);
    cpu.write_reg16(Reg16::Ax, 0x0050);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x0050);
    assert_eq!(cpu.read_reg16(Reg16::Sp), 0x00fe);
    assert_eq!(
        u16::from_le_bytes([bus.memory[0xfe], bus.memory[0xff]]),
        0x0002
    );
}

#[test]
fn jmp_near_indirect_sets_eip_without_push() {
    // jmp bx  (0xff /4, modrm 0xe3).
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0xff, 0xe3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Sp, 0x100);
    cpu.write_reg16(Reg16::Bx, 0x0030);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x0030);
    assert_eq!(cpu.read_reg16(Reg16::Sp), 0x0100); // no push
}

#[test]
fn push_rm_writes_value_and_decrements_sp() {
    // push cx  (0xff /6, modrm 0xf1).
    let mut memory = vec![0; 256];
    memory[0..2].copy_from_slice(&[0xff, 0xf1]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Sp, 0x100);
    cpu.write_reg16(Reg16::Cx, 0xbeef);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Sp), 0x00fe);
    assert_eq!(
        u16::from_le_bytes([bus.memory[0xfe], bus.memory[0xff]]),
        0xbeef
    );
}

#[test]
fn inc_byte_memory_with_displacement() {
    // inc byte [bx+0x10]  (0xfe /0, modrm 0x47, disp 0x10). 0x7f -> 0x80.
    let mut memory = vec![0; 1024];
    memory[0..3].copy_from_slice(&[0xfe, 0x47, 0x10]);
    memory[0x210] = 0x7f;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x200);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(bus.memory[0x210], 0x80);
    assert!(cpu.flag(FLAG_SF));
    assert!(cpu.flag(FLAG_OF)); // 0x7f + 1 byte overflow
    assert_eq!(cpu.registers.eip, 3); // opcode + modrm + disp8
}

#[test]
fn inc_word_overflow_sets_of_and_sf() {
    // inc ax (0x40) on 0x7fff: -> 0x8000, OF and SF set, CF preserved.
    let mut memory = vec![0; 16];
    memory[0] = 0x40;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_CF, true);
    cpu.write_reg16(Reg16::Ax, 0x7fff);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x8000);
    assert!(cpu.flag(FLAG_OF));
    assert!(cpu.flag(FLAG_SF));
    assert!(cpu.flag(FLAG_CF)); // preserved
}

#[test]
fn cmp_memory_form_issues_no_write() {
    // cmp [bx], al  (0x38 modrm 0x07). Equal operands -> ZF, and no write cycle.
    let mut memory = vec![0; 1024];
    memory[0..3].copy_from_slice(&[0x38, 0x07, 0xf4]);
    memory[0x200] = 0x42;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x200);
    cpu.write_gpr8(0, 0x42); // al
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_ZF));
    assert_eq!(bus.memory[0x200], 0x42); // unchanged
    assert!(
        !bus.trace
            .cycles()
            .iter()
            .any(|cycle| cycle.kind == BusAccessKind::DataWrite)
    );
}

#[test]
fn incdec_preserve_carry_both_directions() {
    // DEC with CF set leaves CF set.
    let mut memory = vec![0; 16];
    memory[0] = 0x48; // dec ax
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_CF, true);
    cpu.write_reg16(Reg16::Ax, 0x0005);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0004);
    assert!(cpu.flag(FLAG_CF));

    // INC with CF clear leaves CF clear.
    let mut memory = vec![0; 16];
    memory[0] = 0x40; // inc ax
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_CF, false);
    cpu.write_reg16(Reg16::Ax, 0x0005);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0006);
    assert!(!cpu.flag(FLAG_CF));
}

#[test]
fn dec_word_overflow_sets_of() {
    // dec ax (0x48) on 0x8000 -> 0x7fff: OF set, SF clear.
    let mut memory = vec![0; 16];
    memory[0] = 0x48;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x8000);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x7fff);
    assert!(cpu.flag(FLAG_OF));
    assert!(!cpu.flag(FLAG_SF));
}

#[test]
fn call_near_indirect_memory_displacement_return_addr() {
    // call [bx+0x10] (0xff /2, modrm 0x57, disp 0x10): 3-byte instruction,
    // return address must be computed after the displacement fetch.
    let mut memory = vec![0; 1024];
    memory[0..3].copy_from_slice(&[0xff, 0x57, 0x10]);
    memory[0x210..0x212].copy_from_slice(&0x0080u16.to_le_bytes());
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Sp, 0x100);
    cpu.write_reg16(Reg16::Bx, 0x200);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eip, 0x0080);
    assert_eq!(
        u16::from_le_bytes([bus.memory[0xfe], bus.memory[0xff]]),
        0x0003
    );
}

#[test]
fn push_sp_uses_pre_decrement_value() {
    // push sp (0xff /6, modrm 0xf4): the 386 pushes SP before the decrement.
    let mut memory = vec![0; 256];
    memory[0..2].copy_from_slice(&[0xff, 0xf4]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Sp, 0x100);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Sp), 0x00fe);
    assert_eq!(
        u16::from_le_bytes([bus.memory[0xfe], bus.memory[0xff]]),
        0x0100
    );
}

#[test]
fn inc_dword_uses_32bit_width() {
    // 0x66 0x40 = inc eax (32-bit operand): 0x0000ffff -> 0x00010000.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0x66, 0x40]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0x0000_ffff);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eax(), 0x0001_0000);
    assert!(!cpu.flag(FLAG_ZF));
    assert!(!cpu.flag(FLAG_SF));
}

#[test]
fn shl_word_by_one_sets_of_and_clears_cf() {
    // shl ax,1 (0xd1 /4, modrm 0xe0). 0x4000 -> 0x8000, CF=0 (old bit15), OF=1, SF=1.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xd1, 0xe0]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x4000);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x8000);
    assert!(!cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_OF));
    assert!(cpu.flag(FLAG_SF));
}

#[test]
fn shr_word_by_one_sets_cf_and_of() {
    // shr ax,1 (0xd1 /5, modrm 0xe8). 0x8001 -> 0x4000, CF=1, OF=msb(orig)=1.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xd1, 0xe8]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x8001);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x4000);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_OF));
    assert!(!cpu.flag(FLAG_SF)); // result 0x4000 is positive
}

#[test]
fn shl_dword_by_one_via_operand_size_prefix() {
    // shl eax,1 (0x66 0xd1 /4, modrm 0xe0). 0x4000_0000 -> 0x8000_0000, CF=0, OF=1, SF=1.
    let mut memory = vec![0; 16];
    memory[0..3].copy_from_slice(&[0x66, 0xd1, 0xe0]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0x4000_0000);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eax(), 0x8000_0000);
    assert!(!cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_OF));
    assert!(cpu.flag(FLAG_SF));
    assert_eq!(cpu.registers.eip, 3); // prefix + opcode + modrm
}

#[test]
fn repeated_operand_size_prefix_stays_active() {
    // 66 66 d1 e0 = shl eax,1 with a redundant operand-size prefix. The
    // second 66 must not cancel the first, so this stays a 32-bit shift.
    let mut memory = vec![0; 16];
    memory[0..4].copy_from_slice(&[0x66, 0x66, 0xd1, 0xe0]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0x4000_0000);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eax(), 0x8000_0000);
    assert_eq!(cpu.registers.eip, 4); // two prefixes + opcode + modrm
}

#[test]
fn sar_word_by_one_preserves_sign_and_clears_of() {
    // sar ax,1 (0xd1 /7, modrm 0xf8). 0x8001 -> 0xc000, CF=1, OF=0, SF=1.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xd1, 0xf8]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x8001);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0xc000);
    assert!(cpu.flag(FLAG_CF));
    assert!(!cpu.flag(FLAG_OF));
    assert!(cpu.flag(FLAG_SF));
}

#[test]
fn shl_byte_via_c0_imm_only_touches_low_byte() {
    // shl al,1 (0xc0 /4, modrm 0xe0, imm 0x01). ax=0xff81 -> al 0x81<<1=0x02, ah preserved.
    let mut memory = vec![0; 16];
    memory[0..3].copy_from_slice(&[0xc0, 0xe0, 0x01]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0xff81);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0xff02);
    assert!(cpu.flag(FLAG_CF)); // old bit7 of 0x81
    assert_eq!(cpu.registers.eip, 3); // opcode + modrm + imm8
}

#[test]
fn shl_word_by_imm_count() {
    // shl ax,4 (0xc1 /4, modrm 0xe0, imm 0x04). 0x0001 -> 0x0010.
    let mut memory = vec![0; 16];
    memory[0..3].copy_from_slice(&[0xc1, 0xe0, 0x04]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0001);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0010);
    assert_eq!(cpu.registers.eip, 3);
}

#[test]
fn shift_count_masked_to_five_bits() {
    // shl ax,cl with cl=33 (0xd3 /4, modrm 0xe0). 33 & 0x1f == 1, so one shift.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xd3, 0xe0]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x4000);
    cpu.write_reg16(Reg16::Cx, 33);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x8000);
}

#[test]
fn shift_count_zero_touches_no_flags() {
    // shl ax,cl with cl=32 (0xd3 /4). 32 & 0x1f == 0: operand and flags unchanged.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xd3, 0xe0]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x1234);
    cpu.write_reg16(Reg16::Cx, 32);
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x1234);
    assert!(cpu.flag(FLAG_CF)); // unchanged: a zero count touches no flags
}

#[test]
fn rol_word_by_one() {
    // rol ax,1 (0xd1 /0, modrm 0xc0). 0x8000 -> 0x0001, CF=1, OF=msb^cf=0^1=1.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xd1, 0xc0]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x8000);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0001);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_OF));
}

#[test]
fn ror_word_by_one() {
    // ror ax,1 (0xd1 /1, modrm 0xc8). 0x0001 -> 0x8000, CF=1, OF=msb^next=1^0=1.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xd1, 0xc8]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0001);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x8000);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_OF));
}

#[test]
fn rcl_word_rotates_through_carry() {
    // rcl ax,1 (0xd1 /2, modrm 0xd0). ax=0x0000, CF=1 -> 0x0001, CF=0 (old msb=0).
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xd1, 0xd0]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0000);
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0001); // carry rotated into bit 0
    assert!(!cpu.flag(FLAG_CF)); // old msb (0) rotated out
    assert!(!cpu.flag(FLAG_OF)); // result_msb(0) ^ cf(0)
}

#[test]
fn rcr_word_rotates_through_carry() {
    // rcr ax,1 (0xd1 /3, modrm 0xd8). ax=0x0000, CF=1 -> 0x8000, CF=0 (old bit0=0).
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xd1, 0xd8]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0000);
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x8000); // carry rotated into bit 15
    assert!(!cpu.flag(FLAG_CF)); // old bit0 (0) rotated out
    assert!(cpu.flag(FLAG_OF)); // result_msb(1) ^ result_bit14(0)
}

#[test]
fn rotate_leaves_sign_zero_parity_untouched() {
    // rol ax,1: rotates touch only CF/OF, never SF/ZF/PF. Set ZF first, then
    // rotate to a nonzero result and confirm ZF survives.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xd1, 0xc0]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x8000);
    cpu.set_flag(FLAG_ZF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0001);
    assert!(cpu.flag(FLAG_ZF)); // unchanged by a rotate
}

#[test]
fn ror_byte_by_cl_multi_bit() {
    // ror al,cl with cl=3 (0xd2 /1, modrm 0xc8). Exercises the byte width
    // (msb 0x80, shift by bits-1=7) and a multi-bit count. al 0x01 ror 3 = 0x20.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xd2, 0xc8]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0001);
    cpu.write_reg16(Reg16::Cx, 3);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0020); // ah preserved, al rotated
    assert!(!cpu.flag(FLAG_CF)); // last bit out is 0
}

#[test]
fn not_byte_leaves_flags_untouched() {
    // not bl (0xf6 /2, modrm 0xd3). 0x0f -> 0xf0; NOT affects no flags.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xf6, 0xd3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x000f);
    cpu.set_flag(FLAG_CF, true);
    cpu.set_flag(FLAG_ZF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx) & 0xff, 0xf0);
    assert!(cpu.flag(FLAG_CF)); // unchanged
    assert!(cpu.flag(FLAG_ZF)); // unchanged
}

#[test]
fn neg_byte_sets_carry_and_sign() {
    // neg bl (0xf6 /3, modrm 0xdb). 0x01 -> 0xff; CF set, SF set, ZF clear.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xf6, 0xdb]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x0001);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx) & 0xff, 0xff);
    assert!(cpu.flag(FLAG_CF)); // operand nonzero
    assert!(cpu.flag(FLAG_SF));
    assert!(!cpu.flag(FLAG_ZF));
}

#[test]
fn neg_zero_clears_carry_and_sets_zero() {
    // neg bl of 0x00 -> 0x00; CF clear, ZF set.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xf6, 0xdb]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x0000);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx) & 0xff, 0x00);
    assert!(!cpu.flag(FLAG_CF)); // operand zero
    assert!(cpu.flag(FLAG_ZF));
}

#[test]
fn neg_byte_overflow_at_0x80() {
    // neg bl of 0x80 -> 0x80; OF set (only value that negates to itself), CF and SF set.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xf6, 0xdb]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x0080);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx) & 0xff, 0x80);
    assert!(cpu.flag(FLAG_OF));
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_SF));
}

#[test]
fn not_word_via_f7_complements() {
    // not bx (0xf7 /2, modrm 0xd3). 0x0ff0 -> 0xf00f; flags unchanged.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xf7, 0xd3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x0ff0);
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx), 0xf00f);
    assert!(cpu.flag(FLAG_CF)); // NOT touches no flags
}

#[test]
fn mul_byte_sets_carry_when_high_nonzero() {
    // mul bl (0xf6 /4, modrm 0xe3). al=0x10, bl=0x10 -> ax=0x0100; CF/OF set (ah != 0).
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xf6, 0xe3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0010);
    cpu.write_reg16(Reg16::Bx, 0x0010);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0100);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_OF));
}

#[test]
fn mul_byte_clears_carry_when_high_zero() {
    // mul bl. al=0x05, bl=0x03 -> ax=0x000f; CF/OF clear.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xf6, 0xe3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0005);
    cpu.write_reg16(Reg16::Bx, 0x0003);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x000f);
    assert!(!cpu.flag(FLAG_CF));
    assert!(!cpu.flag(FLAG_OF));
}

#[test]
fn mul_word_writes_dx_ax_preserving_high_halves() {
    // mul bx (0xf7 /4, modrm 0xe3). ax=0x1000, bx=0x0010 -> product 0x0010_0000:
    // ax=0x0000, dx=0x0001; CF/OF set. High 16 bits of EAX/EDX must survive.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xf7, 0xe3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0xaaaa_1000);
    cpu.registers.set_edx(0xbbbb_0000);
    cpu.registers.set_ebx(0x0000_0010);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eax(), 0xaaaa_0000); // ax=0, high preserved
    assert_eq!(cpu.registers.edx(), 0xbbbb_0001); // dx=1, high preserved
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_OF));
}

#[test]
fn imul_byte_clears_carry_when_result_fits() {
    // imul bl (0xf6 /5, modrm 0xeb). al=0xff(-1), bl=0x02(+2) -> ax=0xfffe(-2);
    // CF/OF clear because the high half is the sign extension of the low half.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xf6, 0xeb]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x00ff);
    cpu.write_reg16(Reg16::Bx, 0x0002);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0xfffe);
    assert!(!cpu.flag(FLAG_CF));
    assert!(!cpu.flag(FLAG_OF));
}

#[test]
fn imul_byte_sets_carry_when_result_overflows() {
    // imul bl. al=0x10(+16), bl=0x10(+16) -> ax=0x0100(+256); the low byte is 0x00,
    // its sign extension is 0x0000 != 0x0100, so CF/OF set.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xf6, 0xeb]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0010);
    cpu.write_reg16(Reg16::Bx, 0x0010);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0100);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_OF));
}

#[test]
fn mul_dword_writes_edx_eax() {
    // mul ebx (0x66 0xf7 /4, modrm 0xe3). eax=0x0001_0000 * ebx=0x0001_0000
    // = 0x1_0000_0000 -> eax=0, edx=1; CF/OF set. Exercises the u64 dword path.
    let mut memory = vec![0; 16];
    memory[0..3].copy_from_slice(&[0x66, 0xf7, 0xe3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0x0001_0000);
    cpu.registers.set_ebx(0x0001_0000);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eax(), 0x0000_0000);
    assert_eq!(cpu.registers.edx(), 0x0000_0001);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_OF));
}

#[test]
fn div_byte_writes_quotient_and_remainder() {
    // div bl (0xf6 /6, modrm 0xf3). ax=0x0011(17), bl=0x05 -> al=3, ah=2.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xf6, 0xf3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0011);
    cpu.write_reg16(Reg16::Bx, 0x0005);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0203); // ah=2 (rem), al=3 (quot)
}

#[test]
fn div_word_writes_ax_and_dx() {
    // div bx (0xf7 /6, modrm 0xf3). dx:ax = 0x0000:0x0011 (17), bx=5 -> ax=3 (quot), dx=2 (rem).
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xf7, 0xf3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Dx, 0x0000);
    cpu.write_reg16(Reg16::Ax, 0x0011);
    cpu.write_reg16(Reg16::Bx, 0x0005);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0003);
    assert_eq!(cpu.read_reg16(Reg16::Dx), 0x0002);
}

#[test]
fn idiv_byte_negative_dividend_truncates_toward_zero() {
    // idiv bl (0xf6 /7, modrm 0xfb). ax=-17=0xffef, bl=+5 -> quot=-3 (0xfd), rem=-2 (0xfe).
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xf6, 0xfb]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0xffef);
    cpu.write_reg16(Reg16::Bx, 0x0005);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0xfd); // al = -3
    assert_eq!((cpu.read_reg16(Reg16::Ax) >> 8) & 0xff, 0xfe); // ah = -2
}

#[test]
fn div_by_zero_returns_error_without_writes() {
    // div bl with bl=0 -> #DE delivered through the real-mode IVT; ax unchanged.
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = DE_CODE_ORIGIN;
    cpu.registers.set_esp(0x2000);
    cpu.write_reg16(Reg16::Ax, 0x1234);
    cpu.write_reg16(Reg16::Bx, 0x0000);
    let mut bus = de_trap_bus(&[0xf6, 0xf3]);

    expect_de_delivered(&mut cpu, &mut bus);
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x1234); // no writes
}

#[test]
fn div_quotient_overflow_returns_error() {
    // div bl: ax=0xffff, bl=0x01 -> quotient 0xffff > 0xff -> #DE delivered.
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = DE_CODE_ORIGIN;
    cpu.registers.set_esp(0x2000);
    cpu.write_reg16(Reg16::Ax, 0xffff);
    cpu.write_reg16(Reg16::Bx, 0x0001);
    let mut bus = de_trap_bus(&[0xf6, 0xf3]);

    expect_de_delivered(&mut cpu, &mut bus);
}

#[test]
fn div_dword_writes_eax_edx() {
    // div ebx (0x66 0xf7 /6, modrm 0xf3). edx:eax = 0x1_0000_0005, ebx=2
    // -> quot=0x8000_0002, rem=1. Exercises the u64 dword path.
    let mut memory = vec![0; 16];
    memory[0..3].copy_from_slice(&[0x66, 0xf7, 0xf3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_edx(0x0000_0001);
    cpu.registers.set_eax(0x0000_0005);
    cpu.registers.set_ebx(0x0000_0002);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eax(), 0x8000_0002); // quotient
    assert_eq!(cpu.registers.edx(), 0x0000_0001); // remainder
}

#[test]
fn idiv_dword_min_over_negative_one_is_divide_error() {
    // idiv ebx (0x66 0xf7 /7, modrm 0xfb). edx:eax = i64::MIN, ebx = -1.
    // checked_div catches the overflow so this is #DE (delivered), not a panic.
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = DE_CODE_ORIGIN;
    cpu.registers.set_esp(0x2000);
    cpu.registers.set_edx(0x8000_0000);
    cpu.registers.set_eax(0x0000_0000);
    cpu.registers.set_ebx(0xffff_ffff);
    let mut bus = de_trap_bus(&[0x66, 0xf7, 0xfb]);

    expect_de_delivered(&mut cpu, &mut bus);
}
