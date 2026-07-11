// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn fresh() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    for segment in [
        SegmentIndex::Cs,
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
    ] {
        cpu.load_segment_real(segment, 0);
    }
    let mut cs = cpu.registers.cs();
    cs.default_size_32 = true;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    cpu.registers.eip = 0x100;
    cpu
}

fn drive(cpu: &mut CpuGsw, bus: &mut TestBus) -> Vec<(u32, u32, bool)> {
    let mut outcomes = Vec::new();
    for _ in 0..64 {
        let outcome = cpu.run_straight_line(bus, u64::MAX).unwrap();
        outcomes.push((outcome.core_clocks, cpu.registers.eip, outcome.halted));
        if outcome.halted {
            return outcomes;
        }
    }
    panic!("guest did not halt");
}

fn loop_program() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[0x100..0x110].copy_from_slice(&[
        0xb9, 0x03, 0x00, 0x00, 0x00, // mov ecx,3
        0x83, 0xc0, 0x03, // add eax,3
        0x89, 0xc2, // mov edx,eax
        0x83, 0xe9, 0x01, // sub ecx,1
        0x75, 0xf6, // jnz 0x105
        0xf4, // hlt
    ]);
    memory
}

#[test]
fn direct_block_matches_taken_and_fallthrough_jcc_timing() {
    let mut interp = fresh();
    let mut native = fresh();
    interp.registers.set_eax(1);
    native.registers.set_eax(1);
    let mut interp_bus = TestBus::with_memory(loop_program());
    let mut native_bus = TestBus::with_memory(loop_program());

    // Warm every decode line with admission disabled, then measure the first-seen/second-entry
    // policy without cold-decode boundaries obscuring either Jcc path.
    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);
    for cpu in [&mut interp, &mut native] {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.registers.set_eax(1);
        cpu.registers.set_edx(0);
    }
    let region = jit::block::try_admit(&mut native, 0x105, true).expect("warm loop must admit");
    native.decode_cache.stamp_region(0x105, true, region);
    native.set_jit_auto_admit(true);
    let native_before = native.perf_counters().jit_native_insns;
    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(
        native_outcomes, interp_outcomes,
        "run-boundary timing differs"
    );
    assert_eq!(native, interp, "architectural or clock state differs");
    assert_eq!(native_bus.trace.cycles(), interp_bus.trace.cycles());
    assert_eq!(native.registers.eax(), 10);
    assert_eq!(native.registers.edx(), 10);
    assert!(
        native.jit_direct.len() > 0,
        "the direct block was not cached"
    );
    assert!(
        native.perf_counters().jit_native_insns - native_before >= 8,
        "taken and fallthrough executions must both be native: {:?}, cache={}",
        native.perf_counters(),
        native.jit_direct.len()
    );
    assert_eq!(native.perf_counters().jit_helper_exits, 0);
}

fn shift_program() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[0x100..0x10c].copy_from_slice(&[
        0x90, // nop starter
        0xb8, 0x03, 0x00, 0x00, 0x00, // mov eax,3
        0xc1, 0xe8, 0x01, // shr eax,1
        0x89, 0xc2, // mov edx,eax
        0xf4, // hlt
    ]);
    memory
}

#[test]
fn direct_shift_keeps_raw_timing_and_flag_state() {
    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(shift_program());
    let mut native_bus = TestBus::with_memory(shift_program());

    // Warm with admission disabled, then run one first encounter before the measured compile.
    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);
    let region = jit::block::try_admit(&mut native, 0x101, true).expect("warm block must admit");
    native.decode_cache.stamp_region(0x101, true, region);
    for cpu in [&mut interp, &mut native] {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.alu_add(0xffff_ffff, 1, 0, BusWidth::Dword);
    }
    native.set_jit_auto_admit(true);
    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);

    for cpu in [&mut interp, &mut native] {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.alu_add(0xffff_ffff, 1, 0, BusWidth::Dword);
    }
    let interp_elapsed = interp.elapsed_clocks;
    let native_elapsed = native.elapsed_clocks;
    let native_before = native.perf_counters().jit_native_insns;
    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(native_outcomes, interp_outcomes, "shift timing differs");
    assert_eq!(
        native.elapsed_clocks - native_elapsed,
        interp.elapsed_clocks - interp_elapsed,
        "raw clocks were not batched exactly"
    );
    assert_eq!(native, interp, "shift flags or pending state differs");
    assert_eq!(native.eflags(), interp.eflags());
    assert_eq!(native.registers.eax(), 1);
    assert_eq!(native.registers.edx(), 1);
    assert!(
        native.perf_counters().jit_native_insns - native_before >= 3,
        "direct shift did not run: {:?}, cache={}",
        native.perf_counters(),
        native.jit_direct.len()
    );
}

#[test]
fn cold_and_unstamped_rejected_continuations_do_not_probe_direct() {
    let mut memory = vec![0; 0x1000];
    memory[0x100..0x10d].copy_from_slice(&[
        0xb9, 0x03, 0x00, 0x00, 0x00, // mov ecx,3
        0x8b, 0x06, // mov eax,[esi] (direct compiler rejects memory)
        0x83, 0xe9, 0x01, // sub ecx,1
        0x75, 0xf9, // jnz 0x105
        0xf4, // hlt
    ]);
    let mut cpu = fresh();
    cpu.registers.set_esi(0x200);
    cpu.set_jit_auto_admit(true);
    let mut bus = TestBus::with_memory(memory);

    drive(&mut cpu, &mut bus);

    assert_eq!(cpu.jit_direct.tracked_len(), 0);
    assert_eq!(cpu.jit_direct.len(), 0);
    assert_eq!(cpu.perf_counters().jit_region_entries, 0);
}

#[test]
fn accurate_386_modes_never_enter_either_jit() {
    for mode in [GswMode::Gsw386Slow, GswMode::Gsw386] {
        let mut cpu = fresh();
        cpu.set_mode(mode);
        cpu.set_jit_auto_admit(true);
        cpu.registers.eip = 0x100;
        let mut bus = TestBus::with_memory(loop_program());
        drive(&mut cpu, &mut bus);
        assert_eq!(cpu.perf_counters().jit_region_entries, 0);
        assert_eq!(cpu.perf_counters().jit_native_insns, 0);
        assert_eq!(cpu.jit_direct.len(), 0);
    }
}
