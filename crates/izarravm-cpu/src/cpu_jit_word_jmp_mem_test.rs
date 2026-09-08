// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::sixteen_bit::{
    arm_native_sixteen_bit, sixteen_bit_bus, sixteen_bit_code_cpu, warm_sixteen_bit,
};
use super::*;

#[path = "cpu_jit_word_jmp_mem_x87_test.rs"]
mod x87;

const SOURCE: u32 = 0x100;
const TARGET: u32 = 0x300;
const POINTER: u32 = 0x800;

fn word_jmp_program(pointer: u16) -> Vec<u8> {
    let mut memory = vec![0; 0x4000];
    memory[SOURCE as usize..SOURCE as usize + 7].copy_from_slice(&[
        0x90,
        0x90,
        0x90,
        0xff,
        0x26,
        POINTER as u8,
        (POINTER >> 8) as u8,
    ]);
    memory[TARGET as usize..TARGET as usize + 3].copy_from_slice(&[0x90, 0x90, 0xf4]);
    memory[POINTER as usize..POINTER as usize + 4].copy_from_slice(&[
        pointer as u8,
        (pointer >> 8) as u8,
        0xa5,
        0x5a,
    ]);
    memory
}

fn install_word_jmp(cpu: &mut CpuGsw, entry: u32) -> jit::direct::CompiledBlock {
    let key = jit::direct::key_for(cpu, entry, false).expect("16-bit source key");
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = jit::direct::compile(cpu, entry, false).expect("Word JmpMem block");
    assert_eq!(compilation.span.instructions, 3);
    assert_eq!(compilation.word_reads, 1);
    assert_eq!(compilation.dword_reads, 0);
    assert!(compilation.dynamic_successor);
    assert_eq!(compilation.successors, [None, None]);
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("install source");
    cpu.jit_direct.block(id).expect("live source")
}

#[test]
fn word_memory_jump_emits_two_zero_extending_target_loads() {
    let mut cpu = sixteen_bit_code_cpu(SOURCE);
    let mut bus = sixteen_bit_bus(dynamic_word_jmp_program());
    warm_sixteen_bit(&mut cpu, &mut bus, &[SOURCE + 1, SOURCE + 2, SOURCE + 3]);
    arm_native_sixteen_bit(&mut cpu, &mut bus, &[0]);
    let compilation = jit::direct::compile(&mut cpu, SOURCE + 1, false).expect("source block");
    assert_eq!(compilation.span.instructions, 3);
    assert_eq!(
        compilation
            .code
            .windows(4)
            .filter(|bytes| *bytes == [0x0f, 0xb7, 0x57, 0x00])
            .count(),
        2
    );
    assert_eq!(
        compilation
            .code
            .windows(3)
            .filter(|bytes| *bytes == [0x8b, 0x57, 0x00])
            .count(),
        0
    );
    if let Some(path) = std::env::var_os("IZARRAVM_WORD_JMP_CODE_OUT") {
        std::fs::write(path, &compilation.code).expect("write emitted-code artifact");
    }
}

#[test]
fn word_memory_jump_matches_interpreter_with_poisoned_high_bytes() {
    let memory = word_jmp_program(TARGET as u16);
    let mut interp = sixteen_bit_code_cpu(SOURCE);
    let mut native = sixteen_bit_code_cpu(SOURCE);
    let mut interp_bus = sixteen_bit_bus(memory.clone());
    let mut native_bus = sixteen_bit_bus(memory);
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.direct_page_clocks = true;
    }
    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..3 {
        native.halted = false;
        native.set_eip(SOURCE);
        drive(&mut native, &mut native_bus);
    }
    let source_key = jit::direct::key_for(&native, SOURCE + 1, false).expect("source key");
    assert!(matches!(
        native.jit_direct.probe(source_key),
        jit::direct::BlockProbe::Ready(_)
    ));

    for cpu in [&mut interp, &mut native] {
        cpu.halted = false;
        cpu.set_eip(SOURCE);
        cpu.registers.gpr.fill(0);
        cpu.registers.eflags = 0x246;
        cpu.pending_flags = PendingFlags::default();
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.trace = BusTrace::default();
    }
    let before = native.perf_counters().jit_direct_insns;
    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(
        native_outcomes.iter().map(|outcome| outcome.0).sum::<u64>(),
        interp_outcomes.iter().map(|outcome| outcome.0).sum::<u64>()
    );
    assert_eq!(native.registers.eip, TARGET + 3);
    assert!(native.perf_counters().jit_direct_insns - before >= 3);
    assert_eq!(
        crate::tests::settled_state(&native),
        crate::tests::settled_state(&interp)
    );
    assert_eq!(native.pending_flags, interp.pending_flags);
    assert_eq!(native_bus.memory, interp_bus.memory);
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
}

#[test]
fn word_memory_jump_uses_ds_ss_cs_and_wraps_sixteen_bit_ea() {
    #[derive(Clone, Copy, Debug)]
    enum Shape {
        Ds,
        Ss,
        Cs,
        Wrap,
    }
    for shape in [Shape::Ds, Shape::Ss, Shape::Cs, Shape::Wrap] {
        let mut memory = vec![0; 0x6000];
        let (code, hit, decoys): (Vec<u8>, usize, Vec<usize>) = match shape {
            Shape::Ds => (
                vec![0x90, 0x90, 0xff, 0x26, 0x00, 0x08],
                0x1800,
                vec![0x0800, 0x2800],
            ),
            Shape::Ss => (
                vec![0x90, 0x90, 0xff, 0x66, 0x10],
                0x2810,
                vec![0x0810, 0x1810],
            ),
            Shape::Cs => (
                vec![0x90, 0x90, 0x2e, 0xff, 0x26, 0x00, 0x08],
                0x0800,
                vec![0x1800, 0x2800],
            ),
            Shape::Wrap => (vec![0x90, 0x90, 0xff, 0x20], 0x1020, vec![0x1000, 0x1040]),
        };
        memory[SOURCE as usize..SOURCE as usize + code.len()].copy_from_slice(&code);
        memory[hit..hit + 2].copy_from_slice(&(TARGET as u16).to_le_bytes());
        for decoy in decoys {
            memory[decoy..decoy + 2].copy_from_slice(&0x1234u16.to_le_bytes());
        }

        let mut cpu = sixteen_bit_code_cpu(SOURCE);
        cpu.load_segment_real(SegmentIndex::Ds, 0x100);
        cpu.load_segment_real(SegmentIndex::Ss, 0x200);
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.set_ebp(0x0800);
        cpu.registers.set_ebx(0xfff0);
        cpu.registers.set_esi(0x0030);
        let mut bus = sixteen_bit_bus(memory);
        warm_sixteen_bit(&mut cpu, &mut bus, &[SOURCE, SOURCE + 1, SOURCE + 2]);
        arm_native_sixteen_bit(&mut cpu, &mut bus, &[0, 0x1000, 0x2000]);
        let block = install_word_jmp(&mut cpu, SOURCE);

        assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
        assert_eq!(cpu.registers.eip, TARGET, "{shape:?}");
    }
}

