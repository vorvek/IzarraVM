// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const ENTRY: u32 = 0x100;
const DATA: usize = 0x200;

fn x87_cpu(mode: GswMode) -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.jit_direct.set_fast_map_enabled_for_test(true);
    cpu.set_mode(mode);
    for segment in [
        SegmentIndex::Cs,
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
    ] {
        cpu.load_segment_real(segment, 0);
        let mut descriptor = cpu.registers.segment(segment);
        descriptor.base = 0;
        descriptor.limit = u32::MAX;
        cpu.registers.set_segment(segment, descriptor);
    }
    let mut cs = cpu.registers.cs();
    cs.default_size_32 = true;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    cpu
}

fn arm(cpu: &mut CpuGsw, control: u16) {
    cpu.registers.eip = ENTRY - 1;
    cpu.registers.gpr.fill(0);
    cpu.registers.eflags = 2;
    cpu.fpu = X87::default();
    cpu.fpu.control = control;
    cpu.halted = false;
    cpu.elapsed_clocks = 0;
    cpu.core_clocks_so_far = 0;
    cpu.timing_rem = 0;
    cpu.fp_rem = 3;
    cpu.pending_flags = PendingFlags::default();
    cpu.interrupt_shadow = false;
}

fn run_to_halt(cpu: &mut CpuGsw, bus: &mut TestBus) -> Vec<(u32, u32, bool)> {
    let mut outcomes = Vec::new();
    for _ in 0..32 {
        let outcome = cpu.run_straight_line(bus, u64::MAX).unwrap();
        outcomes.push((outcome.core_clocks, cpu.registers.eip, outcome.halted));
        if outcome.halted {
            return outcomes;
        }
    }
    panic!("x87 test program did not halt");
}

fn direct_memory(mut memory: Vec<u8>) -> TestBus {
    if memory.len() < 0x1000 {
        memory.resize(0x1000, 0);
    }
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus
}

fn assert_program_matches(mode: GswMode, memory: Vec<u8>, control: u16) -> (CpuGsw, TestBus) {
    let mut direct = x87_cpu(mode);
    let mut interpreter = x87_cpu(mode);
    let mut direct_bus = direct_memory(memory.clone());
    let mut interpreter_bus = direct_memory(memory.clone());

    arm(&mut direct, control);
    run_to_halt(&mut direct, &mut direct_bus);
    arm(&mut interpreter, control);
    run_to_halt(&mut interpreter, &mut interpreter_bus);

    direct.set_jit_auto_admit(true);
    arm(&mut direct, control);
    direct_bus.memory.copy_from_slice(&memory);
    direct_bus.trace = BusTrace::default();
    run_to_halt(&mut direct, &mut direct_bus);

    arm(&mut direct, control);
    arm(&mut interpreter, control);
    direct_bus.memory.copy_from_slice(&memory);
    direct_bus.trace = BusTrace::default();
    interpreter_bus.memory.copy_from_slice(&memory);
    interpreter_bus.trace = BusTrace::default();
    let before = direct.perf_counters().jit_direct_insns;
    let direct_outcomes = run_to_halt(&mut direct, &mut direct_bus);
    let interpreter_outcomes = run_to_halt(&mut interpreter, &mut interpreter_bus);

    assert_eq!(direct_outcomes, interpreter_outcomes, "run timing differs");
    assert_eq!(direct.registers, interpreter.registers, "registers differ");
    assert_eq!(direct.fpu, interpreter.fpu, "x87 state differs");
    assert_eq!(direct.pending_flags, interpreter.pending_flags);
    assert_eq!(direct.elapsed_clocks, interpreter.elapsed_clocks);
    assert_eq!(direct.timing_rem, interpreter.timing_rem);
    assert_eq!(direct.fp_rem, interpreter.fp_rem);
    assert_eq!(direct_bus.memory, interpreter_bus.memory, "memory differs");
    assert_eq!(
        direct_bus.trace.elapsed_clocks(),
        interpreter_bus.trace.elapsed_clocks(),
        "bus timing differs"
    );
    assert!(
        direct.perf_counters().jit_direct_insns > before,
        "the x87 sequence did not run natively: {:?}",
        direct.perf_counters()
    );
    (direct, direct_bus)
}

