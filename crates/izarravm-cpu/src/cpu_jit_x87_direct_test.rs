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

fn integer_only_program() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let code = [
        0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax,1
        0x83, 0xc0, 0x02, // add eax,2
        0x83, 0xc0, 0x03, // add eax,3
        0xf4, // hlt
    ];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory
}

// Locates a REX.W 81 /r prologue-or-epilogue immediate (sub rsp is /5, add rsp is /0) and
// returns the imm32 that follows it. Guest ALU code never targets RSP with a 64-bit
// immediate op, so this three-byte prefix is unique to the frame setup and teardown.
fn imm32_after(code: &[u8], prefix: [u8; 3]) -> u32 {
    let at = code
        .windows(prefix.len())
        .position(|window| window == prefix)
        .unwrap_or_else(|| panic!("prefix {prefix:02x?} not found in emitted code"));
    let imm_at = at + prefix.len();
    u32::from_le_bytes(code[imm_at..imm_at + 4].try_into().unwrap())
}

fn frame_setup_and_teardown_immediates(
    memory: Vec<u8>,
    control: u16,
    expect_x87: bool,
) -> (u32, u32) {
    let mut cpu = x87_cpu(GswMode::Gsw586);
    let mut bus = direct_memory(memory);
    arm(&mut cpu, control);
    run_to_halt(&mut cpu, &mut bus);

    arm(&mut cpu, control);
    let compilation = jit::direct::compile(&mut cpu, ENTRY, true).expect("block compiles");
    assert_eq!(
        compilation.has_x87, expect_x87,
        "test fixture drifted: this block's x87-bearing-ness no longer matches what the test \
         means to compare, which would make this test compare two integer (or two x87) frames \
         against each other and pass trivially"
    );
    let sub_rsp = imm32_after(&compilation.code, [0x48, 0x81, 0xec]);
    let add_rsp = imm32_after(&compilation.code, [0x48, 0x81, 0xc4]);
    (sub_rsp, add_rsp)
}