fn dynamic_word_jmp_program() -> Vec<u8> {
    let mut memory = vec![0; 0x3000];
    memory[SOURCE as usize..SOURCE as usize + 5].copy_from_slice(&[0x90, 0x90, 0x90, 0xff, 0x27]);
    memory[TARGET as usize..TARGET as usize + 3].copy_from_slice(&[0x90, 0x90, 0xf4]);
    memory[POINTER as usize..POINTER as usize + 2].copy_from_slice(&(TARGET as u16).to_le_bytes());
    memory
}

fn prime_dynamic_word_jmp(cpu: &mut CpuGsw, bus: &mut TestBus) {
    cpu.registers.set_ebx(POINTER);
    drive(cpu, bus);
    cpu.set_jit_auto_admit(true);
    for _ in 0..3 {
        cpu.halted = false;
        cpu.set_eip(SOURCE);
        cpu.registers.set_ebx(POINTER);
        drive(cpu, bus);
    }
    let key = jit::direct::key_for(cpu, SOURCE + 1, false).expect("dynamic source key");
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Ready(_)
    ));
}

#[test]
fn word_memory_jump_alignment_and_unavailable_exits_replay_once() {
    for unavailable in [false, true] {
        let memory = dynamic_word_jmp_program();
        let mut interp = sixteen_bit_code_cpu(SOURCE);
        let mut native = sixteen_bit_code_cpu(SOURCE);
        let mut interp_bus = sixteen_bit_bus(memory.clone());
        let mut native_bus = sixteen_bit_bus(memory);
        for bus in [&mut interp_bus, &mut native_bus] {
            bus.direct_page_clocks = true;
        }
        interp.registers.set_ebx(POINTER);
        drive(&mut interp, &mut interp_bus);
        prime_dynamic_word_jmp(&mut native, &mut native_bus);

        let operand = if unavailable {
            POINTER + 0x1000
        } else {
            POINTER + 1
        };
        for bus in [&mut interp_bus, &mut native_bus] {
            bus.memory[operand as usize..operand as usize + 2]
                .copy_from_slice(&(TARGET as u16).to_le_bytes());
            bus.side_effect_read_address = Some(operand);
            bus.side_effect_read_count = 0;
            bus.trace.clear();
            if unavailable {
                bus.non_direct_read_pages.push(operand >> 12);
                bus.direct_pages_enabled = false;
                assert!(bus.peek_direct_ram(operand, BusWidth::Word).is_none());
            }
        }
        if unavailable {
            native.jit_fast_map.invalidate_page(operand);
        }
        for cpu in [&mut interp, &mut native] {
            cpu.halted = false;
            cpu.set_eip(SOURCE);
            cpu.registers.gpr.fill(0);
            cpu.registers.set_ebx(operand);
            cpu.registers.eflags = 0x246;
            cpu.pending_flags = PendingFlags::default();
            cpu.elapsed_clocks = 0;
            cpu.timing_rem = 0;
            cpu.core_clocks_so_far = 0;
        }
        let interp_guest_before = interp.perf_counters().instructions;
        let guest_before = native.perf_counters().instructions;
        let direct_before = native.perf_counters().jit_direct_insns;
        let exits_before = native.perf_counters().jit_direct_side_exits;
        let alignment_before = native
            .perf_counters()
            .jit_direct_exit_cross_page_or_alignment;
        let unavailable_before = native.perf_counters().jit_direct_exit_unavailable_or_kind;

        drive(&mut interp, &mut interp_bus);
        drive(&mut native, &mut native_bus);

        assert_eq!(
            crate::tests::settled_state(&native),
            crate::tests::settled_state(&interp)
        );
        assert_eq!(native.pending_flags, interp.pending_flags);
        assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
        assert_eq!(
            native_bus.trace.elapsed_clocks(),
            interp_bus.trace.elapsed_clocks()
        );
        assert_eq!(native_bus.side_effect_read_count, u64::from(unavailable));
        assert_eq!(interp_bus.side_effect_read_count, u64::from(unavailable));
        assert_eq!(
            native_bus
                .trace
                .cycles()
                .iter()
                .filter(|cycle| {
                    cycle.kind == BusAccessKind::DataRead && cycle.address == operand
                })
                .count(),
            1
        );
        let data_reads = native_bus
            .trace
            .cycles()
            .iter()
            .filter(|cycle| {
                cycle.kind == BusAccessKind::DataRead
                    && (cycle.address == operand || cycle.address == operand + 1)
            })
            .map(|cycle| (cycle.address, cycle.width))
            .collect::<Vec<_>>();
        assert_eq!(
            data_reads,
            if unavailable {
                vec![(operand, BusWidth::Word)]
            } else {
                vec![(operand, BusWidth::Byte), (operand + 1, BusWidth::Byte)]
            }
        );
        assert_eq!(native.perf_counters().instructions - guest_before, 7);
        assert_eq!(interp.perf_counters().instructions - interp_guest_before, 7);
        assert_eq!(native.perf_counters().jit_direct_insns - direct_before, 2);
        assert_eq!(
            native.perf_counters().jit_direct_side_exits - exits_before,
            1
        );
        assert_eq!(
            native
                .perf_counters()
                .jit_direct_exit_cross_page_or_alignment
                - alignment_before,
            u64::from(!unavailable)
        );
        assert_eq!(
            native.perf_counters().jit_direct_exit_unavailable_or_kind - unavailable_before,
            u64::from(unavailable)
        );
    }
}

#[test]
fn word_memory_jump_accepts_a_two_byte_aligned_source() {
    let operand = POINTER + 2;
    let mut memory = dynamic_word_jmp_program();
    memory[operand as usize..operand as usize + 2].copy_from_slice(&(TARGET as u16).to_le_bytes());
    let mut cpu = sixteen_bit_code_cpu(SOURCE);
    let mut bus = sixteen_bit_bus(memory);
    warm_sixteen_bit(&mut cpu, &mut bus, &[SOURCE + 1, SOURCE + 2, SOURCE + 3]);
    arm_native_sixteen_bit(&mut cpu, &mut bus, &[0]);
    let block = install_word_jmp(&mut cpu, SOURCE + 1);
    cpu.set_eip(SOURCE + 1);
    cpu.registers.set_ebx(operand);
    let exits = cpu.perf_counters().jit_direct_side_exits;

    assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    assert_eq!(cpu.registers.eip, TARGET);
    assert_eq!(cpu.perf_counters().jit_direct_side_exits, exits);
}

fn run_to_error(cpu: &mut CpuGsw, bus: &mut TestBus) -> CpuRunError {
    for _ in 0..8 {
        match cpu.run_straight_line(bus, u64::MAX) {
            Ok(outcome) => assert!(!outcome.halted, "fixture halted before its fault"),
            Err(error) => return error,
        }
    }
    panic!("fixture did not fault")
}

