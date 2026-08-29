// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// A flat, CPL-0, 32-bit protected-mode segment covering the whole address space. Mirrors the
/// `seg` helper other protected-mode fixtures in `cpu_test.rs` use.
fn flat_pm_segment(selector: u16, access: u8) -> SegmentRegister {
    SegmentRegister {
        selector,
        base: 0,
        limit: 0xffff_ffff,
        access,
        default_size_32: true,
    }
}

/// The run-end ledger's identity, checked against one `PerfCounters` snapshot. Named terms only,
/// per `lib.rs`'s `straight_line_runs` doc comment: the six normal-return break reasons plus the
/// seventh, `brk_fatal`, which the `run_budgeted` wrapper counts on a propagated hard `CpuError`.
fn ledger_identity_gap(p: &PerfCounters) -> i128 {
    i128::from(p.straight_line_runs)
        - i128::from(p.brk_decode_or_branch)
        - i128::from(p.brk_step)
        - i128::from(p.brk_interrupt)
        - i128::from(p.brk_cap)
        - i128::from(p.brk_halt)
        - i128::from(p.brk_rep_resume)
        - i128::from(p.brk_fatal)
}

/// `brk_rep_resume`, in isolation: a REP STOSB budgeted so tightly that it must yield mid-count
/// more than once before it finishes. Each yield is the `rep_resume_active` break at
/// `run.rs`'s `if self.rep_resume_active { break; }`, which increments no counter today.
///
/// Not a bare "nonzero" check: the run count actually reached (`straight_line_runs`) is compared
/// against the reasons the ledger can name, and the two must match exactly once `brk_rep_resume`
/// is wired up. Before that, the three chunk-boundary runs vanish from the sum: their
/// `total` core-clock charge lands in `elapsed_clocks`, but no `brk_*` counter records why they
/// ended, so `straight_line_runs` outruns the visible reasons by exactly 3.
#[test]
fn brk_rep_resume_counts_budgeted_chunk_yields() {
    const ORIGIN: u32 = 0x10;
    let mut memory = vec![0u8; 0x1000];
    memory[ORIGIN as usize..ORIGIN as usize + 2].copy_from_slice(&[0xf3, 0xaa]); // REP STOSB
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw386);
    for segment in [SegmentIndex::Cs, SegmentIndex::Es, SegmentIndex::Ss] {
        cpu.load_segment_real(segment, 0);
    }
    cpu.registers.eip = ORIGIN;
    cpu.registers.set_eax(0x5a);
    cpu.registers.set_edi(0x400);
    cpu.registers.set_ecx(4);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_page_clocks = true;
    bus.report_batch_clocks = true;

    // cap=14 (the same cap the sibling `budgeted_rep_stosb_limits_each_dirty_chunk` fixture
    // uses) admits exactly one STOSB iteration on the first call, whose budget also pays for
    // the initial decode fetch; a resumed call has no such fetch to pay for, so it may or may
    // not admit more than one iteration under the same cap. Either way, every call except the
    // last one that finally drains cx to 0 must yield at the chunk boundary, so the number of
    // calls made is the ground truth to check `brk_rep_resume` against, not a hardcoded count.
    let mut calls = 1;
    cpu.run_budgeted(&mut bus, 14).unwrap();
    while cpu.registers.eip == ORIGIN {
        cpu.run_budgeted(&mut bus, 14).unwrap();
        calls += 1;
    }
    assert!(
        calls >= 2,
        "cx=4 at cap=14 must take more than one call to drain"
    );
    assert_eq!(
        &bus.memory[0x400..0x404],
        &[0x5a; 4],
        "the REP STOSB ran to completion"
    );

    let p = cpu.perf_counters();
    assert_eq!(
        p.brk_rep_resume,
        calls - 1,
        "every call except the one that finally drains cx to 0 must yield at the chunk boundary"
    );
    assert_eq!(
        ledger_identity_gap(p),
        0,
        "straight_line_runs must equal the sum of every named break reason: got {p:#?}"
    );
}

