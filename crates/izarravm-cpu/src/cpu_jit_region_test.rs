// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const ENTRY: u32 = 0x100;
const NOP_STARTER: u32 = 0xff;
const COUNT_ADDR: usize = 0x400;
const PATCHER: u32 = 0x140;
const STEP_IMM: u32 = 0x0133_7c00;
const PATCHED_IMM: u32 = 0x0066_7c00;

/// The 51-byte R_DrawColumn loop, a HLT terminator at the fall-through, and the two-store
/// self-patcher at 0x140 that rewrites both `add ebp,imm32` immediates exactly the way
/// Doom's setup code does. The generic `build_block` must reproduce this exact 15-slot,
/// self-loop shape (kinds + count) so this whole differential suite still holds.
fn program() -> Vec<u8> {
    let mut m = vec![0u8; 0x1000];
    m[NOP_STARTER as usize] = 0x90;
    let loop_bytes: [u8; 0x33] = [
        0x8b, 0xcd, // mov ecx,ebp
        0x81, 0xc5, 0x00, 0x7c, 0x33, 0x01, // add ebp,STEP_IMM (imm at 0x104)
        0x88, 0x07, // mov [edi],al
        0xc1, 0xe9, 0x19, // shr ecx,25
        0x8b, 0xd5, // mov edx,ebp
        0x81, 0xc5, 0x00, 0x7c, 0x33, 0x01, // add ebp,STEP_IMM (imm at 0x111)
        0x88, 0x5f, 0x50, // mov [edi+0x50],bl
        0xc1, 0xea, 0x19, // shr edx,25
        0x8a, 0x04, 0x0e, // mov al,[esi+ecx]
        0x81, 0xc7, 0xa0, 0x00, 0x00, 0x00, // add edi,0xa0
        0x8a, 0x1c, 0x16, // mov bl,[esi+edx]
        0xff, 0x0d, 0x00, 0x04, 0x00, 0x00, // dec dword [0x400]
        0x8a, 0x00, // mov al,[eax]
        0x8a, 0x1b, // mov bl,[ebx]
        0x75, 0xcd, // jnz ENTRY (rel8 -0x33)
    ];
    m[ENTRY as usize..ENTRY as usize + 0x33].copy_from_slice(&loop_bytes);
    m[0x133] = 0xf4; // HLT at the loop fall-through
    // Patcher: mov dword [0x104],PATCHED_IMM ; mov dword [0x111],PATCHED_IMM ; HLT.
    let p = PATCHER as usize;
    m[p..p + 10].copy_from_slice(&[0xc7, 0x05, 0x04, 0x01, 0x00, 0x00, 0x00, 0x7c, 0x66, 0x00]);
    m[p + 10..p + 20]
        .copy_from_slice(&[0xc7, 0x05, 0x11, 0x01, 0x00, 0x00, 0x00, 0x7c, 0x66, 0x00]);
    m[p + 20] = 0xf4;
    // Texture bytes at 0x300..0x380 (indexed by ebp>>25) and the colormap they point
    // into at 0x200..0x280 (the double indirection [eax]/[ebx] after AL/BL replace the
    // low byte of 0x200).
    for i in 0..0x80usize {
        m[0x300 + i] = 0x20 + (i as u8 & 0x1f);
        m[0x200 + i] = 0x80 ^ (i as u8);
    }
    // Real-mode IVT vector 13 (#GP) -> 0:0xB00, HLT handler (the fault test).
    m[13 * 4..13 * 4 + 2].copy_from_slice(&0x0b00u16.to_le_bytes());
    m[0xb00] = 0xf4;
    m
}

/// `program()` in a `size`-byte buffer, for loops that advance edi (stride 0xa0) past the
/// 0x1000 the bare program occupies - hotness needs to run more iterations than that fits.
fn program_in(size: usize) -> Vec<u8> {
    let mut m = vec![0u8; size];
    let p = program();
    m[..p.len()].copy_from_slice(&p);
    m
}

fn fresh_cpu(ds_limit: u32) -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    let mut cs = cpu.registers.cs();
    cs.default_size_32 = true; // the shape is d=32 code
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    let mut ds = cpu.registers.segment(SegmentIndex::Ds);
    ds.limit = ds_limit;
    cpu.registers.set_segment(SegmentIndex::Ds, ds);
    cpu
}

/// Reset the guest to the canonical loop entry state with `count` iterations to run.
fn arm_loop(cpu: &mut CpuGsw, bus: &mut TestBus, count: u32) {
    cpu.registers.eip = NOP_STARTER;
    cpu.registers.set_esp(0x0700);
    cpu.write_gpr32(0, 0x200); // eax
    cpu.write_gpr32(1, 0); // ecx
    cpu.write_gpr32(2, 0); // edx
    cpu.write_gpr32(3, 0x200); // ebx
    cpu.write_gpr32(5, 0x0100_0000); // ebp
    cpu.write_gpr32(6, 0x300); // esi
    cpu.write_gpr32(7, 0x500); // edi
    bus.memory[COUNT_ADDR..COUNT_ADDR + 4].copy_from_slice(&count.to_le_bytes());
}

/// Drive `run_straight_line` (the machine batch seam) until a run halts. Returns the
/// per-call scaled clock totals so cap-boundary shapes can be compared A/B too.
fn drive_to_halt(cpu: &mut CpuGsw, bus: &mut TestBus, cap: u64) -> Vec<(u32, u32)> {
    let mut calls = Vec::new();
    for _ in 0..10_000 {
        let outcome = cpu.run_straight_line(bus, cap).expect("no hard bus error");
        calls.push((outcome.core_clocks, cpu.registers.eip));
        if outcome.halted {
            return calls;
        }
    }
    panic!("guest never halted");
}