#[test]
fn word_memory_jump_source_limit_exits_then_dispatcher_replays_the_fault() {
    const FAILING: u32 = 0x900;
    let memory = dynamic_word_jmp_program();
    let mut interp = sixteen_bit_code_cpu(SOURCE);
    let mut native = sixteen_bit_code_cpu(SOURCE);
    for cpu in [&mut interp, &mut native] {
        let mut ds = cpu.registers.segment(SegmentIndex::Ds);
        ds.limit = FAILING - 1;
        cpu.registers.set_segment(SegmentIndex::Ds, ds);
    }
    let mut interp_bus = sixteen_bit_bus(memory.clone());
    let mut native_bus = sixteen_bit_bus(memory);
    interp.registers.set_ebx(POINTER);
    drive(&mut interp, &mut interp_bus);
    prime_dynamic_word_jmp(&mut native, &mut native_bus);

    for bus in [&mut interp_bus, &mut native_bus] {
        bus.memory[FAILING as usize..FAILING as usize + 2]
            .copy_from_slice(&(TARGET as u16).to_le_bytes());
        bus.side_effect_read_address = Some(FAILING);
        bus.side_effect_read_count = 0;
        bus.trace.clear();
    }
    for cpu in [&mut interp, &mut native] {
        cpu.halted = false;
        cpu.set_eip(SOURCE);
        cpu.registers.gpr.fill(0);
        cpu.registers.set_ebx(FAILING);
        cpu.registers.eflags = 0x246;
        cpu.pending_flags = PendingFlags::default();
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    let interp_guest_before = interp.perf_counters().instructions;
    let guest_before = native.perf_counters().instructions;
    let direct_before = native.perf_counters().jit_direct_insns;
    let exits_before = native.perf_counters().jit_direct_side_exits;
    let limits_before = native.direct_stall_snapshot().side_exit_segment_limit;

    let interp_error = run_to_error(&mut interp, &mut interp_bus);
    let native_error = run_to_error(&mut native, &mut native_bus);

    assert_eq!(native_error, interp_error);
    assert!(matches!(
        native_error,
        CpuRunError {
            error: CpuError::Bus(BusError::UnmappedMemory { address: 0xfffe }),
            consumed_core_clocks: 3,
        }
    ));
    assert_eq!(
        crate::tests::settled_state(&native),
        crate::tests::settled_state(&interp)
    );
    assert_eq!(native.pending_flags, interp.pending_flags);
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert_eq!(native_bus.side_effect_read_count, 0);
    assert_eq!(interp_bus.side_effect_read_count, 0);
    assert_eq!(native.perf_counters().instructions - guest_before, 3);
    assert_eq!(interp.perf_counters().instructions - interp_guest_before, 3);
    assert_eq!(
        native_bus
            .trace
            .cycles()
            .iter()
            .filter(|cycle| cycle.kind == BusAccessKind::DataRead && cycle.address == FAILING)
            .count(),
        0
    );
    assert_eq!(native.perf_counters().jit_direct_insns - direct_before, 2);
    assert_eq!(
        native.perf_counters().jit_direct_side_exits - exits_before,
        1
    );
    assert_eq!(
        native.direct_stall_snapshot().side_exit_segment_limit - limits_before,
        1
    );
}

#[test]
fn word_memory_jump_target_limit_replays_then_next_fetch_faults_without_reread() {
    const TOO_LARGE: u16 = 0x500;
    let memory = dynamic_word_jmp_program();
    let mut interp = sixteen_bit_code_cpu(SOURCE);
    let mut native = sixteen_bit_code_cpu(SOURCE);
    for cpu in [&mut interp, &mut native] {
        let mut cs = cpu.registers.cs();
        cs.limit = 0x3ff;
        cpu.registers.set_segment(SegmentIndex::Cs, cs);
    }
    let mut interp_bus = sixteen_bit_bus(memory.clone());
    let mut native_bus = sixteen_bit_bus(memory);
    interp.registers.set_ebx(POINTER);
    drive(&mut interp, &mut interp_bus);
    prime_dynamic_word_jmp(&mut native, &mut native_bus);

    for bus in [&mut interp_bus, &mut native_bus] {
        bus.memory[POINTER as usize..POINTER as usize + 2]
            .copy_from_slice(&TOO_LARGE.to_le_bytes());
        bus.side_effect_read_address = Some(POINTER);
        bus.side_effect_read_count = 0;
        bus.trace.clear();
    }
    for cpu in [&mut interp, &mut native] {
        cpu.halted = false;
        cpu.set_eip(SOURCE);
        cpu.registers.gpr.fill(0);
        cpu.registers.set_ebx(POINTER);
        cpu.registers.eflags = 0x246;
        cpu.pending_flags = PendingFlags::default();
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    let direct_before = native.perf_counters().jit_direct_insns;
    let exits_before = native.perf_counters().jit_direct_side_exits;
    let guest_before = native.perf_counters().instructions;
    let interp_guest_before = interp.perf_counters().instructions;
    let limits_before = native.direct_stall_snapshot().side_exit_segment_limit;

    interp
        .cycle_no_interrupt_check(&mut interp_bus)
        .expect("interpreter starter");
    native
        .cycle_no_interrupt_check(&mut native_bus)
        .expect("native starter");
    let source_key = jit::direct::key_for(&native, SOURCE + 1, false).expect("source key");
    let jit::direct::BlockProbe::Ready(source_id) = native.jit_direct.probe(source_key) else {
        panic!("primed source block must remain ready");
    };
    let source = native
        .jit_direct
        .block(source_id)
        .expect("live source block");
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, source)
            .expect("native restart exit")
    );
    interp
        .cycle_no_interrupt_check(&mut interp_bus)
        .expect("first interpreted filler");
    interp
        .cycle_no_interrupt_check(&mut interp_bus)
        .expect("second interpreted filler");

    assert_eq!(native.registers.eip, SOURCE + 3);
    assert_eq!(native.perf_counters().instructions - guest_before, 3);
    assert_eq!(interp.perf_counters().instructions - interp_guest_before, 3);
    assert_eq!(native_bus.side_effect_read_count, 0);
    assert_eq!(interp_bus.side_effect_read_count, 0);
    assert_eq!(
        crate::tests::settled_state(&native),
        crate::tests::settled_state(&interp),
        "the native limit exit must preserve the interpreter restart state"
    );
    assert_eq!(native.pending_flags, interp.pending_flags);
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );

    for bus in [&mut interp_bus, &mut native_bus] {
        bus.direct_pages_enabled = false;
    }
    interp.set_fast_map_enabled_for_test(false);
    native.set_fast_map_enabled_for_test(false);
    interp.data_read_pages.invalidate();
    native.data_read_pages.invalidate();
    interp
        .cycle_no_interrupt_check(&mut interp_bus)
        .expect("interpreter jump retires");
    native
        .cycle_no_interrupt_check(&mut native_bus)
        .expect("dispatcher replay retires the jump");
    assert_eq!(native.registers.eip, u32::from(TOO_LARGE));
    assert_eq!(native.perf_counters().instructions - guest_before, 4);
    assert_eq!(interp.perf_counters().instructions - interp_guest_before, 4);
    assert_eq!(native_bus.side_effect_read_count, 1);
    assert_eq!(interp_bus.side_effect_read_count, 1);
    assert_eq!(
        crate::tests::settled_state(&native),
        crate::tests::settled_state(&interp),
        "the replayed jump must retire before the target fetch faults"
    );
    assert_eq!(native.pending_flags, interp.pending_flags);
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );

    let interp_error = interp
        .cycle_no_interrupt_check(&mut interp_bus)
        .expect_err("next interpreter fetch must fault");
    let native_error = native
        .cycle_no_interrupt_check(&mut native_bus)
        .expect_err("next replay fetch must fault");

    assert_eq!(native_error, interp_error);
    assert!(
        matches!(
            native_error,
            CpuRunError {
                error: CpuError::Bus(BusError::UnmappedMemory { address: 0xfffe }),
                consumed_core_clocks: 0,
            }
        ),
        "{native_error:?}"
    );
    assert_eq!(native.registers.eip, u32::from(TOO_LARGE));
    assert_eq!(native.perf_counters().instructions - guest_before, 4);
    assert_eq!(interp.perf_counters().instructions - interp_guest_before, 4);
    assert_eq!(
        crate::tests::settled_state(&native),
        crate::tests::settled_state(&interp)
    );
    assert_eq!(native.pending_flags, interp.pending_flags);
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert_eq!(native_bus.side_effect_read_count, 1);
    assert_eq!(interp_bus.side_effect_read_count, 1);
    assert_eq!(
        native_bus
            .trace
            .cycles()
            .iter()
            .filter(|cycle| cycle.kind == BusAccessKind::DataRead && cycle.address == POINTER)
            .map(|cycle| cycle.width)
            .collect::<Vec<_>>(),
        vec![BusWidth::Word]
    );
    assert_eq!(native.perf_counters().jit_direct_insns - direct_before, 2);
    assert_eq!(
        native.perf_counters().jit_direct_side_exits - exits_before,
        1
    );
    assert_eq!(
        native.direct_stall_snapshot().side_exit_segment_limit - limits_before,
        1
    );
}