fn quake_hot_program() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    let code = [
        0xd9, 0x05, 0x00, 0x02, 0x00, 0x00, // fld dword [0x200]
        0xd8, 0x0d, 0x04, 0x02, 0x00, 0x00, // fmul dword [0x204]
        0xdb, 0x05, 0x08, 0x02, 0x00, 0x00, // fild dword [0x208]
        0xd8, 0xc1, // fadd st(0),st(1)
        0xde, 0xc1, // faddp st(1),st(0)
        0xd9, 0x15, 0x0c, 0x02, 0x00, 0x00, // fst dword [0x20c]
        0xdb, 0x1d, 0x10, 0x02, 0x00, 0x00, // fistp dword [0x210]
        0xdf, 0xe0, // fnstsw ax
        0x89, 0xc2, // mov edx,eax
        0xf4, // hlt
    ];
    memory[ENTRY as usize - 1] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 4].copy_from_slice(&1.5f32.to_bits().to_le_bytes());
    memory[DATA + 4..DATA + 8].copy_from_slice(&2.0f32.to_bits().to_le_bytes());
    memory[DATA + 8..DATA + 12].copy_from_slice(&4i32.to_le_bytes());
    memory
}

fn oversized_x87_program() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let mut code = Vec::new();
    for _ in 0..8 {
        code.extend_from_slice(&[0xd9, 0x05, 0x00, 0x02, 0x00, 0x00]); // fld dword [0x200]
    }
    for _ in 0..4 {
        code.extend_from_slice(&[0x89, 0xc0]); // mov eax,eax
    }
    code.push(0xf4);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 4].copy_from_slice(&1.5f32.to_bits().to_le_bytes());
    memory
}

#[test]
fn oversized_x87_block_compiles_and_runs_as_fitting_prefixes() {
    const FULL_BLOCK_INSTRUCTIONS: usize = 12;
    let memory = oversized_x87_program();
    let mut cpu = x87_cpu(GswMode::Gsw586);
    let mut bus = direct_memory(memory.clone());
    arm(&mut cpu, 0x0f7f);
    run_to_halt(&mut cpu, &mut bus);

    arm(&mut cpu, 0x0f7f);
    let full = jit::direct::compile_with_instruction_limit_for_test(
        &mut cpu,
        ENTRY,
        true,
        FULL_BLOCK_INSTRUCTIONS,
    )
    .expect("unrestricted x87 block");
    assert_eq!(usize::from(full.span.instructions), FULL_BLOCK_INSTRUCTIONS);
    assert!(full.code.len() > jit::exec_mem::host_page_len());

    let compilation = jit::direct::compile(&mut cpu, ENTRY, true).expect("fitting x87 prefix");
    assert!((3..FULL_BLOCK_INSTRUCTIONS).contains(&usize::from(compilation.span.instructions)));
    assert!(compilation.code.len() <= jit::exec_mem::host_page_len());
    let retained = usize::from(compilation.span.instructions);
    let instruction_lens = [6u8, 6, 6, 6, 6, 6, 6, 6, 2, 2, 2, 2];
    assert_eq!(
        usize::from(compilation.span.guest_len),
        instruction_lens[..retained]
            .iter()
            .map(|&len| usize::from(len))
            .sum::<usize>()
    );
    assert_eq!(
        &compilation.fetch_lens[..retained],
        &instruction_lens[..retained]
    );
    assert!(
        compilation.fetch_lens[retained..]
            .iter()
            .all(|&len| len == 0)
    );

    let (direct, _) = assert_program_matches(GswMode::Gsw586, memory, 0x0f7f);
    assert_eq!(direct.fpu.top(), 0);
    assert_eq!(direct.fpu.tag, 0);
}