/// Warm both CPUs identically (fills the decode cache), admit + stamp the region on
/// `jit` only, and assert the warm phases were identical.
fn warm_and_admit(
    interp: &mut CpuGsw,
    bus_i: &mut TestBus,
    jit: &mut CpuGsw,
    bus_j: &mut TestBus,
) -> std::num::NonZeroU32 {
    arm_loop(interp, bus_i, 2);
    arm_loop(jit, bus_j, 2);
    drive_to_halt(interp, bus_i, u64::MAX);
    drive_to_halt(jit, bus_j, u64::MAX);
    assert_eq!(interp, jit, "warm phases must match before admission");
    let idx = jit::block::try_admit(jit, ENTRY, true)
        .expect("the warmed decode cache builds the drawcolumn block");
    let region = jit.jit_regions.get_mut(idx).unwrap();
    assert_eq!(region.ctx.slots[1].insn.imm, STEP_IMM);
    assert_eq!(region.ctx.slots[5].insn.imm, STEP_IMM);
    jit.decode_cache.stamp_region(ENTRY, true, idx);
    idx
}

fn assert_identical(interp: &CpuGsw, bus_i: &TestBus, jit_cpu: &CpuGsw, bus_j: &TestBus) {
    assert_eq!(interp, jit_cpu, "architectural + clock state diverged");
    assert_eq!(
        interp.elapsed_clocks, jit_cpu.elapsed_clocks,
        "elapsed guest clocks diverged"
    );
    assert_eq!(
        interp.timing_rem, jit_cpu.timing_rem,
        "scale remainder diverged"
    );
    assert_eq!(bus_i.memory, bus_j.memory, "guest memory diverged");
    assert_eq!(
        bus_i.trace.cycles(),
        bus_j.trace.cycles(),
        "bus cycle trace diverged"
    );
    let (pi, pj) = (interp.perf_counters(), jit_cpu.perf_counters());
    assert_eq!(
        pi.instructions, pj.instructions,
        "retired instruction count diverged"
    );
    assert_eq!(
        (pi.brk_cap, pi.brk_step, pi.brk_halt, pi.brk_interrupt),
        (pj.brk_cap, pj.brk_step, pj.brk_halt, pj.brk_interrupt),
        "run break attribution diverged"
    );
}

#[test]
fn switching_between_386_modes_clears_regions_stamps_and_remainders() {
    let mut interp = fresh_cpu(u32::MAX);
    let mut jit_cpu = fresh_cpu(u32::MAX);
    interp.set_mode(GswMode::Gsw386);
    jit_cpu.set_mode(GswMode::Gsw386);
    let mut bus_i = TestBus::with_memory(program());
    let mut bus_j = TestBus::with_memory(program());
    let region = warm_and_admit(&mut interp, &mut bus_i, &mut jit_cpu, &mut bus_j);

    assert_eq!(jit_cpu.jit_regions.len(), 1);
    assert_eq!(jit_cpu.decode_cache.region_at(ENTRY, true), Some(region));
    let mode_key = jit_cpu.jit_mode_key();
    jit_cpu.timing_rem = 4;
    jit_cpu.fp_rem = 7;
    jit_cpu.jit_regions.set_auto_admit(true);

    jit_cpu.set_mode(GswMode::Gsw386Slow);

    assert_ne!(jit_cpu.jit_mode_key(), mode_key);
    assert_eq!(jit_cpu.jit_regions.len(), 0);
    assert!(jit_cpu.jit_regions.auto_admit());
    assert_eq!(jit_cpu.decode_cache.region_at(ENTRY, true), None);
    assert_eq!(jit_cpu.timing_rem, 0);
    assert_eq!(jit_cpu.fp_rem, 0);
}

#[test]
fn region_run_is_byte_identical_to_the_interpreter() {
    let mut interp = fresh_cpu(0xffff);
    let mut jit_cpu = fresh_cpu(0xffff);
    let mut bus_i = TestBus::with_memory(program());
    let mut bus_j = TestBus::with_memory(program());
    warm_and_admit(&mut interp, &mut bus_i, &mut jit_cpu, &mut bus_j);

    arm_loop(&mut interp, &mut bus_i, 8);
    arm_loop(&mut jit_cpu, &mut bus_j, 8);
    let calls_i = drive_to_halt(&mut interp, &mut bus_i, u64::MAX);
    let calls_j = drive_to_halt(&mut jit_cpu, &mut bus_j, u64::MAX);

    assert_eq!(calls_i, calls_j, "per-run outcomes diverged");
    assert_identical(&interp, &bus_i, &jit_cpu, &bus_j);
    let perf = jit_cpu.perf_counters();
    assert!(perf.jit_region_entries > 0, "the region never executed");
    assert!(
        perf.jit_region_insns >= 8 * 15,
        "the region should have retired the loop's instructions, got {}",
        perf.jit_region_insns
    );
    assert_eq!(interp.perf_counters().jit_region_entries, 0);
}

#[test]
fn region_breaks_at_the_interpreter_cap_boundary() {
    let mut interp = fresh_cpu(0xffff);
    let mut jit_cpu = fresh_cpu(0xffff);
    let mut bus_i = TestBus::with_memory(program());
    let mut bus_j = TestBus::with_memory(program());
    warm_and_admit(&mut interp, &mut bus_i, &mut jit_cpu, &mut bus_j);

    // Small caps force many mid-loop breaks; every break must land both executions on
    // the same eip with the same charged total (compared per call via drive_to_halt's
    // outcome log). Odd caps exercise the scale-remainder threading too.
    for cap in [7u64, 13, 50] {
        arm_loop(&mut interp, &mut bus_i, 14);
        arm_loop(&mut jit_cpu, &mut bus_j, 14);
        let calls_i = drive_to_halt(&mut interp, &mut bus_i, cap);
        let calls_j = drive_to_halt(&mut jit_cpu, &mut bus_j, cap);
        assert_eq!(calls_i, calls_j, "cap {cap}: break boundaries diverged");
        assert_identical(&interp, &bus_i, &jit_cpu, &bus_j);
    }
    assert!(jit_cpu.perf_counters().jit_region_entries > 0);
}