#[test]
fn word_memory_jump_refuses_an_unreadable_protected_mode_source_segment() {
    for (expect_compiled, access) in [(false, 0xf9), (true, 0xf3)] {
        let mut memory = word_jmp_program(TARGET as u16);
        memory[SOURCE as usize..SOURCE as usize + 6]
            .copy_from_slice(&[0x90, 0x90, 0xff, 0x26, 0x00, 0x08]);
        let mut cpu = sixteen_bit_code_cpu(SOURCE);
        promote_to_cpl3(&mut cpu);
        let mut cs = cpu.registers.cs();
        cs.default_size_32 = false;
        cpu.registers.set_segment(SegmentIndex::Cs, cs);
        let mut ds = cpu.registers.segment(SegmentIndex::Ds);
        ds.access = access;
        cpu.registers.set_segment(SegmentIndex::Ds, ds);
        let mut bus = sixteen_bit_bus(memory);
        warm_sixteen_bit(&mut cpu, &mut bus, &[SOURCE, SOURCE + 1, SOURCE + 2]);
        arm_native_sixteen_bit(&mut cpu, &mut bus, &[0]);

        assert_eq!(
            jit::direct::compile(&mut cpu, SOURCE, false).is_some(),
            expect_compiled
        );
    }
}

#[test]
fn word_memory_jump_reloads_its_target_after_mode13_completion() {
    const APERTURE: usize = 0x000a_0000;
    let mut memory = vec![0; 0x000b_1000];
    memory[SOURCE as usize..SOURCE as usize + 7]
        .copy_from_slice(&[0x90, 0x90, 0x90, 0xff, 0x26, 0x00, 0x00]);
    memory[TARGET as usize..TARGET as usize + 3].copy_from_slice(&[0x90, 0x90, 0xf4]);
    memory[APERTURE..APERTURE + 4].copy_from_slice(&[
        TARGET as u8,
        (TARGET >> 8) as u8,
        0xa5,
        0x5a,
    ]);

    let mut interp = sixteen_bit_code_cpu(SOURCE);
    let mut native = sixteen_bit_code_cpu(SOURCE);
    for cpu in [&mut interp, &mut native] {
        cpu.load_segment_real(SegmentIndex::Ds, 0xa000);
    }
    let mut interp_bus = sixteen_bit_bus(memory.clone());
    let mut native_bus = sixteen_bit_bus(memory);
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.direct_page_clocks = true;
    }
    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..3 {
        native.halted = false;
        native.set_eip(SOURCE);
        drive(&mut native, &mut native_bus);
    }
    let source_key = jit::direct::key_for(&native, SOURCE + 1, false).expect("source key");
    assert!(matches!(
        native.jit_direct.probe(source_key),
        jit::direct::BlockProbe::Ready(_)
    ));

    for cpu in [&mut interp, &mut native] {
        cpu.halted = false;
        cpu.set_eip(SOURCE);
        cpu.registers.gpr.fill(0);
        cpu.registers.eflags = 0x246;
        cpu.pending_flags = PendingFlags::default();
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.trace.clear();
    }
    let direct_before = native.perf_counters().jit_direct_insns;
    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);

    assert_eq!(native.registers.eip, TARGET + 3);
    assert_eq!(
        crate::tests::settled_state(&native),
        crate::tests::settled_state(&interp)
    );
    assert_eq!(native.pending_flags, interp.pending_flags);
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert!(native.perf_counters().jit_direct_insns - direct_before >= 3);
}