#[test]
fn quake_hot_x87_sequence_matches_interpreter_in_486_and_586_modes() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let (cpu, bus) = assert_program_matches(mode, quake_hot_program(), 0x0f7f);
        assert_eq!(
            f32::from_bits(u32::from_le_bytes(
                bus.memory[DATA + 12..DATA + 16].try_into().unwrap()
            )),
            10.0,
            "{mode:?}"
        );
        assert_eq!(
            i32::from_le_bytes(bus.memory[DATA + 16..DATA + 20].try_into().unwrap()),
            10,
            "{mode:?}"
        );
        assert_eq!(cpu.fpu.tag, 0xffff, "{mode:?}");
        assert_eq!(cpu.registers.edx(), u32::from(cpu.fpu.status));
    }
}

#[test]
fn linked_x87_blocks_keep_stack_state_resident_and_validate_root_top() {
    const SECOND: u32 = ENTRY + 24;
    const END: u32 = SECOND + 28;
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let first = [
        0x89, 0xc0, 0x89, 0xc0, 0x89, 0xc0, 0x89, 0xc0, 0x89, 0xc0, 0x89, 0xc0, 0x89, 0xc0, 0x89,
        0xc0, 0x89, 0xc0, 0x89, 0xc0, 0x89, 0xc0, // eleven mov eax,eax
        0xd9, 0xe8, // fld1
    ];
    let second = [
        0xd9, 0x1d, 0x00, 0x02, 0x00, 0x00, // fstp dword [0x200]
        0x89, 0xdb, 0x89, 0xdb, 0x89, 0xdb, 0x89, 0xdb, 0x89, 0xdb, 0x89, 0xdb, 0x89, 0xdb, 0x89,
        0xdb, 0x89, 0xdb, 0x89, 0xdb, 0x89, 0xdb, // eleven mov ebx,ebx
    ];
    memory[ENTRY as usize..SECOND as usize].copy_from_slice(&first);
    memory[SECOND as usize..END as usize].copy_from_slice(&second);
    memory[END as usize] = 0xf4;

    let mut native = x87_cpu(GswMode::Gsw586);
    let mut interpreter = x87_cpu(GswMode::Gsw586);
    let mut native_bus = direct_memory(memory.clone());
    let mut interpreter_bus = direct_memory(memory.clone());
    arm(&mut native, 0x0f7f);
    run_to_halt(&mut native, &mut native_bus);
    arm(&mut interpreter, 0x0f7f);
    run_to_halt(&mut interpreter, &mut interpreter_bus);

    arm(&mut native, 0x0f7f);
    let first_key = jit::direct::key_for(&native, ENTRY, true).expect("first x87 block key");
    assert!(matches!(
        native.jit_direct.probe(first_key),
        jit::direct::BlockProbe::Interpret
    ));
    let first_compilation =
        jit::direct::compile(&mut native, ENTRY, true).expect("first x87 block");
    assert_eq!(first_compilation.span.instructions, 12);
    assert_eq!(first_compilation.x87_entry_top, 0);
    assert_eq!(first_compilation.x87_exit_top, 7);
    let first_id = native
        .jit_direct
        .install(&first_compilation)
        .expect("first x87 block install");
    let first_block = native
        .jit_direct
        .block(first_id)
        .expect("first x87 block remains live");

    native.fpu.dec_top();
    let second_key = jit::direct::key_for(&native, SECOND, true).expect("second x87 block key");
    assert!(matches!(
        native.jit_direct.probe(second_key),
        jit::direct::BlockProbe::Interpret
    ));
    let second_compilation =
        jit::direct::compile(&mut native, SECOND, true).expect("second x87 block");
    assert_eq!(second_compilation.span.instructions, 12);
    assert_eq!(second_compilation.x87_entry_top, 7);
    assert_eq!(second_compilation.x87_exit_top, 0);
    native
        .jit_direct
        .install(&second_compilation)
        .expect("second x87 block install");

    arm(&mut native, 0x0f7f);
    arm(&mut interpreter, 0x0f7f);
    native.registers.eip = ENTRY;
    interpreter.registers.eip = ENTRY;
    native_bus.memory.copy_from_slice(&memory);
    interpreter_bus.memory.copy_from_slice(&memory);
    native_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();
    let entries = native.perf_counters().jit_direct_entries;
    let transfers = native.perf_counters().jit_direct_linked_transfers;
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, first_block)
            .unwrap()
    );
    for _ in 0..24 {
        interpreter.cycle(&mut interpreter_bus).unwrap();
    }

    assert_eq!(native.registers, interpreter.registers);
    assert_eq!(native.fpu, interpreter.fpu);
    assert_eq!(native.pending_flags, interpreter.pending_flags);
    assert_eq!(native.elapsed_clocks, interpreter.elapsed_clocks);
    assert_eq!(native.fp_rem, interpreter.fp_rem);
    assert_eq!(native_bus.memory, interpreter_bus.memory);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interpreter_bus.trace.elapsed_clocks()
    );
    assert_eq!(native.registers.eip, END);
    assert_eq!(native.perf_counters().jit_direct_entries - entries, 1);
    assert_eq!(
        native.perf_counters().jit_direct_linked_transfers - transfers,
        1
    );
    assert_eq!(
        f32::from_bits(u32::from_le_bytes(
            native_bus.memory[DATA..DATA + 4].try_into().unwrap()
        )),
        1.0
    );

    arm(&mut native, 0x0f7f);
    native.registers.eip = ENTRY;
    native.fpu.dec_top();
    let registers = native.registers.clone();
    let fpu = native.fpu.clone();
    let rejects = native.perf_counters().jit_direct_reject_x87_top;
    assert!(
        !native
            .try_run_direct_block_for_test(&mut native_bus, first_block)
            .unwrap()
    );
    assert_eq!(native.registers, registers);
    assert_eq!(native.fpu, fpu);
    assert_eq!(
        native.perf_counters().jit_direct_reject_x87_top - rejects,
        1
    );
}