/// v2's inline slots (mov/add/shr) set gpr and flags natively; the brief flags
/// flag-state equality after EVERY exit (incl. mid-iteration) as the hard correctness property.
/// This test forces a cap-boundary exit at several points across the loop and compares the
/// MATERIALIZED eflags (not just CpuGsw equality, but the actual `eflags()` value that
/// resolves any pending descriptor the inline ADD left behind) between interpreter and JIT.
/// A divergence here would mean the inline ADD's lazy descriptor or the inline SHR's eager
/// materialization differs from the interpreter at the exit eip.
#[test]
fn region_inline_flag_state_matches_after_cap_exits() {
    let mut interp = fresh_cpu(0xffff);
    let mut jit_cpu = fresh_cpu(0xffff);
    let mut bus_i = TestBus::with_memory(program());
    let mut bus_j = TestBus::with_memory(program());
    warm_and_admit(&mut interp, &mut bus_i, &mut jit_cpu, &mut bus_j);
    // Run several iterations with caps that land exits at different slots, then compare the
    // materialized eflags at every break.
    for cap in [4u64, 8, 16, 31, 64, 100] {
        arm_loop(&mut interp, &mut bus_i, 14);
        arm_loop(&mut jit_cpu, &mut bus_j, 14);
        drive_to_halt(&mut interp, &mut bus_i, cap);
        drive_to_halt(&mut jit_cpu, &mut bus_j, cap);
        // The materialized eflags resolve any pending descriptor the inline add/shr left.
        assert_eq!(
            interp.eflags(),
            jit_cpu.eflags(),
            "cap {cap}: materialized eflags diverged after inline slots"
        );
        // And the raw pending-flag descriptor (if any) must match too.
        assert_eq!(
            interp.pending_flags, jit_cpu.pending_flags,
            "cap {cap}: pending flag descriptor diverged"
        );
    }
    assert!(jit_cpu.perf_counters().jit_region_entries > 0);
}

#[test]
fn region_fault_mid_loop_delivers_identically() {
    // DS limit 0x5FF: the third iteration's `mov [edi],al` (edi = 0x640) raises #GP,
    // mid-region, on the write half of the unrolled pair. Both executions must rewind,
    // deliver through IVT 13, and halt in the handler with identical state.
    let mut interp = fresh_cpu(0x5ff);
    let mut jit_cpu = fresh_cpu(0x5ff);
    let mut bus_i = TestBus::with_memory(program());
    let mut bus_j = TestBus::with_memory(program());
    warm_and_admit(&mut interp, &mut bus_i, &mut jit_cpu, &mut bus_j);

    arm_loop(&mut interp, &mut bus_i, 100);
    arm_loop(&mut jit_cpu, &mut bus_j, 100);
    let calls_i = drive_to_halt(&mut interp, &mut bus_i, u64::MAX);
    let calls_j = drive_to_halt(&mut jit_cpu, &mut bus_j, u64::MAX);

    assert_eq!(calls_i, calls_j);
    assert_identical(&interp, &bus_i, &jit_cpu, &bus_j);
    assert_eq!(
        jit_cpu.registers.eip, 0xb01,
        "both sides must halt inside the #GP handler"
    );
    assert!(jit_cpu.perf_counters().jit_region_entries > 0);
}

#[test]
fn smc_repatch_restamps_with_fresh_immediates() {
    let mut interp = fresh_cpu(0xffff);
    let mut jit_cpu = fresh_cpu(0xffff);
    let mut bus_i = TestBus::with_memory(program());
    let mut bus_j = TestBus::with_memory(program());
    let idx = warm_and_admit(&mut interp, &mut bus_i, &mut jit_cpu, &mut bus_j);

    // Run the loop with the region live, then execute the guest self-patcher: its
    // stores hit watched code bytes, bump the decode generation, and kill the stamp.
    arm_loop(&mut interp, &mut bus_i, 3);
    arm_loop(&mut jit_cpu, &mut bus_j, 3);
    drive_to_halt(&mut interp, &mut bus_i, u64::MAX);
    drive_to_halt(&mut jit_cpu, &mut bus_j, u64::MAX);
    let entries_before = jit_cpu.perf_counters().jit_region_entries;
    assert!(entries_before > 0);
    for (cpu, bus) in [(&mut interp, &mut bus_i), (&mut jit_cpu, &mut bus_j)] {
        cpu.registers.eip = PATCHER;
        drive_to_halt(cpu, bus, u64::MAX);
    }
    assert_eq!(bus_j.memory[0x104..0x108], PATCHED_IMM.to_le_bytes());

    // Re-warm interpreted (the dead line means no region runs), then re-admit: the
    // matcher must find the SAME region and refresh its slot table wholesale, patched
    // immediates riding along in the fresh decodes.
    arm_loop(&mut interp, &mut bus_i, 2);
    arm_loop(&mut jit_cpu, &mut bus_j, 2);
    drive_to_halt(&mut interp, &mut bus_i, u64::MAX);
    drive_to_halt(&mut jit_cpu, &mut bus_j, u64::MAX);
    assert_eq!(
        jit_cpu.perf_counters().jit_region_entries,
        entries_before,
        "a dead stamp must keep the region cold until re-admission"
    );
    let idx2 =
        jit::block::try_admit(&mut jit_cpu, ENTRY, true).expect("the re-warmed block still builds");
    assert_eq!(idx2, idx, "re-admission must reuse the installed region");
    jit_cpu.decode_cache.stamp_region(ENTRY, true, idx2);
    {
        let region = jit_cpu.jit_regions.get_mut(idx2).unwrap();
        assert_eq!(region.ctx.slots[1].insn.imm, PATCHED_IMM);
        assert_eq!(region.ctx.slots[5].insn.imm, PATCHED_IMM);
    }

    arm_loop(&mut interp, &mut bus_i, 6);
    arm_loop(&mut jit_cpu, &mut bus_j, 6);
    let calls_i = drive_to_halt(&mut interp, &mut bus_i, u64::MAX);
    let calls_j = drive_to_halt(&mut jit_cpu, &mut bus_j, u64::MAX);
    assert_eq!(calls_i, calls_j);
    assert_identical(&interp, &bus_i, &jit_cpu, &bus_j);
    assert!(jit_cpu.perf_counters().jit_region_entries > entries_before);
}