#[test]
fn word_memory_jump_target_data_mutation_rebinds_without_retiring_the_source() {
    const DATA: u32 = 0x1800;
    const TARGET_A: u32 = 0x300;
    const TARGET_B: u32 = 0x500;
    let mut memory = vec![0; 0x3000];
    memory[SOURCE as usize..SOURCE as usize + 6].copy_from_slice(&[
        0x90,
        0x90,
        0xff,
        0x26,
        DATA as u8,
        (DATA >> 8) as u8,
    ]);
    memory[TARGET_A as usize..TARGET_A as usize + 4].copy_from_slice(&[0x40, 0x40, 0x40, 0xf4]);
    memory[TARGET_B as usize..TARGET_B as usize + 4].copy_from_slice(&[0x43, 0x43, 0x43, 0xf4]);
    memory[DATA as usize..DATA as usize + 2].copy_from_slice(&(TARGET_A as u16).to_le_bytes());

    let mut cpu = sixteen_bit_code_cpu(SOURCE);
    let mut bus = sixteen_bit_bus(memory);
    warm_sixteen_bit(
        &mut cpu,
        &mut bus,
        &[
            SOURCE,
            SOURCE + 1,
            SOURCE + 2,
            TARGET_A,
            TARGET_A + 1,
            TARGET_A + 2,
            TARGET_B,
            TARGET_B + 1,
            TARGET_B + 2,
        ],
    );
    assert!(!cpu.physical_page_watched(DATA));
    arm_native_sixteen_bit(&mut cpu, &mut bus, &[0, 0x1000]);

    let mut target_ids = Vec::new();
    for target in [TARGET_A, TARGET_B] {
        let key = jit::direct::key_for(&cpu, target, false).expect("target key");
        assert!(matches!(
            cpu.jit_direct.probe(key),
            jit::direct::BlockProbe::Interpret
        ));
        let compilation = jit::direct::compile(&mut cpu, target, false).expect("target block");
        target_ids.push(
            cpu.jit_direct
                .install(&compilation)
                .expect("target install"),
        );
    }
    let source_key = jit::direct::key_for(&cpu, SOURCE, false).expect("source key");
    assert!(matches!(
        cpu.jit_direct.probe(source_key),
        jit::direct::BlockProbe::Interpret
    ));
    let source_compilation = jit::direct::compile(&mut cpu, SOURCE, false).expect("source block");
    let source_id = cpu
        .jit_direct
        .install(&source_compilation)
        .expect("source install");
    let source = cpu.jit_direct.block(source_id).expect("live source");
    let initial_cells = [0, 1].map(|slot| {
        cpu.jit_direct
            .link_cell_state_for_test(source_id, slot)
            .expect("source link cell")
    });

    cpu.set_eip(SOURCE);
    assert!(cpu.try_run_direct_block_for_test(&mut bus, source).unwrap());
    assert_eq!(cpu.registers.eip, TARGET_A);
    assert_eq!(
        cpu.jit_direct
            .link_cell_state_for_test(source_id, 0)
            .expect("bound A")
            .1,
        TARGET_A
    );
    let cells_bound = [0, 1].map(|slot| {
        cpu.jit_direct
            .link_cell_state_for_test(source_id, slot)
            .expect("bound source link cell")
    });
    assert!(
        cells_bound
            .iter()
            .any(|state| state.1 == TARGET_A && state.2)
    );
    cpu.set_eip(SOURCE);
    cpu.registers.gpr.fill(0);
    assert!(cpu.try_run_direct_block_for_test(&mut bus, source).unwrap());
    assert_eq!(cpu.registers.eax(), 3);

    let installs = cpu.perf_counters().jit_direct_blocks_installed;
    let invalidations = cpu.perf_counters().code_invalidations;
    cpu.write_memory_sized(
        &mut bus,
        SegmentIndex::Ds,
        DATA,
        OperandSize::Word,
        TARGET_B,
        BusAccessKind::DataWrite,
    )
    .expect("architectural target-data write");
    assert!(!cpu.physical_page_watched(DATA));
    assert!(cpu.jit_direct.block(source_id).is_some());
    assert!(
        target_ids
            .iter()
            .all(|id| cpu.jit_direct.block(*id).is_some())
    );
    assert_eq!(cpu.perf_counters().jit_direct_blocks_installed, installs);
    assert_eq!(cpu.perf_counters().code_invalidations, invalidations);
    for (slot, initial) in initial_cells.iter().enumerate() {
        assert_eq!(
            cpu.jit_direct
                .link_cell_state_for_test(source_id, slot)
                .expect("same live cell")
                .0,
            initial.0
        );
    }
    assert_eq!(
        [0, 1].map(|slot| {
            cpu.jit_direct
                .link_cell_state_for_test(source_id, slot)
                .expect("bound cell survives target-data write")
        }),
        cells_bound,
        "the unwatched target-data write must preserve the full A binding"
    );

    let transfers = cpu.perf_counters().jit_direct_linked_transfers;
    cpu.set_eip(SOURCE);
    cpu.registers.gpr.fill(0);
    assert!(cpu.try_run_direct_block_for_test(&mut bus, source).unwrap());
    assert_eq!(cpu.registers.eip, TARGET_B);
    assert_eq!(cpu.registers.eax(), 0);
    assert_eq!(cpu.perf_counters().jit_direct_linked_transfers, transfers);
    assert!([0, 1].into_iter().any(|slot| {
        cpu.jit_direct
            .link_cell_state_for_test(source_id, slot)
            .is_some_and(|state| state.1 == TARGET_B && state.2)
    }));

    cpu.set_eip(SOURCE);
    cpu.registers.gpr.fill(0);
    assert!(cpu.try_run_direct_block_for_test(&mut bus, source).unwrap());
    assert_eq!(cpu.registers.ebx(), 3);
    assert_eq!(cpu.registers.eax(), 0);
    assert_eq!(
        cpu.perf_counters().jit_direct_linked_transfers - transfers,
        1
    );
}