// A chained native transfer jumps straight into a target block's body, skipping its own
// prologue, so the target's epilogue always tears down whatever frame the entering block's
// prologue built. That only works if every block, x87-bearing or not, builds the same frame
// shape: the same sub rsp immediate in the prologue as the add rsp immediate in the
// epilogue, and that same immediate across both kinds of block.
#[test]
fn native_frame_size_matches_between_x87_and_integer_blocks() {
    let (x87_sub, x87_add) = frame_setup_and_teardown_immediates(quake_hot_program(), 0x0f7f, true);
    let (int_sub, int_add) =
        frame_setup_and_teardown_immediates(integer_only_program(), 0x0f7f, false);

    assert_eq!(
        x87_sub, x87_add,
        "x87 block: prologue sub rsp must match epilogue add rsp"
    );
    assert_eq!(
        int_sub, int_add,
        "integer block: prologue sub rsp must match epilogue add rsp"
    );
    assert_eq!(
        x87_sub, int_sub,
        "x87 and integer blocks must emit the same native frame size"
    );
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

// The x87 link-relaxation slice: a float block statically chained into a pure integer block
// (no x87 opcodes anywhere in it) must still cross natively, and the boundary spill it triggers
// must leave CpuGsw.fpu exactly where the interpreter would. Second block never calls
// emit_x87_enter or emit_x87_spill itself (has_x87 is false for it), so if the source's own
// jump does not flush the physical x87 cache and packed status/tag first, cpu.fpu stays at
// whatever it was before this native call started, not what fld1 actually produced.
#[test]
fn linked_float_to_integer_chain_spills_the_boundary_and_matches_the_interpreter() {
    const SECOND: u32 = ENTRY + 24;
    const END: u32 = SECOND + 6;
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let first = [
        0x89, 0xc0, 0x89, 0xc0, 0x89, 0xc0, 0x89, 0xc0, 0x89, 0xc0, 0x89, 0xc0, 0x89, 0xc0, 0x89,
        0xc0, 0x89, 0xc0, 0x89, 0xc0, 0x89, 0xc0, // eleven mov eax,eax
        0xd9, 0xe8, // fld1
    ];
    // Pure integer, no x87 opcode anywhere in this block. A non-terminal block needs at least
    // three instructions to be worth compiling at all (see compile_with_instruction_limit's
    // `slots.len() < 3` guard), hence three, not one.
    let second = [
        0x89, 0xdb, 0x89, 0xdb, 0x89, 0xdb, // mov ebx,ebx x3
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
    let first_key = jit::direct::key_for(&native, ENTRY, true).expect("first block key");
    assert!(matches!(
        native.jit_direct.probe(first_key),
        jit::direct::BlockProbe::Interpret
    ));
    let first_compilation =
        jit::direct::compile(&mut native, ENTRY, true).expect("first x87 block");
    assert_eq!(first_compilation.span.instructions, 12);
    assert!(first_compilation.has_x87);
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

    let second_key = jit::direct::key_for(&native, SECOND, true).expect("second block key");
    assert!(matches!(
        native.jit_direct.probe(second_key),
        jit::direct::BlockProbe::Interpret
    ));
    let second_compilation =
        jit::direct::compile(&mut native, SECOND, true).expect("second integer block");
    assert!(
        !second_compilation.has_x87,
        "second block must stay pure integer for this to test the relaxed edge"
    );
    native
        .jit_direct
        .install(&second_compilation)
        .expect("second integer block install");

    arm(&mut native, 0x0f7f);
    arm(&mut interpreter, 0x0f7f);
    native.registers.eip = ENTRY;
    interpreter.registers.eip = ENTRY;
    native_bus.memory.copy_from_slice(&memory);
    interpreter_bus.memory.copy_from_slice(&memory);
    native_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();
    let transfers = native.perf_counters().jit_direct_linked_transfers;
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, first_block)
            .unwrap()
    );
    for _ in 0..15 {
        interpreter.cycle(&mut interpreter_bus).unwrap();
    }

    assert_eq!(native.registers, interpreter.registers);
    assert_eq!(
        native.fpu, interpreter.fpu,
        "x87 register file, status and tag words must match: the boundary spill must have run"
    );
    assert_eq!(native.pending_flags, interpreter.pending_flags);
    assert_eq!(native.elapsed_clocks, interpreter.elapsed_clocks);
    assert_eq!(native.fp_rem, interpreter.fp_rem);
    assert_eq!(native_bus.memory, interpreter_bus.memory);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interpreter_bus.trace.elapsed_clocks()
    );
    assert_eq!(native.registers.eip, END);
    assert_eq!(
        native.perf_counters().jit_direct_linked_transfers - transfers,
        1,
        "the chain must actually cross natively, not fall back through the interpreter"
    );
}