#[test]
fn x87_self_loop_with_a_net_top_change_stays_interpreted() {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + 7].copy_from_slice(&[
        0xd9, 0xe8, // fld1
        0x89, 0xc0, // mov eax,eax
        0x75, 0xfa, // jnz ENTRY
        0xf4, // hlt
    ]);
    let mut cpu = x87_cpu(GswMode::Gsw586);
    let mut bus = direct_memory(memory);
    arm(&mut cpu, 0x0f7f);
    cpu.registers.eflags |= FLAG_ZF;
    run_to_halt(&mut cpu, &mut bus);

    arm(&mut cpu, 0x0f7f);
    let key = jit::direct::key_for(&cpu, ENTRY, true).expect("x87 loop key");
    assert_eq!(cpu.fpu.top(), 0);
    assert!(jit::direct::compile(&mut cpu, key.linear, true).is_none());
}

fn d8_program(extension: u8, memory_source: bool) -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let mut code = vec![0xd9, 0x05, 0x04, 0x02, 0x00, 0x00]; // fld dword [0x204]
    if memory_source {
        code.extend_from_slice(&[0xd8, (extension << 3) | 5, 0x00, 0x02, 0x00, 0x00]);
    } else {
        code.extend_from_slice(&[0xd9, 0x05, 0x00, 0x02, 0x00, 0x00]);
        code.extend_from_slice(&[0xd8, 0xc1 | (extension << 3)]);
    }
    code.extend_from_slice(&[
        0xdf, 0xe0, // fnstsw ax
        0x89, 0xc2, // mov edx,eax
        0x89, 0xc3, // mov ebx,eax
        0xf4,
    ]);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 4].copy_from_slice(&2.0f32.to_bits().to_le_bytes());
    memory[DATA + 4..DATA + 8].copy_from_slice(&3.0f32.to_bits().to_le_bytes());
    memory
}