#[test]
fn aliased_displacement_write_retires_and_recompiles_the_word_memory_jump() {
    const DATA_A: u32 = 0x1800;
    const DATA_B: u32 = 0x1a00;
    const TARGET_A: u32 = 0x2300;
    const TARGET_B: u32 = 0x2500;
    const PAGE_TABLE: u32 = 0x4000;
    const ALIAS_PAGE: u32 = 0x6000;
    const DISP_HIGH: u32 = SOURCE + 6;

    let mut memory = vec![0; 0x1_0000];
    memory[SOURCE as usize..SOURCE as usize + 7].copy_from_slice(&[
        0x90,
        0x90,
        0x90,
        0xff,
        0x26,
        DATA_A as u8,
        (DATA_A >> 8) as u8,
    ]);
    memory[TARGET_A as usize..TARGET_A as usize + 4].copy_from_slice(&[0x40, 0x40, 0x40, 0xf4]);
    memory[TARGET_B as usize..TARGET_B as usize + 4].copy_from_slice(&[0x43, 0x43, 0x43, 0xf4]);
    memory[DATA_A as usize..DATA_A as usize + 2].copy_from_slice(&(TARGET_A as u16).to_le_bytes());
    memory[DATA_B as usize..DATA_B as usize + 2].copy_from_slice(&(TARGET_B as u16).to_le_bytes());
    memory[0x3000..0x3004].copy_from_slice(&(PAGE_TABLE | 7).to_le_bytes());
    for page in 0..8u32 {
        let entry = (page << 12) | 7;
        let pte = PAGE_TABLE as usize + page as usize * 4;
        memory[pte..pte + 4].copy_from_slice(&entry.to_le_bytes());
    }
    let alias_pte = PAGE_TABLE as usize + (ALIAS_PAGE >> 12) as usize * 4;
    memory[alias_pte..alias_pte + 4].copy_from_slice(&7u32.to_le_bytes());

    let mut cpu = paged_word_cpu(SOURCE);
    let mut bus = sixteen_bit_bus(memory);
    warm_sixteen_bit(
        &mut cpu,
        &mut bus,
        &[
            SOURCE,
            SOURCE + 1,
            SOURCE + 2,
            SOURCE + 3,
            TARGET_A,
            TARGET_A + 1,
            TARGET_A + 2,
            TARGET_B,
            TARGET_B + 1,
            TARGET_B + 2,
        ],
    );
    arm_native_sixteen_bit(&mut cpu, &mut bus, &[0, 0x1000]);

    let mut target_ids = Vec::new();
    for target in [TARGET_A, TARGET_B] {
        let key = jit::direct::key_for(&cpu, target, false).expect("target key");
        assert!(matches!(
            cpu.jit_direct.probe(key),
            jit::direct::BlockProbe::Interpret
        ));
        let compilation = jit::direct::compile(&mut cpu, target, false).expect("target block");
        target_ids.push(
            cpu.jit_direct
                .install(&compilation)
                .expect("target install"),
        );
    }
    warm_sixteen_bit(
        &mut cpu,
        &mut bus,
        &[
            SOURCE + 1,
            SOURCE + 2,
            SOURCE + 3,
            TARGET_A,
            TARGET_A + 1,
            TARGET_A + 2,
            TARGET_B,
            TARGET_B + 1,
            TARGET_B + 2,
        ],
    );
    let source_key = jit::direct::key_for(&cpu, SOURCE + 1, false).expect("source key");
    assert!(matches!(
        cpu.jit_direct.probe(source_key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = jit::direct::compile(&mut cpu, SOURCE + 1, false).expect("source block");
    let source_id = cpu
        .jit_direct
        .install(&compilation)
        .expect("source install");
    let source = cpu.jit_direct.block(source_id).expect("live source");
    cpu.set_eip(SOURCE + 1);
    assert!(cpu.try_run_direct_block_for_test(&mut bus, source).unwrap());
    assert_eq!(cpu.registers.eip, TARGET_A);
    assert!([0, 1].into_iter().any(|slot| {
        cpu.jit_direct
            .link_cell_state_for_test(source_id, slot)
            .is_some_and(|state| state.1 == TARGET_A && state.2)
    }));
    assert!(cpu.physical_page_watched(0));
    assert!(!cpu.physical_page_watched(DATA_A));

    let invalidations = cpu.perf_counters().code_invalidations;
    cpu.write_memory_u8(
        &mut bus,
        SegmentIndex::Ds,
        ALIAS_PAGE + DISP_HIGH,
        (DATA_B >> 8) as u8,
        BusAccessKind::DataWrite,
    )
    .expect("architectural alias patch");
    assert_eq!(bus.memory[DISP_HIGH as usize], (DATA_B >> 8) as u8);
    assert_eq!(cpu.perf_counters().code_invalidations - invalidations, 1);
    assert!(cpu.jit_direct.block(source_id).is_none());
    assert_eq!(cpu.jit_direct.link_cell_state_for_test(source_id, 0), None);
    assert_eq!(cpu.jit_direct.link_cell_state_for_test(source_id, 1), None);
    assert!(
        target_ids
            .iter()
            .all(|id| cpu.jit_direct.block(*id).is_some())
    );

    warm_sixteen_bit(
        &mut cpu,
        &mut bus,
        &[
            SOURCE + 1,
            SOURCE + 2,
            SOURCE + 3,
            TARGET_A,
            TARGET_A + 1,
            TARGET_A + 2,
            TARGET_B,
            TARGET_B + 1,
            TARGET_B + 2,
        ],
    );
    arm_native_sixteen_bit(&mut cpu, &mut bus, &[0, 0x1000]);
    let new_key = jit::direct::key_for(&cpu, SOURCE + 1, false).expect("new source key");
    assert!(matches!(
        cpu.jit_direct.probe(new_key),
        jit::direct::BlockProbe::Interpret
    ));
    let new_compilation =
        jit::direct::compile(&mut cpu, SOURCE + 1, false).expect("recompiled source");
    let new_source_id = cpu
        .jit_direct
        .install(&new_compilation)
        .expect("new source install");
    assert_ne!(new_source_id, source_id);
    let new_source = cpu
        .jit_direct
        .block(new_source_id)
        .expect("live recompiled source");
    let rebound_target_key = jit::direct::key_for(&cpu, TARGET_B, false).expect("target B key");
    assert!(matches!(
        cpu.jit_direct.probe(rebound_target_key),
        jit::direct::BlockProbe::Ready(id) if id == target_ids[1]
    ));
    cpu.jit_direct
        .revalidate_translation(rebound_target_key)
        .expect("target B remains live after translation revalidation");
    cpu.set_eip(SOURCE + 1);
    cpu.registers.gpr.fill(0);
    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, new_source)
            .unwrap()
    );
    assert_eq!(cpu.registers.eip, TARGET_B);
    assert_eq!(cpu.registers.eax(), 0);
    let rebound_cells =
        [0, 1].map(|slot| cpu.jit_direct.link_cell_state_for_test(new_source_id, slot));
    assert!(
        rebound_cells
            .iter()
            .flatten()
            .any(|state| state.1 == TARGET_B && state.2),
        "recompiled link cells: {rebound_cells:?}"
    );
    cpu.set_eip(SOURCE + 1);
    cpu.registers.gpr.fill(0);
    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, new_source)
            .unwrap()
    );
    assert_eq!(cpu.registers.ebx(), 3);
    assert_eq!(cpu.registers.eax(), 0);
}

#[test]
fn word_memory_jump_respects_tight_budget_pending_irq_and_interrupt_shadow() {
    let mut memory = dynamic_word_jmp_program();
    memory[0] = 0xf4;
    let mut cpu = sixteen_bit_code_cpu(SOURCE);
    let mut bus = sixteen_bit_bus(memory);
    warm_sixteen_bit(
        &mut cpu,
        &mut bus,
        &[SOURCE, SOURCE + 1, SOURCE + 2, SOURCE + 3],
    );
    arm_native_sixteen_bit(&mut cpu, &mut bus, &[0]);
    let block = install_word_jmp(&mut cpu, SOURCE + 1);
    cpu.set_eip(SOURCE + 1);
    cpu.registers.set_ebx(POINTER);

    let (num, den) = level_timing(cpu.persona());
    let scaled_core_upper = u64::from(block.raw_clocks())
        .saturating_mul(u64::from(num))
        .div_ceil(u64::from(den));
    let fetch_upper = bus
        .jit_fetch_cost_clocks()
        .saturating_mul(u64::from(block.span().instructions));
    let word_read_upper = bus.jit_data_cost_clocks(BusWidth::Word);
    let iteration_upper = scaled_core_upper
        .saturating_add(bus.jit_scale_bus_cost_upper(fetch_upper.saturating_add(word_read_upper)));

    let registers = cpu.registers.clone();
    let pending = cpu.pending_flags;
    let budget_refusals = cpu.perf_counters().jit_direct_reject_zero_budget;
    assert!(
        !cpu.try_run_direct_block_with_cap_for_test(&mut bus, block, iteration_upper)
            .unwrap()
    );
    assert_eq!(cpu.registers, registers);
    assert_eq!(cpu.pending_flags, pending);
    assert_eq!(
        cpu.perf_counters().jit_direct_reject_zero_budget - budget_refusals,
        1
    );

    let guest_before = cpu.perf_counters().instructions;
    let direct_before = cpu.perf_counters().jit_direct_insns;
    assert!(
        cpu.try_run_direct_block_with_cap_for_test(&mut bus, block, iteration_upper + 1)
            .unwrap()
    );
    assert_eq!(cpu.registers.eip, TARGET);
    assert_eq!(cpu.perf_counters().instructions - guest_before, 3);
    assert_eq!(cpu.perf_counters().jit_direct_insns - direct_before, 3);

    cpu.registers = registers.clone();
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;

    let shadow_refusals = cpu.perf_counters().jit_direct_reject_interrupt_shadow;
    cpu.interrupt_shadow = true;
    assert!(!cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    assert_eq!(cpu.registers, registers);
    assert_eq!(cpu.pending_flags, pending);
    assert_eq!(
        cpu.perf_counters().jit_direct_reject_interrupt_shadow - shadow_refusals,
        1
    );

    cpu.interrupt_shadow = false;
    cpu.set_eip(SOURCE + 1);
    cpu.registers.eflags |= FLAG_IF;
    bus.pending_irq = Some(8);
    let source_key = jit::direct::key_for(&cpu, SOURCE + 1, false).expect("source key");
    assert!(matches!(
        cpu.jit_direct.probe(source_key),
        jit::direct::BlockProbe::Ready(_)
    ));
    let entries_before = cpu.perf_counters().jit_direct_entries;
    let direct_before = cpu.perf_counters().jit_direct_insns;
    assert!(
        cpu.service_pending_interrupt(&mut bus)
            .expect("pending IRQ delivery")
            .is_some()
    );
    assert_eq!(cpu.registers.eip, 0);
    assert_eq!(cpu.perf_counters().jit_direct_entries, entries_before);
    assert_eq!(cpu.perf_counters().jit_direct_insns, direct_before);
    cpu.cycle(&mut bus).unwrap();
    assert!(cpu.halted);
    assert_eq!(cpu.registers.eip, 1);
    assert_eq!(cpu.perf_counters().jit_direct_insns, direct_before);
}

fn paged_word_cpu(entry: u32) -> CpuGsw {
    let mut cpu = sixteen_bit_code_cpu(entry);
    cpu.control.cr0 |= CR0_PE | CR0_PG | CR0_WP;
    cpu.control.cr3 = 0x3000;
    cpu.cpl = 3;
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 3,
            base: 0,
            limit: u32::MAX,
            access: 0xfb,
            default_size_32: false,
        },
    );
    for segment in [SegmentIndex::Ds, SegmentIndex::Ss, SegmentIndex::Es] {
        cpu.registers.set_segment(
            segment,
            SegmentRegister {
                selector: 3,
                base: 0,
                limit: u32::MAX,
                access: 0xf3,
                default_size_32: false,
            },
        );
    }
    cpu
}