#[test]
fn profiling_falls_back_to_the_interpreter() {
    let mut interp = fresh_cpu(0xffff);
    let mut jit_cpu = fresh_cpu(0xffff);
    let mut bus_i = TestBus::with_memory(program());
    let mut bus_j = TestBus::with_memory(program());
    warm_and_admit(&mut interp, &mut bus_i, &mut jit_cpu, &mut bus_j);

    jit_cpu.profile.enable(1_000_000);
    arm_loop(&mut interp, &mut bus_i, 4);
    arm_loop(&mut jit_cpu, &mut bus_j, 4);
    drive_to_halt(&mut interp, &mut bus_i, u64::MAX);
    drive_to_halt(&mut jit_cpu, &mut bus_j, u64::MAX);

    assert_eq!(
        jit_cpu.perf_counters().jit_region_entries,
        0,
        "profiled runs must not enter the region (per-instruction sampling)"
    );
    assert_eq!(interp.registers, jit_cpu.registers);
    assert_eq!(bus_i.memory, bus_j.memory);
}

/// A `TestBus` wrapper adding the two machine-bus behaviors this port-free loop can never
/// raise on `TestBus` itself: `requires_step_break` arms on the Nth memory write
/// (standing in for the `io_touched` edge; the driver clears it per run like the machine
/// batch loop), and `in_batch_scaled_bus_clocks` reports a synthetic monotonic count (2
/// per bus access) so the run cap's bus-growth term is live, as it is at 486/586 on the
/// real machine bus.
struct InstrumentedBus {
    inner: TestBus,
    writes_until_break: u32,
    armed: bool,
    bus_clocks: u64,
}

impl InstrumentedBus {
    fn new(memory: Vec<u8>) -> Self {
        Self {
            inner: TestBus::with_memory(memory),
            writes_until_break: u32::MAX, // step break disarmed
            armed: false,
            bus_clocks: 0,
        }
    }
}

impl CpuBus for InstrumentedBus {
    fn read_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<u32, BusError> {
        self.bus_clocks += 2;
        self.inner.read_memory(address, width, kind)
    }
    fn write_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        value: u32,
        kind: BusAccessKind,
    ) -> Result<(), BusError> {
        self.bus_clocks += 2;
        if self.writes_until_break > 0 {
            self.writes_until_break -= 1;
            if self.writes_until_break == 0 {
                self.armed = true;
            }
        }
        self.inner.write_memory(address, width, value, kind)
    }
    fn prefetch_memory(&mut self, address: u32, out: &mut [u8]) -> Result<usize, BusError> {
        self.inner.prefetch_memory(address, out)
    }
    fn charge_instruction_fetch(&mut self, address: u32) -> Result<(), BusError> {
        self.bus_clocks += 2;
        self.inner.charge_instruction_fetch(address)
    }
    fn in_batch_scaled_bus_clocks(&self) -> u64 {
        self.bus_clocks
    }
    fn read_io(
        &mut self,
        port: u16,
        width: BusWidth,
        core_clocks_so_far: u64,
        cpu_is_ring0_pm: bool,
    ) -> Result<u32, BusError> {
        self.inner
            .read_io(port, width, core_clocks_so_far, cpu_is_ring0_pm)
    }
    fn write_io(
        &mut self,
        port: u16,
        width: BusWidth,
        value: u32,
        core_clocks_so_far: u64,
        cpu_is_ring0_pm: bool,
    ) -> Result<(), BusError> {
        self.inner
            .write_io(port, width, value, core_clocks_so_far, cpu_is_ring0_pm)
    }
    fn interrupt_acknowledge(&mut self, vector: u8, ax: u16) -> Result<(), BusError> {
        self.inner.interrupt_acknowledge(vector, ax)
    }
    fn requires_step_break(&self) -> bool {
        self.armed || self.inner.requires_step_break()
    }
}

#[test]
fn region_breaks_at_the_step_break_boundary() {
    // Arm the break on the 5th guest write: mid-iteration-2, on the region's slot-6
    // store. Both executions must end that run at exactly that instruction boundary.
    let run = |admit: bool| {
        let mut cpu = fresh_cpu(0xffff);
        let mut bus = InstrumentedBus::new(program());
        arm_loop(&mut cpu, &mut bus.inner, 2);
        for _ in 0..1000 {
            if cpu.run_straight_line(&mut bus, u64::MAX).unwrap().halted {
                break;
            }
        }
        if admit {
            let idx = jit::block::try_admit(&mut cpu, ENTRY, true).unwrap();
            cpu.decode_cache.stamp_region(ENTRY, true, idx);
        }
        arm_loop(&mut cpu, &mut bus.inner, 6);
        bus.writes_until_break = 5;
        let mut boundaries = Vec::new();
        for _ in 0..1000 {
            bus.armed = false; // the machine batch loop clears io_touched per batch
            let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
            boundaries.push((outcome.core_clocks, cpu.registers.eip));
            if outcome.halted {
                break;
            }
        }
        (boundaries, cpu, bus)
    };
    let (bounds_i, cpu_i, bus_i) = run(false);
    let (bounds_j, cpu_j, bus_j) = run(true);
    assert_eq!(bounds_i, bounds_j, "step-break boundaries diverged");
    assert_eq!(cpu_i, cpu_j);
    assert_eq!(bus_i.inner.memory, bus_j.inner.memory);
    assert_eq!(bus_i.inner.trace.cycles(), bus_j.inner.trace.cycles());
    assert!(cpu_j.perf_counters().jit_region_entries > 0);
}