#[test]
fn every_d8_memory_and_register_operation_matches_the_interpreter() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for memory_source in [false, true] {
            for extension in 0..=7 {
                let (cpu, _) =
                    assert_program_matches(mode, d8_program(extension, memory_source), 0x0f7f);
                assert_eq!(
                    cpu.perf_counters().jit_direct_side_exits,
                    0,
                    "mode={mode:?} memory={memory_source} extension={extension}"
                );
            }
        }
    }
}

fn stack_transfer_program() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let code = [
        0xd9, 0x05, 0x00, 0x02, 0x00, 0x00, // fld dword [0x200]
        0xd9, 0x05, 0x04, 0x02, 0x00, 0x00, // fld dword [0x204]
        0xd9, 0xc1, // fld st(1)
        0xd9, 0xc9, // fxch st(1)
        0xd9, 0x1d, 0x08, 0x02, 0x00, 0x00, // fstp dword [0x208]
        0xde, 0xc9, // fmulp st(1),st(0)
        0xd9, 0x1d, 0x0c, 0x02, 0x00, 0x00, // fstp dword [0x20c]
        0x89, 0xc0, // mov eax,eax
        0xf4,
    ];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 4].copy_from_slice(&2.0f32.to_bits().to_le_bytes());
    memory[DATA + 4..DATA + 8].copy_from_slice(&3.0f32.to_bits().to_le_bytes());
    memory
}

#[test]
fn fld_register_fxch_fstp_and_fmulp_match_the_interpreter() {
    let (cpu, bus) = assert_program_matches(GswMode::Gsw586, stack_transfer_program(), 0x0f7f);
    assert_eq!(cpu.perf_counters().jit_direct_side_exits, 0);
    assert_eq!(
        f32::from_bits(u32::from_le_bytes(
            bus.memory[DATA + 8..DATA + 12].try_into().unwrap()
        )),
        3.0
    );
    assert_eq!(
        f32::from_bits(u32::from_le_bytes(
            bus.memory[DATA + 12..DATA + 16].try_into().unwrap()
        )),
        4.0
    );
}

fn de_pop_program(extension: u8) -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let code = [
        0xd9,
        0x05,
        0x00,
        0x02,
        0x00,
        0x00, // fld dword [0x200]
        0xd9,
        0x05,
        0x04,
        0x02,
        0x00,
        0x00, // fld dword [0x204]
        0xde,
        0xc1 | (extension << 3),
        0xd9,
        0x1d,
        0x08,
        0x02,
        0x00,
        0x00, // fstp dword [0x208]
        0x89,
        0xc0, // mov eax,eax
        0x89,
        0xdb, // mov ebx,ebx
        0xf4,
    ];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 4].copy_from_slice(&6.0f32.to_bits().to_le_bytes());
    memory[DATA + 4..DATA + 8].copy_from_slice(&2.0f32.to_bits().to_le_bytes());
    memory
}

#[test]
fn every_de_pop_subtract_and_divide_matches_the_interpreter() {
    let expected = [-4.0f32, 4.0, 1.0 / 3.0, 3.0];
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for (extension, expected) in (4..=7).zip(expected) {
            let (cpu, bus) = assert_program_matches(mode, de_pop_program(extension), 0x0f7f);
            let actual = f32::from_bits(u32::from_le_bytes(
                bus.memory[DATA + 8..DATA + 12].try_into().unwrap(),
            ));
            assert_eq!(actual, expected, "mode={mode:?} extension={extension}");
            assert_eq!(
                cpu.perf_counters().jit_direct_side_exits,
                0,
                "mode={mode:?} extension={extension}"
            );
        }
    }
}