#[test]
fn cpl3_word_memory_jump_permission_exit_replays_through_the_dispatcher() {
    const DATA: u32 = 0x1800;
    let mut memory = vec![0; 0x6000];
    memory[SOURCE as usize..SOURCE as usize + 7].copy_from_slice(&[
        0x90,
        0x90,
        0x90,
        0xff,
        0x26,
        DATA as u8,
        (DATA >> 8) as u8,
    ]);
    memory[DATA as usize..DATA as usize + 2].copy_from_slice(&(TARGET as u16).to_le_bytes());
    memory[TARGET as usize..TARGET as usize + 3].copy_from_slice(&[0x90, 0x90, 0xf4]);
    memory[0x3000..0x3004].copy_from_slice(&0x4007u32.to_le_bytes());
    memory[0x4000..0x4004].copy_from_slice(&0x0007u32.to_le_bytes());
    memory[0x4004..0x4008].copy_from_slice(&0x1003u32.to_le_bytes());

    let mut interp = paged_word_cpu(SOURCE);
    let mut native = paged_word_cpu(SOURCE);
    let mut interp_bus = sixteen_bit_bus(memory.clone());
    let mut native_bus = sixteen_bit_bus(memory);
    warm_sixteen_bit(
        &mut interp,
        &mut interp_bus,
        &[SOURCE, SOURCE + 1, SOURCE + 2, SOURCE + 3],
    );
    warm_sixteen_bit(
        &mut native,
        &mut native_bus,
        &[SOURCE, SOURCE + 1, SOURCE + 2, SOURCE + 3],
    );
    arm_native_sixteen_bit(&mut native, &mut native_bus, &[0]);
    map_direct_page(
        &mut native,
        &mut native_bus,
        0x1000,
        0x1000,
        jit::fast_map::PagePermissions {
            writable: true,
            user: false,
        },
        true,
        false,
    );
    let block = install_word_jmp(&mut native, SOURCE + 1);
    assert_eq!(block.span().instructions, 3);

    for cpu in [&mut interp, &mut native] {
        cpu.set_eip(SOURCE);
        cpu.registers.eflags = 0x246;
        cpu.pending_flags = PendingFlags::default();
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.trace.clear();
    }
    let guest_before = native.perf_counters().instructions;
    let interp_guest_before = interp.perf_counters().instructions;
    let direct_before = native.perf_counters().jit_direct_insns;
    let exits_before = native.perf_counters().jit_direct_side_exits;
    let permission_before = native.perf_counters().jit_direct_exit_permission;
    let native_walks_before = native.perf_counters().tlb_walks;
    let interp_walks_before = interp.perf_counters().tlb_walks;

    native.cycle_no_interrupt_check(&mut native_bus).unwrap();
    interp.cycle_no_interrupt_check(&mut interp_bus).unwrap();
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    interp.cycle_no_interrupt_check(&mut interp_bus).unwrap();
    interp.cycle_no_interrupt_check(&mut interp_bus).unwrap();
    assert_eq!(native.registers.eip, SOURCE + 3);
    assert_eq!(native.registers.eip, interp.registers.eip);
    assert_eq!(native.perf_counters().instructions - guest_before, 3);
    assert_eq!(interp.perf_counters().instructions - interp_guest_before, 3);
    assert_eq!(native.perf_counters().tlb_walks, native_walks_before);
    assert_eq!(interp.perf_counters().tlb_walks, interp_walks_before);
    assert!(
        native_bus
            .trace
            .cycles()
            .iter()
            .all(|cycle| cycle.kind != BusAccessKind::DataRead || cycle.address != DATA)
    );

    native.jit_fast_map.invalidate_page(DATA);
    let interp_error = run_to_error(&mut interp, &mut interp_bus);
    let native_error = run_to_error(&mut native, &mut native_bus);

    assert_eq!(native_error, interp_error);
    assert!(matches!(
        native_error,
        CpuRunError {
            error: CpuError::TripleFault {
                original_vector: 14,
                nested_vector: 11,
            },
            consumed_core_clocks: 0,
        }
    ));
    assert_eq!(native.control.cr2, DATA);
    assert_eq!(native.control.cr2, interp.control.cr2);
    assert_eq!(native.perf_counters().instructions - guest_before, 3);
    assert_eq!(interp.perf_counters().instructions - interp_guest_before, 3);
    assert_eq!(native.perf_counters().tlb_walks - native_walks_before, 1);
    assert_eq!(interp.perf_counters().tlb_walks - interp_walks_before, 1);
    assert_eq!(
        crate::tests::settled_state(&native),
        crate::tests::settled_state(&interp)
    );
    assert_eq!(native_bus.memory, interp_bus.memory);
    assert_eq!(native_bus.trace.cycles(), interp_bus.trace.cycles());
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
    let page_walk_reads = native_bus
        .trace
        .cycles()
        .iter()
        .filter(|cycle| cycle.kind == BusAccessKind::PageWalkRead)
        .map(|cycle| (cycle.address, cycle.width, cycle.clocks))
        .collect::<Vec<_>>();
    assert_eq!(
        page_walk_reads,
        vec![(0x3000, BusWidth::Dword, 2), (0x4004, BusWidth::Dword, 2),]
    );
    let page_walk_writes = native_bus
        .trace
        .cycles()
        .iter()
        .filter(|cycle| cycle.kind == BusAccessKind::PageWalkWrite)
        .map(|cycle| (cycle.address, cycle.width, cycle.clocks))
        .collect::<Vec<_>>();
    assert!(page_walk_writes.is_empty());
    assert_eq!(
        u32::from_le_bytes(native_bus.memory[0x4004..0x4008].try_into().unwrap()),
        0x1003
    );
    assert!(
        native_bus
            .trace
            .cycles()
            .iter()
            .all(|cycle| cycle.kind != BusAccessKind::DataRead || cycle.address != DATA)
    );
    assert_eq!(native.perf_counters().jit_direct_insns - direct_before, 2);
    assert_eq!(
        native.perf_counters().jit_direct_side_exits - exits_before,
        1
    );
    assert_eq!(
        native.perf_counters().jit_direct_exit_permission - permission_before,
        1
    );
}