#[test]
fn cap_bus_growth_term_breaks_identically_on_a_clock_reporting_bus() {
    // The run cap check adds the bus's in-batch scaled clock GROWTH to the core total
    // (nonzero on the real 486/586 machine bus). With the synthetic 2-clocks-per-access
    // counter live, the region's per-slot cap check must break at exactly the
    // interpreter's instruction boundary.
    let run = |admit: bool, cap: u64| {
        let mut cpu = fresh_cpu(0xffff);
        let mut bus = InstrumentedBus::new(program());
        arm_loop(&mut cpu, &mut bus.inner, 2);
        for _ in 0..1000 {
            if cpu.run_straight_line(&mut bus, u64::MAX).unwrap().halted {
                break;
            }
        }
        if admit {
            let idx = jit::block::try_admit(&mut cpu, ENTRY, true).unwrap();
            cpu.decode_cache.stamp_region(ENTRY, true, idx);
        }
        arm_loop(&mut cpu, &mut bus.inner, 10);
        let mut boundaries = Vec::new();
        for _ in 0..10_000 {
            let outcome = cpu.run_straight_line(&mut bus, cap).unwrap();
            boundaries.push((outcome.core_clocks, cpu.registers.eip, bus.bus_clocks));
            if outcome.halted {
                break;
            }
        }
        (boundaries, cpu, bus)
    };
    for cap in [60u64, 145, 400] {
        let (bounds_i, cpu_i, bus_i) = run(false, cap);
        let (bounds_j, cpu_j, bus_j) = run(true, cap);
        assert_eq!(
            bounds_i, bounds_j,
            "cap {cap}: bus-growth boundaries diverged"
        );
        assert_eq!(cpu_i, cpu_j);
        assert_eq!(bus_i.inner.trace.cycles(), bus_j.inner.trace.cycles());
        assert!(cpu_j.perf_counters().jit_region_entries > 0);
    }
}

#[test]
fn narrow_smc_kills_only_the_covering_lines() {
    let mut cpu = fresh_cpu(0xffff);
    let mut bus = TestBus::with_memory(program());
    arm_loop(&mut cpu, &mut bus, 2);
    drive_to_halt(&mut cpu, &mut bus, u64::MAX);
    assert!(cpu.decode_cache.line_live(ENTRY, true));
    assert!(cpu.decode_cache.line_live(0x102, true));
    let inval_before = cpu.perf_counters().decode_inval_smc;

    // The guest self-patcher writes the two imm32s at 0x104/0x111: covering lines
    // (0x102, 0x10f) die individually; every other loop line survives, and no
    // whole-cache flush happens.
    cpu.registers.eip = PATCHER;
    drive_to_halt(&mut cpu, &mut bus, u64::MAX);

    assert_eq!(cpu.perf_counters().decode_inval_smc, inval_before);
    assert!(cpu.perf_counters().smc_narrow_kills >= 2);
    assert!(
        !cpu.decode_cache.line_live(0x102, true),
        "covering line must die"
    );
    assert!(
        !cpu.decode_cache.line_live(0x10f, true),
        "covering line must die"
    );
    assert!(
        cpu.decode_cache.line_live(ENTRY, true),
        "neighbor must survive"
    );
    assert!(
        cpu.decode_cache.line_live(0x108, true),
        "neighbor must survive"
    );
    assert!(
        cpu.decode_cache.line_live(0x131, true),
        "neighbor must survive"
    );
}

#[test]
fn narrow_smc_falls_back_globally_on_an_aliased_page() {
    // Two linear pages decoding through the same physical page make the
    // physical-to-linear reconstruction ambiguous: narrow_invalidate must refuse.
    let mut cpu = fresh_cpu(0xffff);
    let mut bus = TestBus::with_memory(program());
    arm_loop(&mut cpu, &mut bus, 2);
    drive_to_halt(&mut cpu, &mut bus, u64::MAX);
    let insn = cpu.decode_cache.get(ENTRY, true).unwrap();
    // A second mapping: linear 0x5100 claims the same physical 0x100.
    cpu.decode_cache.put(0x5100, insn, true, ENTRY);
    assert!(
        cpu.decode_cache.narrow_invalidate(ENTRY).is_none(),
        "an aliased physical page must force the global flush"
    );
}

#[test]
fn narrow_smc_falls_back_globally_on_a_straddling_instruction() {
    let mut cpu = fresh_cpu(0xffff);
    let mut bus = TestBus::with_memory(program());
    arm_loop(&mut cpu, &mut bus, 2);
    drive_to_halt(&mut cpu, &mut bus, u64::MAX);
    let insn = cpu.decode_cache.get(0x102, true).unwrap(); // 6-byte add
    // Pretend it was decoded straddling the page edge at 0xffe: both pages flag.
    cpu.decode_cache.put(0xffe, insn, true, 0xffe);
    assert!(cpu.decode_cache.narrow_invalidate(0xffe).is_none());
    assert!(cpu.decode_cache.narrow_invalidate(0x1001).is_none());
}