/// `brk_fatal`, in isolation: a guest `#DE` (DIV by zero) whose delivery itself cannot proceed
/// because the IDT does not cover vector 0. `deliver_exception_body` reports that as
/// `CpuError::IdtLimit` (`control.rs`), which `finish_instruction`'s exception arm returns
/// directly rather than delivering -- exactly the "propagated hard `CpuError`" the ledger's old
/// comment excused without counting. It propagates out of `run_budgeted_inner`'s first `?` and
/// out of `run_budgeted` itself.
#[test]
fn brk_fatal_counts_a_propagated_hard_cpu_error() {
    let mut memory = vec![0u8; 16];
    memory[0] = 0xf6;
    memory[1] = 0xf3; // DIV BL
    let mut cpu = CpuGsw::default();
    cpu.control.cr0 |= CR0_PE;
    cpu.registers
        .set_segment(SegmentIndex::Cs, flat_pm_segment(0x08, 0x9b));
    cpu.registers
        .set_segment(SegmentIndex::Ds, flat_pm_segment(0x10, 0x93));
    cpu.registers
        .set_segment(SegmentIndex::Ss, flat_pm_segment(0x10, 0x93));
    cpu.registers.eip = 0;
    cpu.registers.set_ebx(0); // bl = 0: DIV BL divides by zero
    // The IDT does not cover even vector 0 (needs limit >= 7): delivery fails closed instead
    // of reading a gate.
    cpu.idtr.limit = 0;
    let mut bus = TestBus::with_memory(memory);

    let result = cpu.run_budgeted(&mut bus, 10_000);

    // TripleFault, not IdtLimit, since 2026-08-30: an out-of-limit vector is a
    // deliverable #GP(vector*8+2) per the PRM, so #DE escalates to #GP, which is
    // also out of limit here, then to #DF, and delivering THAT is the PRM's
    // shutdown. The point of this test is unchanged -- a hard CpuError
    // propagates out of run_budgeted and brk_fatal counts it -- only the way the
    // fixture reaches one is now the architectural chain.
    assert!(
        matches!(result, Err(CpuError::TripleFault { .. })),
        "{result:?}"
    );
    let p = cpu.perf_counters();
    assert_eq!(
        p.brk_fatal, 1,
        "the propagated IdtLimit error must be counted, not excused"
    );
    assert_eq!(p.straight_line_runs, 1);
    assert_eq!(
        ledger_identity_gap(p),
        0,
        "a single fatal run: brk_fatal alone must account for it: got {p:#?}"
    );
}