// The differential test above proves CpuGsw.fpu ends up correct after a float-to-integer
// crossing, but that alone does not prove the crossing restores RSI and XMM6-11 before handing
// control to the integer target. Deleting that restore while keeping the spill would still pass
// the differential test, since CpuGsw.fpu does not carry host RSI or XMM6-11, yet it would hand
// back a corrupted RSI and XMM6-11 to the Rust caller. Those are Windows callee-saved registers,
// so whether a Rust test would even notice depends on whether rustc happened to keep a live value
// in them across the call, which a debug build rarely does. Planting sentinel values in RSI and
// XMM6-11 and checking they survive the call would need those registers to stay live, untouched
// by rustc, across the call boundary, which is not something safe portable Rust can pin down. So
// this checks the emitted byte order directly instead: the spill, then the RSI restore, then the
// XMM6-11 restore, must all precede the jump that hands control to the integer target. It builds
// each instruction's expected bytes by emitting it in isolation (the load/store instructions with
// a placeholder displacement, since the frame offset itself is not this test's concern) and
// searches for that exact byte sequence in the real compiled code.
#[cfg(target_os = "windows")]
#[test]
fn float_to_integer_boundary_restores_rsi_and_xmm_before_the_jump() {
    const SECOND: u32 = ENTRY + 24;
    const END: u32 = SECOND + 6;
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let first = [
        0x89, 0xc0, 0x89, 0xc0, 0x89, 0xc0, 0x89, 0xc0, 0x89, 0xc0, 0x89, 0xc0, 0x89, 0xc0, 0x89,
        0xc0, 0x89, 0xc0, 0x89, 0xc0, 0x89, 0xc0, // eleven mov eax,eax
        0xd9, 0xe8, // fld1
    ];
    let second = [0x89, 0xdb, 0x89, 0xdb, 0x89, 0xdb]; // mov ebx,ebx x3, pure integer
    memory[ENTRY as usize..SECOND as usize].copy_from_slice(&first);
    memory[SECOND as usize..END as usize].copy_from_slice(&second);
    memory[END as usize] = 0xf4;

    let mut cpu = x87_cpu(GswMode::Gsw586);
    let mut bus = direct_memory(memory);
    arm(&mut cpu, 0x0f7f);
    run_to_halt(&mut cpu, &mut bus);

    arm(&mut cpu, 0x0f7f);
    let compilation = jit::direct::compile(&mut cpu, ENTRY, true).expect("float source compiles");
    assert!(compilation.has_x87);
    let code = &compilation.code;

    fn without_trailing_disp32(bytes: Vec<u8>) -> Vec<u8> {
        bytes[..bytes.len() - 4].to_vec()
    }
    fn first_position(haystack: &[u8], needle: &[u8]) -> usize {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
            .unwrap_or_else(|| panic!("pattern {needle:02x?} not found in emitted code"))
    }

    let mut probe = jit::encoder::Encoder::new();
    probe.load_r64_disp32(jit::encoder::Reg::RSI, jit::encoder::Reg::RSP, 0);
    let rsi_restore = without_trailing_disp32(probe.finish());

    let mut probe = jit::encoder::Encoder::new();
    probe.vmovupd_xmm_disp32(jit::encoder::Xmm::XMM6, jit::encoder::Reg::RSP, 0);
    let xmm_restore = without_trailing_disp32(probe.finish());

    let mut probe = jit::encoder::Encoder::new();
    probe.vzeroupper();
    let spill_end = probe.finish();

    let mut probe = jit::encoder::Encoder::new();
    probe.jmp_r64(jit::encoder::Reg::RDX);
    let transfer_jump = probe.finish();

    let spill_pos = first_position(code, &spill_end);
    let rsi_pos = first_position(code, &rsi_restore);
    let xmm_pos = first_position(code, &xmm_restore);
    let jmp_pos = first_position(code, &transfer_jump);
    assert!(
        spill_pos < rsi_pos && rsi_pos < xmm_pos && xmm_pos < jmp_pos,
        "the float-to-integer boundary must spill the x87 cache, then restore RSI, then restore \
         XMM6-11, then jump to the integer target, in that order: got spill {spill_pos}, rsi \
         {rsi_pos}, xmm {xmm_pos}, jmp {jmp_pos}"
    );
}