#[test]
fn builder_admits_a_different_but_valid_loop_shape() {
    // Same program with the first SHR count byte changed (0x19 -> 0x18): a different but
    // still valid continuable self-loop. The old matcher pinned the exact drawcolumn shape
    // and rejected this; the generic builder admits ANY continuable basic block, so it now
    // compiles it as a 15-slot self-loop (the point of the generalization).
    let mut memory = program();
    memory[0x10c] = 0x18;
    let mut cpu = fresh_cpu(0xffff);
    let mut bus = TestBus::with_memory(memory);
    arm_loop(&mut cpu, &mut bus, 2);
    drive_to_halt(&mut cpu, &mut bus, u64::MAX);
    let idx = jit::block::try_admit(&mut cpu, ENTRY, true)
        .expect("a valid continuable self-loop must build");
    let region = cpu.jit_regions.get_mut(idx).unwrap();
    assert_eq!(
        region.ctx.slots.len(),
        15,
        "same shape, different shift count"
    );
    assert!(region.is_loop, "the back-edge still targets the entry");
    assert_eq!(
        region.ctx.slots[3].insn.imm, 24,
        "the mutated shift count rode along into the slot"
    );
}

#[test]
fn cold_decode_lines_defer_admission() {
    // Before any execution the decode cache is empty: admission must return None
    // rather than reading guest memory itself.
    let mut cpu = fresh_cpu(0xffff);
    let mut bus = TestBus::with_memory(program());
    arm_loop(&mut cpu, &mut bus, 1);
    assert!(jit::block::try_admit(&mut cpu, ENTRY, true).is_none());
    drive_to_halt(&mut cpu, &mut bus, u64::MAX);
    assert!(jit::block::try_admit(&mut cpu, ENTRY, true).is_some());
}

/// Compare native inline-slot bookkeeping with the `region_inline_slot` call
/// path. The test first checks bit identity on the drawcolumn, then times both
/// paths through `run_straight_line`.
///   cargo test -j8 -p izarravm-cpu --release --features jit s2_native_bookkeeping -- --ignored --nocapture
#[test]
#[ignore]
fn s2_native_bookkeeping_prototype() {
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    // Verify bit identity against the interpreter.
    jit::block::NATIVE_BOOKKEEPING.store(1, Ordering::Relaxed);
    {
        let mut interp = fresh_cpu(0xffff);
        let mut jit_cpu = fresh_cpu(0xffff);
        let mut bus_i = TestBus::with_memory(program());
        let mut bus_j = TestBus::with_memory(program());
        warm_and_admit(&mut interp, &mut bus_i, &mut jit_cpu, &mut bus_j);
        arm_loop(&mut interp, &mut bus_i, 8);
        arm_loop(&mut jit_cpu, &mut bus_j, 8);
        let ci = drive_to_halt(&mut interp, &mut bus_i, u64::MAX);
        let cj = drive_to_halt(&mut jit_cpu, &mut bus_j, u64::MAX);
        assert_eq!(ci, cj, "native bookkeeping: per-run outcomes diverged");
        assert_identical(&interp, &bus_i, &jit_cpu, &bus_j);
        assert!(jit_cpu.perf_counters().jit_region_entries > 0);
    }
    jit::block::NATIVE_BOOKKEEPING.store(0, Ordering::Relaxed);
    eprintln!("native bookkeeping BIT-IDENTICAL on the drawcolumn (region vs interpreter)");

    // Time both paths through run_straight_line. Big memory lets EDI advance across many
    // iterations; tracing off (representative of production, not the traced test bus).
    const ITERS: u32 = 200_000;
    let time_variant = |mode: u8| -> f64 {
        jit::block::NATIVE_BOOKKEEPING.store(mode, Ordering::Relaxed);
        let mut m = vec![0u8; 64 << 20];
        let p = program();
        m[..p.len()].copy_from_slice(&p);
        let mut cpu = fresh_cpu(0xffff_ffff);
        let mut bus = TestBus::with_memory(m);
        bus.direct_pages_enabled = true; // host-pointer cache path, like production
        bus.trace.set_tracing_mode(izarravm_bus::TracingMode::Off);
        arm_loop(&mut cpu, &mut bus, 2);
        drive_to_halt(&mut cpu, &mut bus, u64::MAX);
        let idx = jit::block::try_admit(&mut cpu, ENTRY, true).expect("admit");
        cpu.decode_cache.stamp_region(ENTRY, true, idx);
        let mut best = f64::MAX;
        for _ in 0..7 {
            arm_loop(&mut cpu, &mut bus, ITERS);
            let t = Instant::now();
            drive_to_halt(&mut cpu, &mut bus, u64::MAX);
            best = best.min(t.elapsed().as_secs_f64() / (ITERS as f64 * 15.0) * 1e9);
        }
        best
    };
    let call = time_variant(0);
    let native = time_variant(1);
    jit::block::NATIVE_BOOKKEEPING.store(0, Ordering::Relaxed);
    eprintln!("\n=== Native-bookkeeping A/B (drawcolumn, ns/insn, best of 7) ===");
    eprintln!("0. call bookkeeping (today's region) : {call:.3} ns/insn");
    eprintln!(
        "1. native bookkeeping (emit_native)  : {native:.3} ns/insn  ({:.2}x)",
        call / native
    );
    eprintln!("   -> the inline bookkeeping CALL is ~2 ns/slot x 8 inline slots ~= 16 ns of a");
    eprintln!(
        "      ~{:.0} ns iteration (~1%); even a fully-native version cannot move the",
        call * 15.0
    );
    eprintln!("      drawcolumn. Most cost remains in the 7 memory-slot dispatches.");
    eprintln!("=== end A/B ===\n");
}