/// The real deliverable: the run-end ledger's identity, asserted EXACTLY, on one CPU driven
/// through several distinct break reasons in the SAME session -- decode-cache miss, a port
/// step-break, an interrupt-serviceable transition (STI's one-instruction shadow expiring),
/// the scaled-clock cap, HLT, and a budgeted REP's chunk yield.
///
/// This is the test that must fail before the fix: without `brk_rep_resume`, the REP segment's
/// three chunk-boundary runs are straight_line_runs with no matching brk_* increment, so the sum
/// undercounts by exactly 3. A workload that never reached the REP-resume break would already
/// close (every OTHER break reason was already counted correctly), which is why the REP segment
/// is not optional here.
#[test]
fn run_end_ledger_identity_closes_across_mixed_break_reasons() {
    const DECODE_MISS_ORIGIN: u32 = 0x100;
    const STEP_ORIGIN: u32 = 0x110;
    const INTERRUPT_ORIGIN: u32 = 0x120;
    const CAP_ORIGIN: u32 = 0x130;
    const HALT_ORIGIN: u32 = 0x140;
    const REP_ORIGIN: u32 = 0x150;

    let mut memory = vec![0u8; 0x1000];
    memory[DECODE_MISS_ORIGIN as usize] = 0x90; // NOP; nothing after it is ever decoded
    memory[STEP_ORIGIN as usize..STEP_ORIGIN as usize + 2].copy_from_slice(&[0xe6, 0x20]); // OUT 0x20,AL
    memory[INTERRUPT_ORIGIN as usize..INTERRUPT_ORIGIN as usize + 2].copy_from_slice(&[0xfb, 0x90]); // STI; NOP
    memory[CAP_ORIGIN as usize..CAP_ORIGIN as usize + 4].copy_from_slice(&[0x40, 0x40, 0xeb, 0xfc]); // INC AX; INC AX; JMP $-4
    memory[HALT_ORIGIN as usize] = 0xf4; // HLT
    memory[REP_ORIGIN as usize..REP_ORIGIN as usize + 2].copy_from_slice(&[0xf3, 0xaa]); // REP STOSB

    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw386);
    for segment in [
        SegmentIndex::Cs,
        SegmentIndex::Ds,
        SegmentIndex::Es,
        SegmentIndex::Ss,
    ] {
        cpu.load_segment_real(segment, 0);
    }
    let mut bus = TestBus::with_memory(memory);
    bus.direct_page_clocks = true;
    bus.report_batch_clocks = true;

    // 1. brk_decode_or_branch: the NOP's continuation probes an address that was never decoded.
    cpu.registers.eip = DECODE_MISS_ORIGIN;
    cpu.run_budgeted(&mut bus, 10_000).unwrap();

    // 2. brk_step: OUT sets io_touched, which ends the run right after this one instruction.
    cpu.registers.eip = STEP_ORIGIN;
    cpu.write_reg16(Reg16::Ax, 0);
    cpu.run_budgeted(&mut bus, 10_000).unwrap();
    bus.io_touched = false; // host-harness reset between segments, mirroring a fresh batch

    // 3. brk_interrupt: IF starts clear. STI arms the one-instruction shadow; the shadow expires
    // on the very next instruction (the NOP), which is exactly where the transition fires.
    //
    // The NOP's address must already be in the decode cache before the measured run: a
    // continuation's decode-cache miss is screened BEFORE the instruction executes, so an
    // uncached NOP would end the run right there and the transition would never be checked.
    // Warming with two single steps runs STI and NOP for real (`cycle` never touches
    // `perf.brk_interrupt`, only `run_budgeted_inner` does), so IF and eip are reset by hand
    // afterwards to put the measured run back at the pre-STI state.
    cpu.registers.eip = INTERRUPT_ORIGIN;
    cpu.cycle(&mut bus).unwrap(); // warm STI's decode-cache line
    cpu.cycle(&mut bus).unwrap(); // warm NOP's decode-cache line
    cpu.registers.eip = INTERRUPT_ORIGIN;
    cpu.set_flag(FLAG_IF, false);
    assert!(!cpu.flag(FLAG_IF));
    cpu.run_budgeted(&mut bus, 10_000).unwrap();
    assert!(cpu.flag(FLAG_IF), "STI must have taken effect");

    // 4. brk_cap: a tight INC/INC/JMP loop runs until the scaled-clock cap fires. Warmed with
    // six single steps first (two full passes), exactly as `perf_counters_track_decode_hits_and_run_breaks`
    // does: on a cold cache the loop's own second instruction is a fresh address and the run
    // would end on a decode-cache miss instead of ever reaching the cap.
    cpu.registers.eip = CAP_ORIGIN;
    for _ in 0..6 {
        cpu.cycle(&mut bus).unwrap();
    }
    assert_eq!(
        cpu.registers.eip, CAP_ORIGIN,
        "two warm-up passes land back at the loop head"
    );
    cpu.run_budgeted(&mut bus, 10_000).unwrap();

    // 5. brk_halt: HLT.
    cpu.registers.eip = HALT_ORIGIN;
    let halted = cpu.run_budgeted(&mut bus, 10_000).unwrap();
    assert!(halted.halted);
    cpu.halted = false; // host-harness wake, mirroring the machine's own HLT-wake prologue

    // 6. brk_rep_resume: the same tightly-budgeted REP STOSB as the isolated fixture above.
    cpu.registers.eip = REP_ORIGIN;
    cpu.registers.set_eax(0x5a);
    cpu.registers.set_edi(0x400);
    cpu.registers.set_ecx(4);
    cpu.run_budgeted(&mut bus, 14).unwrap();
    while cpu.registers.eip == REP_ORIGIN {
        cpu.run_budgeted(&mut bus, 14).unwrap();
    }

    let p = cpu.perf_counters();
    assert!(p.brk_decode_or_branch > 0, "decode-miss segment: {p:#?}");
    assert!(p.brk_step > 0, "step-break segment: {p:#?}");
    assert!(p.brk_interrupt > 0, "interrupt-transition segment: {p:#?}");
    assert!(p.brk_cap > 0, "cap segment: {p:#?}");
    assert_eq!(p.brk_halt, 1, "halt segment: {p:#?}");
    assert!(p.brk_rep_resume > 0, "rep-resume segment: {p:#?}");
    assert_eq!(
        ledger_identity_gap(p),
        0,
        "straight_line_runs must equal the sum of every named break reason: got {p:#?}"
    );
}