// The reverse edge: an integer source must never chain into a float target. Its own prologue
// (emit_x87_enter) sits above body_offset, so a chained jump into its body would skip loading
// the physical x87 cache from CpuGsw.fpu and would run against an unpinned compile-time TOP.
// This does not assert on has_linked_successor directly: that would catch a permissive mutation
// before it ever runs, which is a weaker guarantee than proving the actual consequence. Instead
// it always finishes the guest program (crossing natively if the two blocks turned out to be
// linked, or through the target's own entry point if not) and compares the resulting fp register
// file against the interpreter, so a wrongly-permitted link is caught by the corrupted state it
// produces, not merely by a boolean.
#[test]
fn linked_integer_to_float_chain_is_refused_and_the_float_block_still_runs_correctly() {
    const SECOND: u32 = ENTRY + 6;
    const END: u32 = SECOND + 6;
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let first = [0x89, 0xc0, 0x89, 0xc0, 0x89, 0xc0]; // three mov eax,eax -- pure integer
    // Three instructions, same reason as the float-to-integer test's second block: a
    // non-terminal block needs at least three to be worth compiling at all.
    let second = [0xd9, 0xe8, 0xd9, 0xe8, 0xde, 0xc9]; // fld1; fld1; fmulp st(1),st(0)
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
    let first_key = jit::direct::key_for(&native, ENTRY, true).expect("first block key");
    assert!(matches!(
        native.jit_direct.probe(first_key),
        jit::direct::BlockProbe::Interpret
    ));
    // Force the split right after the third mov, so the block stays pure integer and its
    // fallthrough successor lands exactly on the float block that follows. Nothing about this
    // program would naturally split here: no cap or terminal opcode does it on its own.
    let first_compilation =
        jit::direct::compile_with_instruction_limit_for_test(&mut native, ENTRY, true, 3)
            .expect("first integer block");
    assert_eq!(first_compilation.span.instructions, 3);
    assert!(
        !first_compilation.has_x87,
        "first block must stay pure integer for this to test the refused edge"
    );
    let first_id = native
        .jit_direct
        .install(&first_compilation)
        .expect("first integer block install");
    let first_block = native
        .jit_direct
        .block(first_id)
        .expect("first integer block remains live");

    let second_key = jit::direct::key_for(&native, SECOND, true).expect("second block key");
    assert!(matches!(
        native.jit_direct.probe(second_key),
        jit::direct::BlockProbe::Interpret
    ));
    let second_compilation =
        jit::direct::compile(&mut native, SECOND, true).expect("second float block");
    assert!(second_compilation.has_x87);
    let second_id = native
        .jit_direct
        .install(&second_compilation)
        .expect("second float block install");
    let second_block = native
        .jit_direct
        .block(second_id)
        .expect("second float block remains live");

    arm(&mut native, 0x0f7f);
    arm(&mut interpreter, 0x0f7f);
    native.registers.eip = ENTRY;
    interpreter.registers.eip = ENTRY;
    native_bus.memory.copy_from_slice(&memory);
    interpreter_bus.memory.copy_from_slice(&memory);
    native_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, first_block)
            .unwrap()
    );
    if native.registers.eip == SECOND {
        assert!(
            native
                .try_run_direct_block_for_test(&mut native_bus, second_block)
                .unwrap()
        );
    }
    for _ in 0..6 {
        interpreter.cycle(&mut interpreter_bus).unwrap();
    }

    assert_eq!(native.registers, interpreter.registers);
    assert_eq!(
        native.fpu, interpreter.fpu,
        "an integer source must never chain into a float target: skipping its prologue leaves \
         the physical x87 cache unloaded and its compile-time TOP unpinned"
    );
    assert_eq!(native.registers.eip, END);
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

/// `0xDC` mod=3: ST(1) op ST(0) with the result in ST(1) and NO pop.
///
/// The two trailing `fstp`s are load-bearing rather than plumbing. The first pops ST(0), which
/// is still 2.0 because this form does not pop, and the second stores the result. A `top_delta`
/// of 1 instead of 0 would make the emitter's running TOP advance after the DC slot, so BOTH
/// stores would address the wrong physical register and the two memory words would come out
/// swapped. Without an x87 slot after the DC instruction the field is uncatchable at runtime:
/// its only other effect is `x87_exit_top`, where a wrong value silently loses a link and fails
/// no test at all.
fn dc_sti_program(extension: u8) -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let code = [
        0xd9,
        0x05,
        0x00,
        0x02,
        0x00,
        0x00, // fld dword [0x200]   ST(0)=6.0
        0xd9,
        0x05,
        0x04,
        0x02,
        0x00,
        0x00, // fld dword [0x204]   ST(0)=2.0, ST(1)=6.0
        0xdc,
        0xc1 | (extension << 3), //        ST(1) op= ST(0), no pop
        0xd9,
        0x1d,
        0x08,
        0x02,
        0x00,
        0x00, // fstp dword [0x208]  stores ST(0), still 2.0
        0xd9,
        0x1d,
        0x0c,
        0x02,
        0x00,
        0x00, // fstp dword [0x20c]  stores the result
        0xf4,
    ];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 4].copy_from_slice(&6.0f32.to_bits().to_le_bytes());
    memory[DATA + 4..DATA + 8].copy_from_slice(&2.0f32.to_bits().to_le_bytes());
    memory
}