/// The region trampoline must stay byte-identical to the interpreter on the host-pointer
/// direct-page path too: with `direct_pages_enabled` the bus hands out host pages, so data
/// accesses are cached derefs (`data_read_pages`/`data_write_pages`) rather than the slow
/// `read_memory_direct` fallback the rest of the differential suite exercises. This is the
/// production-representative memory path (MachineBus always hands out direct pages).
#[test]
fn region_is_byte_identical_on_the_direct_page_path() {
    let mut interp = fresh_cpu(0xffff);
    let mut jit_cpu = fresh_cpu(0xffff);
    let mut bus_i = TestBus::with_memory(program());
    let mut bus_j = TestBus::with_memory(program());
    bus_i.direct_pages_enabled = true;
    bus_j.direct_pages_enabled = true;
    warm_and_admit(&mut interp, &mut bus_i, &mut jit_cpu, &mut bus_j);
    arm_loop(&mut interp, &mut bus_i, 8);
    arm_loop(&mut jit_cpu, &mut bus_j, 8);
    let ci = drive_to_halt(&mut interp, &mut bus_i, u64::MAX);
    let cj = drive_to_halt(&mut jit_cpu, &mut bus_j, u64::MAX);
    assert_eq!(ci, cj, "direct-page path: per-run outcomes diverged");
    assert_identical(&interp, &bus_i, &jit_cpu, &bus_j);
    assert!(jit_cpu.perf_counters().jit_region_entries > 0);
    assert!(
        jit_cpu.perf_counters().direct_page_hits > 0,
        "the direct-page (host-pointer) path was exercised"
    );
}

/// Baseline drawcolumn region throughput on a production-representative harness (the one-op
/// instruction-fetch charge and host-pointer direct pages, both matching MachineBus). The
/// reference for native-template A/B measurements. On this harness the
/// drawcolumn is about 165 ns per iteration with no single dominant cost.
///   cargo test -j8 -p izarravm-cpu --release --features jit drawcolumn_region_baseline -- --ignored --nocapture
#[test]
#[ignore]
fn drawcolumn_region_baseline() {
    use std::time::Instant;
    const ITERS: u32 = 200_000;
    let mut m = vec![0u8; 64 << 20];
    let p = program();
    m[..p.len()].copy_from_slice(&p);
    let mut cpu = fresh_cpu(0xffff_ffff);
    let mut bus = TestBus::with_memory(m);
    bus.direct_pages_enabled = true; // host-pointer cache path, like production
    bus.trace.set_tracing_mode(izarravm_bus::TracingMode::Off);
    arm_loop(&mut cpu, &mut bus, 2);
    drive_to_halt(&mut cpu, &mut bus, u64::MAX);
    let idx = jit::block::try_admit(&mut cpu, ENTRY, true).expect("admit");
    cpu.decode_cache.stamp_region(ENTRY, true, idx);
    let mut best = f64::MAX;
    for _ in 0..7 {
        arm_loop(&mut cpu, &mut bus, ITERS);
        let t = Instant::now();
        drive_to_halt(&mut cpu, &mut bus, u64::MAX);
        best = best.min(t.elapsed().as_secs_f64() / ITERS as f64 * 1e9);
    }
    eprintln!("drawcolumn region baseline: {best:.0} ns/iter (15 insns), representative harness");
}

/// Cost-fold native-LOAD smoke A/B on the drawcolumn. The fold fires only with
/// flat DS, no paging, and direct pages. The anchors use paging, so the fold is
/// inert there. Times the same drawcolumn with the fold off and on. Run filtered
/// (this sets the process-global FOLD_TIMING; a concurrent flat-DS #[ignore] bench would see it):
///   cargo test -p izarravm-cpu --release --features jit drawcolumn_region_fold_ab -- --ignored --nocapture
#[test]
#[ignore]
fn drawcolumn_region_fold_ab() {
    use std::sync::atomic::Ordering;
    use std::time::Instant;
    const ITERS: u32 = 200_000;
    let time_variant = |fold: bool| -> (f64, u64, u64) {
        jit::block::FOLD_TIMING.store(fold, Ordering::Relaxed);
        let mut m = vec![0u8; 64 << 20];
        let p = program();
        m[..p.len()].copy_from_slice(&p);
        let mut cpu = fresh_cpu(0xffff_ffff);
        let mut bus = TestBus::with_memory(m);
        bus.direct_pages_enabled = true;
        bus.trace.set_tracing_mode(izarravm_bus::TracingMode::Off);
        arm_loop(&mut cpu, &mut bus, 2);
        drive_to_halt(&mut cpu, &mut bus, u64::MAX);
        let idx = jit::block::try_admit(&mut cpu, ENTRY, true).expect("admit");
        cpu.decode_cache.stamp_region(ENTRY, true, idx);
        let mut best = f64::MAX;
        for _ in 0..7 {
            arm_loop(&mut cpu, &mut bus, ITERS);
            let t = Instant::now();
            drive_to_halt(&mut cpu, &mut bus, u64::MAX);
            best = best.min(t.elapsed().as_secs_f64() / ITERS as f64 * 1e9);
        }
        let pc = cpu.perf_counters();
        (best, pc.jit_native_load_hits, pc.jit_native_store_hits)
    };
    let (off, off_ld, off_st) = time_variant(false);
    let (on, on_ld, on_st) = time_variant(true);
    jit::block::FOLD_TIMING.store(false, Ordering::Relaxed);
    eprintln!("\n=== drawcolumn cost-fold native LOAD+STORE+ALU A/B (ns/iter, best of 7) ===");
    eprintln!("fold OFF: {off:.0} ns/iter  (load_hits={off_ld}, store_hits={off_st})");
    eprintln!(
        "fold ON : {on:.0} ns/iter  (load_hits={on_ld}, store_hits={on_st})  ({:.2}x)",
        off / on
    );
    assert_eq!(
        (off_ld, off_st),
        (0, 0),
        "fold-off must run no native slots"
    );
    assert!(on_ld > 0, "fold-on must run the native LOAD slots");
    assert!(on_st > 0, "fold-on must run the native STORE slots");
    eprintln!("=== end A/B (the anchors run PAGED, so this fold is Doom-inert) ===\n");
}