fn compare_pop_pop_program(opcode: u8) -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let modrm = if opcode == 0xda { 0xe9 } else { 0xd9 };
    let code = [
        0xd9, 0x05, 0x00, 0x02, 0x00, 0x00, // fld dword [0x200]
        0xd9, 0x05, 0x04, 0x02, 0x00, 0x00, // fld dword [0x204]
        opcode, modrm, 0xdf, 0xe0, // fnstsw ax
        0x89, 0xc2, // mov edx,eax
        0x89, 0xdb, // mov ebx,ebx
        0xf4,
    ];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 4].copy_from_slice(&3.0f32.to_bits().to_le_bytes());
    memory[DATA + 4..DATA + 8].copy_from_slice(&2.0f32.to_bits().to_le_bytes());
    memory
}

#[test]
fn fcompp_and_fucompp_match_the_interpreter_and_pop_twice() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for opcode in [0xde, 0xda] {
            let (cpu, _) = assert_program_matches(mode, compare_pop_pop_program(opcode), 0x0f7f);
            assert_eq!(cpu.fpu.status & 0x4500, 1 << 8, "mode={mode:?}");
            assert_eq!(cpu.fpu.top(), 0, "mode={mode:?}");
            assert_eq!(cpu.fpu.tag, 0xffff, "mode={mode:?}");
            assert_eq!(cpu.perf_counters().jit_direct_side_exits, 0);
        }
    }
}

fn constants_and_register_store_program() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let code = [
        0xd9, 0xe8, // fld1
        0xd9, 0xee, // fldz
        0xdd, 0xd1, // fst st(1)
        0xd9, 0xe8, // fld1
        0xdd, 0xd9, // fstp st(1)
        0xd9, 0x1d, 0x00, 0x02, 0x00, 0x00, // fstp dword [0x200]
        0xd9, 0x1d, 0x04, 0x02, 0x00, 0x00, // fstp dword [0x204]
        0xd9, 0xe8, // fld1
        0xdd, 0xd0, // fst st(0)
        0xdd, 0xd8, // fstp st(0)
        0x89, 0xc0, // mov eax,eax
        0xf4,
    ];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory
}

#[test]
fn fld1_fldz_and_register_stores_match_the_interpreter() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let (cpu, bus) =
            assert_program_matches(mode, constants_and_register_store_program(), 0x0f7f);
        assert_eq!(
            f32::from_bits(u32::from_le_bytes(
                bus.memory[DATA..DATA + 4].try_into().unwrap()
            )),
            1.0,
            "{mode:?}"
        );
        assert_eq!(
            f32::from_bits(u32::from_le_bytes(
                bus.memory[DATA + 4..DATA + 8].try_into().unwrap()
            )),
            0.0,
            "{mode:?}"
        );
        assert_eq!(cpu.fpu.top(), 0, "{mode:?}");
        assert_eq!(cpu.fpu.tag, 0xffff, "{mode:?}");
        assert_eq!(cpu.perf_counters().jit_direct_side_exits, 0);
    }
}

fn cross_page_load_program() -> Vec<u8> {
    let mut memory = vec![0; 0x2000];
    memory[ENTRY as usize - 1] = 0x90;
    let code = [
        0xd9, 0x05, 0xfe, 0x0f, 0x00, 0x00, // fld dword [0xffe]
        0xd9, 0x1d, 0x00, 0x03, 0x00, 0x00, // fstp dword [0x300]
        0x89, 0xc0, // mov eax,eax
        0x89, 0xdb, // mov ebx,ebx
        0xf4,
    ];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[0xffe..0x1002].copy_from_slice(&6.25f32.to_bits().to_le_bytes());
    memory
}

#[test]
fn cross_page_x87_memory_exit_reexecutes_precisely() {
    let (cpu, bus) = assert_program_matches(GswMode::Gsw586, cross_page_load_program(), 0x0f7f);
    assert!(cpu.perf_counters().jit_direct_exit_cross_page_or_alignment > 0);
    assert_eq!(
        f32::from_bits(u32::from_le_bytes(
            bus.memory[0x300..0x304].try_into().unwrap()
        )),
        6.25
    );
}