#[test]
fn every_dc_register_destination_binary_matches_the_interpreter() {
    // ABSOLUTE expected values, not merely JIT-equals-interpreter. `assert_program_matches`
    // proves the two agree; these numbers prove they agree with Intel. 6.0 and 2.0 are chosen
    // so every non-commutative op has a distinct result and no pair collides.
    let cases = [
        (0u8, 8.0f32),  // FADD  ST(1),ST(0)  6 + 2
        (1, 12.0),      // FMUL  ST(1),ST(0)  6 * 2
        (4, -4.0),      // FSUBR ST(1),ST(0)  2 - 6
        (5, 4.0),       // FSUB  ST(1),ST(0)  6 - 2
        (6, 1.0 / 3.0), // FDIVR ST(1),ST(0)  2 / 6
        (7, 3.0),       // FDIV  ST(1),ST(0)  6 / 2
    ];
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for (extension, expected) in cases {
            let (cpu, bus) = assert_program_matches(mode, dc_sti_program(extension), 0x0f7f);
            let popped = f32::from_bits(u32::from_le_bytes(
                bus.memory[DATA + 8..DATA + 12].try_into().unwrap(),
            ));
            let result = f32::from_bits(u32::from_le_bytes(
                bus.memory[DATA + 12..DATA + 16].try_into().unwrap(),
            ));
            // ST(0) is untouched by a non-popping form. If this reads as the result the stack
            // position advanced when it should not have.
            assert_eq!(popped, 2.0, "mode={mode:?} extension={extension} ST(0)");
            assert_eq!(result, expected, "mode={mode:?} extension={extension}");
            assert_eq!(cpu.fpu.top(), 0, "mode={mode:?} extension={extension} top");
            assert_eq!(
                cpu.perf_counters().jit_direct_side_exits,
                0,
                "mode={mode:?} extension={extension}"
            );
        }
    }
}

/// A divide by zero produces an infinity, which `emit_finite_guard` must catch BEFORE
/// `emit_store_physical` runs. The interpreter's `fpu_record_exceptions` sets ZE for this case,
/// and no native arm can write status bits 0 to 5, so the only correct native behaviour is to
/// leave. Asserting the exit is the point: values alone would match either way, because the
/// interpreter produces them on the fallback path.
#[test]
fn a_dc_divide_by_zero_exits_before_touching_x87_state() {
    let mut memory = dc_sti_program(7);
    memory[DATA + 4..DATA + 8].copy_from_slice(&0.0f32.to_bits().to_le_bytes());
    let (cpu, _) = assert_program_matches(GswMode::Gsw586, memory, 0x0f7f);
    assert!(
        cpu.perf_counters().jit_direct_exit_other > 0,
        "the infinite result must side-exit rather than be stored"
    );
    assert_ne!(cpu.fpu.status & 0x04, 0, "the interpreter recorded ZE");
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

/// Quake's float-to-integer bracket, the idiom the control-word pair exists for: save the control
/// word, set chop mode through the integer registers, convert, restore.
///
/// [0x202] is POISONED with 0xbeef. FNSTCW writes two bytes at [0x200]; a four-byte store would
/// clear it, and nothing else in the fixture would notice. `assert_program_matches` compares the
/// whole memory image.
fn control_word_bracket_program() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let code = [
        0xd9, 0x05, 0x08, 0x02, 0x00, 0x00, // fld dword [0x208]
        0xd9, 0x3d, 0x00, 0x02, 0x00, 0x00, // fnstcw word [0x200]
        0x66, 0x8b, 0x05, 0x00, 0x02, 0x00, 0x00, // mov ax,[0x200]
        0x80, 0xcc, 0x0c, // or ah,0x0c
        0x66, 0x89, 0x05, 0x04, 0x02, 0x00, 0x00, // mov [0x204],ax
        0xd9, 0x2d, 0x04, 0x02, 0x00, 0x00, // fldcw word [0x204]
        0xdb, 0x1d, 0x0c, 0x02, 0x00, 0x00, // fistp dword [0x20c]
        0xd9, 0x2d, 0x00, 0x02, 0x00, 0x00, // fldcw word [0x200]
        0xf4,
    ];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA + 2..DATA + 4].copy_from_slice(&0xbeefu16.to_le_bytes());
    memory[DATA + 8..DATA + 12].copy_from_slice(&(-3.7f32).to_bits().to_le_bytes());
    memory
}