/// Round 1 hotness admission: with `set_jit_auto_admit(true)` and NO manual `try_admit`, a
/// hot loop compiles itself once its entry line crosses JIT_HOTNESS_THRESHOLD, and the
/// auto-admitted region stays byte-identical to the interpreter. The interp CPU (auto-admit
/// off) never compiles, proving the flag gates it.
#[test]
fn hotness_admission_compiles_a_hot_loop_and_stays_identical() {
    let mut interp = fresh_cpu(0xffff);
    let mut jit_cpu = fresh_cpu(0xffff);
    jit_cpu.set_jit_auto_admit(true);
    // A 64 KB buffer holds the ~0x2D00 that edi reaches over 64 iterations (0x500 + 64*0xa0).
    let mut bus_i = TestBus::with_memory(program_in(0x1_0000));
    let mut bus_j = TestBus::with_memory(program_in(0x1_0000));
    // 64 iterations: past the threshold (32), so the loop auto-admits mid-run and the region
    // runs the remaining iterations.
    arm_loop(&mut interp, &mut bus_i, 64);
    arm_loop(&mut jit_cpu, &mut bus_j, 64);
    let ci = drive_to_halt(&mut interp, &mut bus_i, u64::MAX);
    let cj = drive_to_halt(&mut jit_cpu, &mut bus_j, u64::MAX);
    assert_eq!(ci, cj, "hotness admission: per-run outcomes diverged");
    assert_identical(&interp, &bus_i, &jit_cpu, &bus_j);
    assert!(
        jit_cpu.perf_counters().jit_region_entries > 0,
        "the hot loop auto-admitted and ran a region"
    );
    assert_eq!(
        interp.perf_counters().jit_region_entries,
        0,
        "auto-admit off: the interpreter never compiles"
    );
}

/// Direct `CpuGsw` use keeps auto-admit off until requested. `Machine` applies the production
/// environment policy separately, while this default keeps manual-admission tests deterministic.
#[test]
fn no_auto_admit_by_default() {
    let mut cpu = fresh_cpu(0xffff);
    let mut bus = TestBus::with_memory(program_in(0x1_0000));
    arm_loop(&mut cpu, &mut bus, 64);
    drive_to_halt(&mut cpu, &mut bus, u64::MAX);
    assert_eq!(
        cpu.perf_counters().jit_region_entries,
        0,
        "no region should compile without auto-admit or the forced address"
    );
}

/// The capacity-GC primitive: `RegionTable::clear` + a decode-generation bump must leave NO
/// live stamp pointing into the emptied table, so `try_admit`'s clear-on-full can never
/// follow a dangling index. Admit a region, confirm it resolves, clear + invalidate, confirm
/// the stamp no longer resolves.
#[test]
fn clear_and_invalidate_drops_region_stamps() {
    let mut cpu = fresh_cpu(0xffff);
    let mut bus = TestBus::with_memory(program());
    arm_loop(&mut cpu, &mut bus, 2);
    drive_to_halt(&mut cpu, &mut bus, u64::MAX);
    let idx = jit::block::try_admit(&mut cpu, ENTRY, true).expect("admit");
    cpu.decode_cache.stamp_region(ENTRY, true, idx);
    assert_eq!(cpu.decode_cache.region_at(ENTRY, true), Some(idx));
    cpu.jit_regions.clear();
    cpu.decode_cache.invalidate();
    assert_eq!(
        cpu.decode_cache.region_at(ENTRY, true),
        None,
        "a cleared table must leave no resolvable stamp"
    );
    assert_eq!(cpu.jit_regions.len(), 0);
}

/// `run_region` unstamps a stale region (SMC epoch / mode-key mismatch) while leaving the
/// entry line LIVE - no generation bump, no re-decode - so its hotness counter is NOT reset
/// by `put`. Without `unstamp_region` re-priming it, the fire-once counter stays pinned at
/// the threshold and, under pure auto-admit (no forced address to re-trigger `try_admit`),
/// the loop de-JITs permanently. This tests the primitive directly: an unstamp of a live,
/// hot line must leave it ready to re-fire admission on the very next miss. (An integration
/// test cannot reliably reach this state - the drawcolumn self-patcher and a segment reload
/// both bump the decode generation, which re-decodes the entry line and resets hotness via
/// `put`, masking the gap.)
#[test]
fn unstamp_reprimes_hotness_so_a_stale_region_re_admits() {
    let mut cpu = fresh_cpu(0xffff);
    let mut bus = TestBus::with_memory(program());
    // Warm the ENTRY line so its decode is live (auto-admit off, so hotness stays 0).
    arm_loop(&mut cpu, &mut bus, 2);
    drive_to_halt(&mut cpu, &mut bus, u64::MAX);
    // Drive the counter across the threshold (fires once), then confirm it is pinned.
    let mut fired = false;
    for _ in 0..64 {
        fired |= cpu.decode_cache.note_hot_miss(ENTRY, true);
    }
    assert!(
        fired,
        "hotness crosses the threshold and fires admission once"
    );
    assert!(
        !cpu.decode_cache.note_hot_miss(ENTRY, true),
        "the fire-once counter is pinned after firing"
    );
    // Unstamping a live line (run_region's stale-region path) must re-prime it, so the very
    // next miss re-fires. Without the fix this stays false and the loop never re-admits.
    cpu.decode_cache.unstamp_region(ENTRY, true);
    assert!(
        cpu.decode_cache.note_hot_miss(ENTRY, true),
        "unstamp re-primes hotness so the next miss re-fires admission"
    );
}