fn exceptional_divide_program() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let code = [
        0xd9, 0x05, 0x00, 0x02, 0x00, 0x00, // fld 0.0
        0xd9, 0x05, 0x04, 0x02, 0x00, 0x00, // fld 1.0
        0xd8, 0xf1, // fdiv st(0),st(1)
        0xdf, 0xe0, // fnstsw ax
        0x89, 0xc2, // mov edx,eax
        0xf4,
    ];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 4].copy_from_slice(&0.0f32.to_bits().to_le_bytes());
    memory[DATA + 4..DATA + 8].copy_from_slice(&1.0f32.to_bits().to_le_bytes());
    memory
}

fn nearest_fistp_program() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let code = [
        0xd9, 0x05, 0x00, 0x02, 0x00, 0x00, // fld 3.5
        0xdb, 0x1d, 0x04, 0x02, 0x00, 0x00, // fistp dword [0x204]
        0x89, 0xc0, // mov eax,eax
        0x89, 0xdb, // mov ebx,ebx
        0xf4,
    ];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 4].copy_from_slice(&3.5f32.to_bits().to_le_bytes());
    memory
}

#[test]
fn exceptional_arithmetic_and_non_chop_fistp_exit_before_mutation() {
    let (divide_cpu, _) =
        assert_program_matches(GswMode::Gsw586, exceptional_divide_program(), 0x037f);
    assert!(divide_cpu.perf_counters().jit_direct_exit_other > 0);
    assert!(divide_cpu.fpu.get(0).is_infinite());
    assert_ne!(divide_cpu.fpu.status & 0x04, 0);

    let (fist_cpu, fist_bus) =
        assert_program_matches(GswMode::Gsw586, nearest_fistp_program(), 0x037f);
    assert!(fist_cpu.perf_counters().jit_direct_exit_other > 0);
    assert_eq!(
        i32::from_le_bytes(fist_bus.memory[DATA + 4..DATA + 8].try_into().unwrap()),
        4
    );
}

fn gate_program() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let code = [
        0xd9, 0x05, 0x00, 0x02, 0x00, 0x00, // fld dword [0x200]
        0x89, 0xc0, // mov eax,eax
        0x89, 0xdb, // mov ebx,ebx
        0xf4,
    ];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 4].copy_from_slice(&1.0f32.to_bits().to_le_bytes());
    memory
}

#[test]
fn nm_and_mf_gates_match_interpreter_without_touching_x87_or_memory() {
    let memory = gate_program();
    let (mut direct, mut direct_bus) =
        assert_program_matches(GswMode::Gsw586, memory.clone(), 0x037f);

    for pending_mf in [false, true] {
        let mut interpreter = x87_cpu(GswMode::Gsw586);
        let mut interpreter_bus = direct_memory(memory.clone());
        arm(&mut interpreter, 0x037f);
        run_to_halt(&mut interpreter, &mut interpreter_bus);
        arm(&mut direct, 0x037f);
        arm(&mut interpreter, 0x037f);
        direct_bus.memory.copy_from_slice(&memory);
        direct_bus.trace = BusTrace::default();
        interpreter_bus.memory.copy_from_slice(&memory);
        interpreter_bus.trace = BusTrace::default();
        if pending_mf {
            for cpu in [&mut direct, &mut interpreter] {
                cpu.control.cr0 = CR0_NE;
                cpu.fpu.control &= !1;
                cpu.fpu.raise_exception(1);
            }
        } else {
            direct.control.cr0 = CR0_TS;
            interpreter.control.cr0 = CR0_TS;
        }
        let direct_fpu = direct.fpu.clone();
        let direct_memory = direct_bus.memory.clone();
        let direct_result = direct.run_straight_line(&mut direct_bus, u64::MAX);
        let interpreter_result = interpreter.run_straight_line(&mut interpreter_bus, u64::MAX);
        assert_eq!(direct_result, interpreter_result, "pending_mf={pending_mf}");
        assert_eq!(direct.registers, interpreter.registers);
        assert_eq!(direct.fpu, interpreter.fpu);
        assert_eq!(direct.fpu, direct_fpu);
        assert_eq!(direct_bus.memory, interpreter_bus.memory);
        assert_eq!(direct_bus.memory, direct_memory);
        assert_eq!(direct.elapsed_clocks, interpreter.elapsed_clocks);
        assert_eq!(direct.fp_rem, interpreter.fp_rem);
    }
}