#[test]
fn quake_control_word_bracket_matches_the_interpreter() {
    let (cpu, bus) =
        assert_program_matches(GswMode::Gsw586, control_word_bracket_program(), 0x027f);

    // Saved, modified and restored. 0x027f | 0x0c00 is RC = 11, truncate.
    assert_eq!(
        u16::from_le_bytes(bus.memory[DATA..DATA + 2].try_into().unwrap()),
        0x027f,
        "FNSTCW stored the live control word"
    );
    assert_eq!(
        u16::from_le_bytes(bus.memory[DATA + 2..DATA + 4].try_into().unwrap()),
        0xbeef,
        "FNSTCW wrote two bytes, not four"
    );
    assert_eq!(
        u16::from_le_bytes(bus.memory[DATA + 4..DATA + 6].try_into().unwrap()),
        0x0e7f,
        "chop mode was armed through the integer registers"
    );
    assert_eq!(
        cpu.fpu.control, 0x027f,
        "the bracket restored the entry value"
    );
    // -3.7 truncated toward zero, which is what chop mode means and what round-to-nearest would
    // have made -4.
    assert_eq!(
        i32::from_le_bytes(bus.memory[DATA + 12..DATA + 16].try_into().unwrap()),
        -3
    );
}

fn fldcw_then_fistp_program(control_at: u32) -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let mut code = vec![0xd9u8, 0x05, 0x08, 0x02, 0x00, 0x00]; // fld dword [0x208]
    code.extend_from_slice(&[0xd9, 0x2d]); // fldcw word [control_at]
    code.extend_from_slice(&control_at.to_le_bytes());
    code.extend_from_slice(&[0xdb, 0x1d, 0x0c, 0x02, 0x00, 0x00]); // fistp dword [0x20c]
    code.extend_from_slice(&[0x89, 0xc0, 0xf4]); // mov eax,eax ; hlt
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[control_at as usize..control_at as usize + 2].copy_from_slice(&0x0f7fu16.to_le_bytes());
    memory[DATA + 8..DATA + 12].copy_from_slice(&3.7f32.to_bits().to_le_bytes());
    memory
}

/// THE SECTION 4a TEST. `emit_fistp_chop_guard` reads the control word from `CpuGsw.fpu.control`
/// at RUNTIME, so a FISTP compiled after a lowered FLDCW in the SAME block sees the value that
/// FLDCW just wrote. The CPU enters with round-to-nearest, which the guard refuses, and the FLDCW
/// switches it to chop.
///
/// The assertion is `jit_direct_side_exits == 0`. A stale control word would make the guard exit,
/// and the truncated result would still be correct because the interpreter would produce it; only
/// the exit count distinguishes the two.
#[test]
fn a_lowered_fldcw_is_visible_to_a_later_fistp_in_the_same_block() {
    let (cpu, bus) =
        assert_program_matches(GswMode::Gsw586, fldcw_then_fistp_program(0x200), 0x037f);
    assert_eq!(cpu.fpu.control, 0x0f7f);
    assert_eq!(
        i32::from_le_bytes(bus.memory[DATA + 12..DATA + 16].try_into().unwrap()),
        3
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_side_exits,
        0,
        "the FISTP chop guard must see the control word the FLDCW wrote"
    );
}

/// THE ALIGNMENT-WIDTH TEST. `emit_wide_page_guard` refuses any access not aligned to
/// `width.bytes()`, so a control word pinned at `MemoryWidth::Dword` side-exits at every address
/// that is 2-aligned but not 4-aligned. Quake keeps the saved and the chop-mode word in adjacent
/// 2-byte slots, so one of each pair is always in that state.
///
/// 0x202 is deliberate. The other x87 fixtures in this file sit at 4-aligned addresses and could
/// not tell the two widths apart.
#[test]
fn a_two_aligned_control_word_runs_natively() {
    let (_, bus) = assert_program_matches(GswMode::Gsw586, fldcw_then_fistp_program(0x202), 0x037f);
    assert_eq!(
        i32::from_le_bytes(bus.memory[DATA + 12..DATA + 16].try_into().unwrap()),
        3
    );
}