#[test]
fn crossing_word_memory_jump_faults_on_the_first_unmapped_page_in_replay_order() {
    const OPERAND: u32 = 0x1fff;
    for first_present in [false, true] {
        let mut memory = vec![0; 0x7000];
        memory[SOURCE as usize..SOURCE as usize + 5]
            .copy_from_slice(&[0x90, 0x90, 0x90, 0xff, 0x27]);
        memory[0x1fff] = TARGET as u8;
        memory[0x2000] = (TARGET >> 8) as u8;
        memory[0x3000..0x3004].copy_from_slice(&0x4007u32.to_le_bytes());
        memory[0x4000..0x4004].copy_from_slice(&0x0007u32.to_le_bytes());
        memory[0x4004..0x4008]
            .copy_from_slice(&(if first_present { 0x1007u32 } else { 0x1006u32 }).to_le_bytes());
        memory[0x4008..0x400c]
            .copy_from_slice(&(if first_present { 0x2006u32 } else { 0x2007u32 }).to_le_bytes());

        let mut interp = paged_word_cpu(SOURCE);
        let mut native = paged_word_cpu(SOURCE);
        let mut interp_bus = sixteen_bit_bus(memory.clone());
        let mut native_bus = sixteen_bit_bus(memory);
        for (cpu, bus) in [
            (&mut interp, &mut interp_bus),
            (&mut native, &mut native_bus),
        ] {
            warm_sixteen_bit(cpu, bus, &[SOURCE, SOURCE + 1, SOURCE + 2, SOURCE + 3]);
        }
        arm_native_sixteen_bit(&mut native, &mut native_bus, &[0]);
        let block = install_word_jmp(&mut native, SOURCE + 1);
        assert_eq!(block.span().instructions, 3);
        native.set_jit_auto_admit(true);

        for bus in [&mut interp_bus, &mut native_bus] {
            bus.direct_pages_enabled = false;
            bus.side_effect_read_address = Some(0x1fff);
            bus.side_effect_read_count = 0;
            bus.trace.clear();
        }
        for cpu in [&mut interp, &mut native] {
            cpu.set_eip(SOURCE);
            cpu.registers.set_ebx(OPERAND);
            cpu.registers.eflags = 0x246;
            cpu.pending_flags = PendingFlags::default();
            cpu.elapsed_clocks = 0;
            cpu.timing_rem = 0;
            cpu.core_clocks_so_far = 0;
        }
        let direct_before = native.perf_counters().jit_direct_insns;
        let guest_before = native.perf_counters().instructions;
        let interp_guest_before = interp.perf_counters().instructions;
        let exits_before = native.perf_counters().jit_direct_side_exits;
        let crossing_before = native
            .perf_counters()
            .jit_direct_exit_cross_page_or_alignment;
        let native_walks_before = native.perf_counters().tlb_walks;
        let interp_walks_before = interp.perf_counters().tlb_walks;

        let interp_error = run_to_error(&mut interp, &mut interp_bus);
        let native_error = run_to_error(&mut native, &mut native_bus);
        let expected_cr2 = if first_present { 0x2000 } else { 0x1fff };

        assert_eq!(native_error, interp_error);
        assert!(
            matches!(
                native_error,
                CpuRunError {
                    error: CpuError::TripleFault {
                        original_vector: 14,
                        nested_vector: 11,
                    },
                    consumed_core_clocks: 3,
                }
            ),
            "first_present={first_present}: {native_error:?}"
        );
        assert_eq!(native.control.cr2, expected_cr2);
        assert_eq!(native.control.cr2, interp.control.cr2);
        assert_eq!(
            crate::tests::settled_state(&native),
            crate::tests::settled_state(&interp)
        );
        assert_eq!(native_bus.memory, interp_bus.memory);
        assert_eq!(native_bus.trace.cycles(), interp_bus.trace.cycles());
        assert_eq!(native_bus.side_effect_read_count, u64::from(first_present));
        assert_eq!(
            native_bus.side_effect_read_count,
            interp_bus.side_effect_read_count
        );
        assert_eq!(native.perf_counters().instructions - guest_before, 3);
        assert_eq!(interp.perf_counters().instructions - interp_guest_before, 3);
        let expected_walks = if first_present { 3 } else { 1 };
        assert_eq!(
            native.perf_counters().tlb_walks - native_walks_before,
            expected_walks
        );
        assert_eq!(
            interp.perf_counters().tlb_walks - interp_walks_before,
            expected_walks
        );
        let page_walk_reads = native_bus
            .trace
            .cycles()
            .iter()
            .filter(|cycle| cycle.kind == BusAccessKind::PageWalkRead)
            .map(|cycle| (cycle.address, cycle.width, cycle.clocks))
            .collect::<Vec<_>>();
        let expected_reads = if first_present {
            vec![
                (0x3000, BusWidth::Dword, 2),
                (0x4004, BusWidth::Dword, 2),
                (0x3000, BusWidth::Dword, 2),
                (0x4008, BusWidth::Dword, 2),
                (0x3000, BusWidth::Dword, 2),
                (0x4000, BusWidth::Dword, 2),
            ]
        } else {
            vec![(0x3000, BusWidth::Dword, 2), (0x4004, BusWidth::Dword, 2)]
        };
        assert_eq!(page_walk_reads, expected_reads);
        let page_walk_writes = native_bus
            .trace
            .cycles()
            .iter()
            .filter(|cycle| cycle.kind == BusAccessKind::PageWalkWrite)
            .map(|cycle| (cycle.address, cycle.width, cycle.clocks))
            .collect::<Vec<_>>();
        assert_eq!(
            page_walk_writes,
            if first_present {
                vec![(0x4004, BusWidth::Dword, 2)]
            } else {
                Vec::new()
            }
        );
        assert_eq!(
            native_bus
                .trace
                .cycles()
                .iter()
                .filter(|cycle| {
                    cycle.kind == BusAccessKind::DataRead
                        && matches!(cycle.address, 0x1fff | 0x2000)
                })
                .map(|cycle| (cycle.address, cycle.width))
                .collect::<Vec<_>>(),
            if first_present {
                vec![(0x1fff, BusWidth::Byte)]
            } else {
                Vec::new()
            }
        );
        assert_eq!(
            u32::from_le_bytes(native_bus.memory[0x4004..0x4008].try_into().unwrap()),
            if first_present { 0x1027 } else { 0x1006 }
        );
        assert_eq!(
            u32::from_le_bytes(native_bus.memory[0x4008..0x400c].try_into().unwrap()),
            if first_present { 0x2006 } else { 0x2007 }
        );
        assert_eq!(native.perf_counters().jit_direct_insns - direct_before, 2);
        assert_eq!(
            native.perf_counters().jit_direct_side_exits - exits_before,
            1
        );
        assert_eq!(
            native
                .perf_counters()
                .jit_direct_exit_cross_page_or_alignment
                - crossing_before,
            1
        );
    }
}