fn conversion_loop_program() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    let code = [
        0xdb, 0x05, 0x00, 0x02, 0x00, 0x00, // fild dword [0x200]
        0xdb, 0x1d, 0x04, 0x02, 0x00, 0x00, // fistp dword [0x204]
        0x83, 0xe9, 0x01, // sub ecx,1
        0x75, 0xef, // jnz 0x100
        0xf4,
    ];
    memory[ENTRY as usize - 1] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 4].copy_from_slice(&123_456i32.to_le_bytes());
    memory
}

#[test]
fn x87_conversion_self_loop_respects_a_tight_event_cap() {
    let memory = conversion_loop_program();
    let mut direct = x87_cpu(GswMode::Gsw586);
    arm(&mut direct, 0x0f7f);
    direct.registers.set_ecx(1);
    let mut direct_bus = direct_memory(memory.clone());
    run_to_halt(&mut direct, &mut direct_bus);

    direct.set_jit_auto_admit(true);
    for _ in 0..2 {
        arm(&mut direct, 0x0f7f);
        direct.registers.set_ecx(1);
        direct_bus.memory.copy_from_slice(&memory);
        direct_bus.trace = BusTrace::default();
        run_to_halt(&mut direct, &mut direct_bus);
    }
    let key = jit::direct::key_for(&direct, ENTRY, true).expect("decoded x87 loop");
    for linear in [ENTRY, ENTRY + 6, ENTRY + 12, ENTRY + 15] {
        assert!(
            direct.decode_cache.get(linear, true).is_some(),
            "missing decoded instruction at {linear:#x}"
        );
    }
    let probe = direct.jit_direct.probe(key);
    let jit::direct::BlockProbe::Ready(id) = probe else {
        panic!(
            "x87 loop was not installed: probe={probe:?}, tracked={}, live={}, perf={:?}",
            direct.jit_direct.tracked_len(),
            direct.jit_direct.len(),
            direct.perf_counters()
        )
    };
    let block = direct.jit_direct.block(id).expect("resident x87 loop");

    arm(&mut direct, 0x0f7f);
    direct.registers.eip = ENTRY;
    direct.registers.set_ecx(3);
    let before_registers = direct.registers.clone();
    let before_fpu = direct.fpu.clone();
    let before_fp_rem = direct.fp_rem;
    direct_bus.memory.copy_from_slice(&memory);
    direct_bus.trace = BusTrace::default();
    assert!(
        !direct
            .try_run_direct_block_with_cap_for_test(&mut direct_bus, block, 10)
            .unwrap()
    );
    assert_eq!(direct.registers, before_registers);
    assert_eq!(direct.fpu, before_fpu);
    assert_eq!(direct.fp_rem, before_fp_rem);
    assert_eq!(direct_bus.memory, memory);

    let mut interpreter = x87_cpu(GswMode::Gsw586);
    arm(&mut interpreter, 0x0f7f);
    interpreter.registers.eip = ENTRY;
    interpreter.registers.set_ecx(3);
    let mut interpreter_bus = direct_memory(memory);
    assert!(
        direct
            .try_run_direct_block_with_cap_for_test(&mut direct_bus, block, 81)
            .unwrap()
    );
    for _ in 0..4 {
        interpreter.cycle(&mut interpreter_bus).unwrap();
    }
    assert_eq!(direct.registers, interpreter.registers);
    assert_eq!(direct.fpu, interpreter.fpu);
    assert_eq!(direct.elapsed_clocks, interpreter.elapsed_clocks);
    assert_eq!(direct.timing_rem, interpreter.timing_rem);
    assert_eq!(direct.fp_rem, interpreter.fp_rem);
    assert_eq!(direct_bus.memory, interpreter_bus.memory);
}