#[test]
fn a_two_aligned_control_word_takes_no_side_exit() {
    let mut cpu = x87_cpu(GswMode::Gsw586);
    let memory = fldcw_then_fistp_program(0x202);
    let mut bus = direct_memory(memory.clone());
    arm(&mut cpu, 0x037f);
    run_to_halt(&mut cpu, &mut bus);
    cpu.set_jit_auto_admit(true);
    for _ in 0..2 {
        arm(&mut cpu, 0x037f);
        bus.memory.copy_from_slice(&memory);
        run_to_halt(&mut cpu, &mut bus);
    }
    let before = cpu.perf_counters().jit_direct_side_exits;
    let before_insns = cpu.perf_counters().jit_direct_insns;
    arm(&mut cpu, 0x037f);
    bus.memory.copy_from_slice(&memory);
    run_to_halt(&mut cpu, &mut bus);
    // Growth in the LAST run, not a cumulative total, so the side-exit assertion below cannot
    // pass by the block never having run.
    assert!(
        cpu.perf_counters().jit_direct_insns > before_insns,
        "the sequence did not run natively"
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_side_exits,
        before,
        "a 2-aligned control word must not trip the alignment guard"
    );
}

fn unmasking_fldcw_program() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let code = [
        0xd9, 0x2d, 0x00, 0x02, 0x00, 0x00, // fldcw word [0x200]
        0xd9, 0xe8, // fld1
        0x89, 0xc0, // mov eax,eax
        0x89, 0xdb, // mov ebx,ebx
        0xf4,
    ];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    // 0x037e clears IM, so it UNMASKS the invalid-operation exception that 0x037f masks.
    memory[DATA..DATA + 2].copy_from_slice(&0x037eu16.to_le_bytes());
    memory
}

/// THE GATE RE-ARM TEST, and the only test of the hazard the standing x87 plan did not name.
///
/// The native #MF gate is emitted ONCE per block, on the reasoning that no successful x87
/// instruction can make an exception pending for a later slot. FLDCW breaks that from the other
/// side: the gate condition is `status & 0x3f & !(control & 0x3f)`, and FLDCW changes the MASK, so
/// a status bit set earlier by an INTERPRETED instruction can be masked at block entry and
/// unmasked mid-block. The interpreter re-checks before every x87 instruction.
///
/// Entry: IE set in the status word, IM set in the control word so it is masked, CR0.NE on. The
/// FLDCW clears IM, and the FLD1 behind it must then trap exactly as it does on the interpreter.
/// Without the re-arm the native block runs the FLD1 and the two runs diverge completely.
#[test]
fn an_unmasking_fldcw_rearms_the_mf_gate_for_the_next_slot() {
    let memory = unmasking_fldcw_program();
    let (mut direct, mut direct_bus) =
        assert_program_matches(GswMode::Gsw586, memory.clone(), 0x037f);

    let mut interpreter = x87_cpu(GswMode::Gsw586);
    let mut interpreter_bus = direct_memory(memory.clone());
    arm(&mut interpreter, 0x037f);
    run_to_halt(&mut interpreter, &mut interpreter_bus);

    for cpu in [&mut direct, &mut interpreter] {
        arm(cpu, 0x037f);
        cpu.control.cr0 = CR0_NE;
        cpu.fpu.raise_exception(1);
    }
    direct_bus.memory.copy_from_slice(&memory);
    direct_bus.trace = BusTrace::default();
    interpreter_bus.memory.copy_from_slice(&memory);
    interpreter_bus.trace = BusTrace::default();

    let direct_result = direct.run_straight_line(&mut direct_bus, u64::MAX);
    let interpreter_result = interpreter.run_straight_line(&mut interpreter_bus, u64::MAX);
    assert_eq!(direct_result, interpreter_result, "outcome");
    assert_eq!(direct.registers, interpreter.registers, "registers");
    assert_eq!(direct.fpu, interpreter.fpu, "x87 state");
    assert_eq!(direct.elapsed_clocks, interpreter.elapsed_clocks);
    assert_eq!(direct.fp_rem, interpreter.fp_rem);
    // The FLDCW itself retired on both sides; only the FLD1 behind it must not.
    assert_eq!(direct.fpu.control, 0x037e, "the FLDCW retired");
    assert_eq!(direct.fpu.top(), 0, "the FLD1 did not push");
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
