// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const ENTRY: u32 = 0x100;
const DATA: usize = 0x200;

pub(super) fn x87_cpu(mode: GswMode) -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_fast_map_enabled_for_test(true);
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

pub(super) fn arm(cpu: &mut CpuGsw, control: u16) {
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

pub(super) fn run_to_halt(cpu: &mut CpuGsw, bus: &mut TestBus) -> Vec<(u32, u32, bool)> {
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

pub(super) fn direct_memory(mut memory: Vec<u8>) -> TestBus {
    if memory.len() < 0x1000 {
        memory.resize(0x1000, 0);
    }
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus
}

/// How strictly a differential fixture pins the number of instructions the native side actually
/// retired, on top of the state comparison every fixture already gets.
///
/// `AtLeastOne` is the historical behaviour: it only proves SOME prefix of the program ran
/// natively, which is enough when the state comparison itself is the point. `Exact` is the
/// stronger MID-BLOCK gate the 0xDA slice fixtures need: a block that silently stopped short of
/// the instruction under test (a classify regression, say) would still compare correctly against
/// the interpreter, because both sides would just be running the interpreter for that
/// instruction. Pinning the exact retirement count catches that the instruction under test
/// actually went through the native path rather than falling back invisibly.
enum InsnsExpectation {
    AtLeastOne,
    Exact(u64),
}

fn assert_program_matches_impl(
    mode: GswMode,
    memory: Vec<u8>,
    control: u16,
    expect_insns: InsnsExpectation,
) -> (CpuGsw, TestBus) {
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
    let retired = direct.perf_counters().jit_direct_insns - before;
    match expect_insns {
        InsnsExpectation::AtLeastOne => assert!(
            retired > 0,
            "the x87 sequence did not run natively: {:?}",
            direct.perf_counters()
        ),
        InsnsExpectation::Exact(expected) => assert_eq!(
            retired,
            expected,
            "native instructions retired differ from the expected count, meaning a slot lost \
             its native retirement (or gained an extra one): {:?}",
            direct.perf_counters()
        ),
    }
    (direct, direct_bus)
}

fn assert_program_matches(mode: GswMode, memory: Vec<u8>, control: u16) -> (CpuGsw, TestBus) {
    assert_program_matches_impl(mode, memory, control, InsnsExpectation::AtLeastOne)
}

/// Like `assert_program_matches`, but also pins the EXACT number of instructions the native side
/// retired rather than just proving it retired more than zero. See `InsnsExpectation` for why
/// that distinction matters for a mid-block fixture.
fn assert_program_matches_exact_insns(
    mode: GswMode,
    memory: Vec<u8>,
    control: u16,
    expected_insns: u64,
) -> (CpuGsw, TestBus) {
    assert_program_matches_impl(
        mode,
        memory,
        control,
        InsnsExpectation::Exact(expected_insns),
    )
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

/// The same float-to-integer crossing, but through the DYNAMIC path. `emit_completed_dynamic_path`
/// used to refuse this shape outright (`LinkRefusal::DynamicFloatToInteger`, 1,469,508 refusals on
/// the Quake fixture) because it never emitted the boundary spill; the static path's fixture above
/// cannot see that, because a static edge and a RET/JmpMem edge are two different emitters.
///
/// Structured like `a_jmp_through_memory_links_and_transfers_natively_on_the_second_entry`: the
/// first native call reports the miss and the bind happens afterwards in Rust, so the crossing is
/// only native on the second call. The x87 assertion is what makes it non-vacuous - the integer
/// target never runs `emit_x87_spill` itself, so if the source's jump does not flush the physical
/// cache and the packed status/tag word, `cpu.fpu` keeps whatever it held before the call.
#[test]
fn dynamic_float_to_integer_crossing_spills_the_boundary_and_matches_the_interpreter() {
    const SOURCE: u32 = ENTRY;
    const TARGET: u32 = 0x300;
    const MEM: u32 = 0x800;
    let mut memory = vec![0; 0x2000];
    memory[SOURCE as usize - 1] = 0x90;
    let source = [
        0x89, 0xc0, 0x89, 0xc0, 0x89, 0xc0, // three mov eax,eax
        0xd9, 0xe8, // fld1
        0xff, 0x25, 0x00, 0x08, 0x00, 0x00, // jmp dword [0x800]
    ];
    // Pure integer: no x87 opcode anywhere, so `has_x87` is false and the target neither enters
    // nor spills the register cache on its own account.
    let target = [0x89, 0xdb, 0x89, 0xdb, 0x89, 0xdb, 0xf4];
    memory[SOURCE as usize..SOURCE as usize + source.len()].copy_from_slice(&source);
    memory[TARGET as usize..TARGET as usize + target.len()].copy_from_slice(&target);
    memory[MEM as usize..MEM as usize + 4].copy_from_slice(&TARGET.to_le_bytes());

    let mut native = x87_cpu(GswMode::Gsw586);
    let mut interpreter = x87_cpu(GswMode::Gsw586);
    let mut native_bus = direct_memory(memory.clone());
    let mut interpreter_bus = direct_memory(memory.clone());
    arm(&mut native, 0x0f7f);
    run_to_halt(&mut native, &mut native_bus);
    arm(&mut interpreter, 0x0f7f);
    run_to_halt(&mut interpreter, &mut interpreter_bus);
    // `JmpMem`'s dword read needs the native map to exist before `compile` will emit it, and a
    // plain interpreted run never populates it.
    native
        .read_memory_u8(
            &mut native_bus,
            SegmentIndex::Ds,
            0,
            BusAccessKind::DataRead,
        )
        .expect("initialize direct map");

    arm(&mut native, 0x0f7f);
    native.registers.eip = TARGET;
    // The probe is what moves the key into `Seen`; `install` refuses any key that is not, so
    // skipping it turns the whole fixture into an `expect` failure rather than a test.
    let target_key = jit::direct::key_for(&native, TARGET, true).expect("target key");
    assert!(matches!(
        native.jit_direct.probe(target_key),
        jit::direct::BlockProbe::Interpret
    ));
    let target_compilation =
        jit::direct::compile(&mut native, TARGET, true).expect("integer target block");
    assert!(
        !target_compilation.has_x87,
        "the target must stay pure integer for this to test the crossing"
    );
    native
        .jit_direct
        .install(&target_compilation)
        .expect("target install");

    native.registers.eip = SOURCE;
    let source_key = jit::direct::key_for(&native, SOURCE, true).expect("source key");
    assert!(matches!(
        native.jit_direct.probe(source_key),
        jit::direct::BlockProbe::Interpret
    ));
    let source_compilation =
        jit::direct::compile(&mut native, SOURCE, true).expect("float source block");
    assert!(source_compilation.has_x87);
    assert!(source_compilation.dynamic_successor);
    let source_id = native
        .jit_direct
        .install(&source_compilation)
        .expect("source install");
    let source_block = native
        .jit_direct
        .block(source_id)
        .expect("source block remains live");

    // First pass: the cell is unbound when the native call is made, so this reports the miss and
    // binds afterwards in Rust. It also leaves the CPU mid-program, so both roles are re-armed
    // before the pass that is actually compared.
    arm(&mut native, 0x0f7f);
    native.registers.eip = SOURCE;
    native_bus.memory.copy_from_slice(&memory);
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, source_block)
            .unwrap()
    );

    arm(&mut native, 0x0f7f);
    arm(&mut interpreter, 0x0f7f);
    native.registers.eip = SOURCE;
    interpreter.registers.eip = SOURCE;
    native_bus.memory.copy_from_slice(&memory);
    interpreter_bus.memory.copy_from_slice(&memory);
    native_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();
    let transfers = native.perf_counters().jit_direct_linked_transfers;
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, source_block)
            .unwrap()
    );
    for _ in 0..8 {
        interpreter.cycle(&mut interpreter_bus).unwrap();
    }

    assert_eq!(
        native.perf_counters().jit_direct_linked_transfers - transfers,
        1,
        "the crossing must go native, not fall back through the dispatcher"
    );
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
    assert_eq!(native.registers.eip, TARGET + 6);
    let refusals = native.jit_direct.stall_snapshot().link_refusals;
    let refused = |name: &str| {
        refusals
            .iter()
            .find(|(label, _)| *label == name)
            .map(|(_, count)| *count)
            .expect("named refusal")
    };
    assert_eq!(
        refused("dynamic_float_to_integer"),
        0,
        "the refusal this slice retires must never fire again"
    );
}

/// The other dynamic direction: an INTEGER source into a FLOAT target, which
/// `emit_completed_dynamic_path` used to refuse (`LinkRefusal::DynamicIntegerToFloat`, 1,064,706
/// refusals on the Quake fixture) because it loaded `BlockPortal::body` unconditionally and would
/// have entered the target with an unloaded x87 register cache. It now loads `integer_entry`,
/// which for a float target is the shared re-entry pad.
///
/// The x87 comparison is the non-vacuity: the target's own prologue is SKIPPED on a chained entry,
/// so if the pad does not run in its place, the block's epilogue spills whatever XMM4-11 happened
/// to hold into `CpuGsw.fpu.st` and the comparison fails. `x87_pad_bails` is asserted at zero
/// separately, because a pad that bailed on the TOP guard would also leave `fpu` correct - by
/// never crossing at all, which is the vacuous pass this fixture has to exclude.
#[test]
fn dynamic_integer_to_float_crossing_enters_through_the_pad_and_matches_the_interpreter() {
    const SOURCE: u32 = ENTRY;
    const TARGET: u32 = 0x300;
    const MEM: u32 = 0x800;
    let mut memory = vec![0; 0x2000];
    memory[SOURCE as usize - 1] = 0x90;
    // Pure integer, no x87 opcode: `has_x87` is false, so this block loads `integer_entry`.
    let source = [
        0x89, 0xc0, 0x89, 0xc0, 0x89, 0xc0, // three mov eax,eax
        0xff, 0x25, 0x00, 0x08, 0x00, 0x00, // jmp dword [0x800]
    ];
    let target = [
        0xd9, 0xe8, // fld1
        0x89, 0xdb, 0x89, 0xdb, // mov ebx,ebx x2
        0xf4, // hlt
    ];
    memory[SOURCE as usize..SOURCE as usize + source.len()].copy_from_slice(&source);
    memory[TARGET as usize..TARGET as usize + target.len()].copy_from_slice(&target);
    memory[MEM as usize..MEM as usize + 4].copy_from_slice(&TARGET.to_le_bytes());

    let mut native = x87_cpu(GswMode::Gsw586);
    let mut interpreter = x87_cpu(GswMode::Gsw586);
    let mut native_bus = direct_memory(memory.clone());
    let mut interpreter_bus = direct_memory(memory.clone());
    arm(&mut native, 0x0f7f);
    run_to_halt(&mut native, &mut native_bus);
    arm(&mut interpreter, 0x0f7f);
    run_to_halt(&mut interpreter, &mut interpreter_bus);
    native
        .read_memory_u8(
            &mut native_bus,
            SegmentIndex::Ds,
            0,
            BusAccessKind::DataRead,
        )
        .expect("initialize direct map");

    arm(&mut native, 0x0f7f);
    native.registers.eip = TARGET;
    let target_key = jit::direct::key_for(&native, TARGET, true).expect("target key");
    assert!(matches!(
        native.jit_direct.probe(target_key),
        jit::direct::BlockProbe::Interpret
    ));
    let target_compilation =
        jit::direct::compile(&mut native, TARGET, true).expect("float target block");
    assert!(
        target_compilation.has_x87,
        "the target must carry x87 for this to test the pad"
    );
    // The pad guards the target's baked entry TOP against the CPU's live TOP, so a fixture whose
    // two disagree would bail instead of crossing and prove nothing.
    assert_eq!(target_compilation.x87_entry_top, 0);
    native
        .jit_direct
        .install(&target_compilation)
        .expect("target install");

    native.registers.eip = SOURCE;
    let source_key = jit::direct::key_for(&native, SOURCE, true).expect("source key");
    assert!(matches!(
        native.jit_direct.probe(source_key),
        jit::direct::BlockProbe::Interpret
    ));
    let source_compilation =
        jit::direct::compile(&mut native, SOURCE, true).expect("integer source block");
    assert!(!source_compilation.has_x87);
    assert!(source_compilation.dynamic_successor);
    let source_id = native
        .jit_direct
        .install(&source_compilation)
        .expect("source install");
    let source_block = native
        .jit_direct
        .block(source_id)
        .expect("source block remains live");

    // First pass reports the miss; the bind happens afterwards, in Rust.
    arm(&mut native, 0x0f7f);
    native.registers.eip = SOURCE;
    native_bus.memory.copy_from_slice(&memory);
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, source_block)
            .unwrap()
    );

    arm(&mut native, 0x0f7f);
    arm(&mut interpreter, 0x0f7f);
    native.registers.eip = SOURCE;
    interpreter.registers.eip = SOURCE;
    native_bus.memory.copy_from_slice(&memory);
    interpreter_bus.memory.copy_from_slice(&memory);
    native_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();
    let transfers = native.perf_counters().jit_direct_linked_transfers;
    let bails = native.perf_counters().jit_direct_x87_pad_bails;
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, source_block)
            .unwrap()
    );
    for _ in 0..7 {
        interpreter.cycle(&mut interpreter_bus).unwrap();
    }

    assert_eq!(
        native.perf_counters().jit_direct_linked_transfers - transfers,
        1,
        "the crossing must go native through the pad, not fall back through the dispatcher"
    );
    assert_eq!(
        native.perf_counters().jit_direct_x87_pad_bails - bails,
        0,
        "a bailing pad would leave fpu correct by never crossing, which is a vacuous pass"
    );
    assert_eq!(native.registers, interpreter.registers);
    assert_eq!(
        native.fpu, interpreter.fpu,
        "the pad must load the register cache the target's skipped prologue would have loaded"
    );
    assert_eq!(native.pending_flags, interpreter.pending_flags);
    assert_eq!(native.elapsed_clocks, interpreter.elapsed_clocks);
    assert_eq!(native.fp_rem, interpreter.fp_rem);
    assert_eq!(native_bus.memory, interpreter_bus.memory);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interpreter_bus.trace.elapsed_clocks()
    );
    let refusals = native.jit_direct.stall_snapshot().link_refusals;
    let refused = |name: &str| {
        refusals
            .iter()
            .find(|(label, _)| *label == name)
            .map(|(_, count)| *count)
            .expect("named refusal")
    };
    assert_eq!(refused("dynamic_integer_to_float"), 0);
    assert_eq!(refused("dynamic_float_to_integer"), 0);
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
        cpu.direct_stall_snapshot().side_exit_x87_eligibility > 0,
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

/// FUCOM/FUCOMP ST(1) with an x87 slot BEHIND the compare, so the pop is observable.
///
/// `a` lands in ST(1) and `b` in ST(0), so the compare is `b` against `a`. The trailing FSTP
/// stores whatever the compare left as ST(0): `b` for the non-popping form, `a` for the popping
/// one. That difference is the `top_delta` pin -- a shape that claimed the wrong stack effect
/// would store the other value or trip the empty-tag guard, and neither survives the state
/// comparison or the exact-retirement gate.
fn fucom_program(pop: bool, a: f32, b: f32) -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    // 0xDD mod=3 with rm=1: /4 is FUCOM ST(1) (0xE1), /5 is FUCOMP ST(1) (0xE9).
    let modrm = if pop { 0xe9 } else { 0xe1 };
    let code = [
        0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax,1                integer, before
        0xd9, 0x05, 0x00, 0x02, 0x00, 0x00, // fld dword [0x200]  ST(0)=a
        0xd9, 0x05, 0x04, 0x02, 0x00, 0x00, // fld dword [0x204]  ST(0)=b, ST(1)=a
        0xdd, modrm, // fucom/fucomp st(1)
        0xdf, 0xe0, // fnstsw ax
        0xd9, 0x1d, 0x08, 0x02, 0x00, 0x00, // fstp dword [0x208] the top_delta trap
        0x89, 0xc2, // mov edx,eax             integer, after
        0xf4,
    ];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 4].copy_from_slice(&a.to_bits().to_le_bytes());
    memory[DATA + 4..DATA + 8].copy_from_slice(&b.to_bits().to_le_bytes());
    memory
}

/// FUCOM/FUCOMP's three-way condition bits and stack effect. The interpreter serves these with
/// the same `fpu_compare` the ordered forms use, so the values are not the interesting part; the
/// interesting parts are that the pair is admitted at all, that only `/5` pops, and that the
/// shape is charged `clocks(4)` rather than the ordered register compare's 20. The clock
/// difference rides `run timing differs` inside the shared comparison.
#[test]
fn fucom_and_fucomp_condition_bits_and_pop_match_the_interpreter() {
    // (a in ST(1), b in ST(0), expected C3|C2|C0 for b against a).
    let cases = [
        (3.0f32, 5.0f32, 0u16), // b > a: above
        (5.0, 5.0, 1 << 14),    // b == a: equal
        (7.0, 5.0, 1 << 8),     // b < a: below
    ];
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for pop in [false, true] {
            for (a, b, expected_bits) in cases {
                let (cpu, bus) = assert_program_matches_exact_insns(
                    mode,
                    fucom_program(pop, a, b),
                    0x0f7f,
                    7, // mov, fld, fld, fucom, fnstsw, fstp, mov -- hlt never retires natively
                );
                assert_eq!(
                    cpu.fpu.status & 0x4500,
                    expected_bits,
                    "mode={mode:?} pop={pop} a={a} b={b}"
                );
                assert_eq!(
                    f32::from_bits(u32::from_le_bytes(
                        bus.memory[DATA + 8..DATA + 12].try_into().unwrap()
                    )),
                    if pop { a } else { b },
                    "mode={mode:?} pop={pop}: FSTP stored the wrong stack slot"
                );
                assert_eq!(
                    cpu.perf_counters().jit_direct_side_exits,
                    0,
                    "mode={mode:?} pop={pop}"
                );
            }
        }
    }
}

/// FIST m32 (0xDB /2) followed by FISTP m32 (0xDB /3) against the SAME source value.
///
/// The second store is the pin on the first one's stack effect: FIST must leave ST(0) standing,
/// so the FISTP behind it converts the same value and both slots land identical bytes. Had /2
/// been given FISTP's `pop`, the FISTP would address a register the pop just tagged Empty,
/// `emit_load_physical` would side exit, and both the exact-retirement gate and the
/// side-exit assertion below would fail.
fn fist_then_fistp_program(value: f32) -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let code = [
        0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax,1                integer, before
        0xd9, 0x05, 0x00, 0x02, 0x00, 0x00, // fld dword [0x200]  ST(0)=value
        0xdb, 0x15, 0x04, 0x02, 0x00, 0x00, // fist dword [0x204]  /2, no pop
        0xdb, 0x1d, 0x08, 0x02, 0x00, 0x00, // fistp dword [0x208] /3, pops
        0xba, 0x02, 0x00, 0x00, 0x00, // mov edx,2                integer, after
        0xf4,
    ];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 4].copy_from_slice(&value.to_bits().to_le_bytes());
    memory
}

/// The control word is 0x0F7F, whose RC field is truncate, which is the only mode
/// `emit_fistp_chop_guard` admits. Negative and positive sources both carry a fraction so a
/// rounding mode that was not truncate would show in the stored integer.
#[test]
fn fist_m32_stores_without_popping_and_matches_the_interpreter() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for (value, expected) in [(-2.75f32, -2i32), (2.75, 2), (7.0, 7)] {
            let (cpu, bus) = assert_program_matches_exact_insns(
                mode,
                fist_then_fistp_program(value),
                0x0f7f,
                5, // mov, fld, fist, fistp, mov -- hlt never retires natively
            );
            for slot in [DATA + 4, DATA + 8] {
                assert_eq!(
                    i32::from_le_bytes(bus.memory[slot..slot + 4].try_into().unwrap()),
                    expected,
                    "mode={mode:?} value={value} slot={slot:#x}"
                );
            }
            assert_eq!(cpu.fpu.top(), 0, "mode={mode:?} value={value}");
            assert_eq!(cpu.fpu.tag, 0xffff, "mode={mode:?} value={value}");
            assert_eq!(
                cpu.perf_counters().jit_direct_side_exits,
                0,
                "mode={mode:?} value={value}"
            );
        }
    }
}

/// One 0xD9 /4 register form, with an FNSTSW to capture the condition bits it may write and an
/// FSTP to capture the value it may rewrite. Both halves matter: FCHS and FABS move the value and
/// must leave the condition bits alone, FTST and FXAM do the exact opposite.
fn d9_sign_and_classify_program(op: u8, value: f32) -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let code = [
        0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax,1                integer, before
        0xd9, 0x05, 0x00, 0x02, 0x00, 0x00, // fld dword [0x200]  ST(0)=value
        0xd9, op, // the tested 0xD9 /4 form
        0xdf, 0xe0, // fnstsw ax
        0xd9, 0x1d, 0x04, 0x02, 0x00, 0x00, // fstp dword [0x204]
        0xba, 0x02, 0x00, 0x00, 0x00, // mov edx,2                integer, after
        0xf4,
    ];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 4].copy_from_slice(&value.to_bits().to_le_bytes());
    memory
}

/// FCHS, FABS, FTST and FXAM against the interpreter.
///
/// -0.0 is the load-bearing input. It is the one value where the zero test and the sign test
/// disagree, so it separates FXAM's C3 (set: the value IS zero) from its C1 (set: the sign bit is
/// on) -- a lowering that derived C1 from the comparison rather than from bit 63 passes every
/// other row here and fails this one. It also catches an FCHS written as a subtraction from zero,
/// which would turn -0.0 into +0.0 and land different bytes in the stored dword.
#[test]
fn fchs_fabs_ftst_and_fxam_match_the_interpreter() {
    const C0: u16 = 1 << 8;
    const C1: u16 = 1 << 9;
    const C2: u16 = 1 << 10;
    const C3: u16 = 1 << 14;
    // (op, value, expected stored f32, expected C3|C2|C1|C0).
    let cases = [
        (0xe0u8, -2.5f32, 2.5f32, 0u16), // FCHS: value moves, condition bits do not
        (0xe0, 3.5, -3.5, 0),
        (0xe0, -0.0, 0.0, 0),
        (0xe1, -2.5, 2.5, 0), // FABS
        (0xe1, 3.5, 3.5, 0),
        (0xe1, -0.0, 0.0, 0),
        (0xe4, 3.5, 3.5, 0),         // FTST: ST(0) above zero
        (0xe4, -2.5, -2.5, C0),      // below
        (0xe4, 0.0, 0.0, C3),        // equal
        (0xe4, -0.0, -0.0, C3),      // -0.0 compares EQUAL to zero, so C3 and not C0
        (0xe5, 3.5, 3.5, C2),        // FXAM: finite non-zero, positive
        (0xe5, -2.5, -2.5, C2 | C1), // finite non-zero, negative
        (0xe5, 0.0, 0.0, C3),        // zero, positive
        (0xe5, -0.0, -0.0, C3 | C1), // zero, NEGATIVE: C3 from the value, C1 from the sign bit
    ];
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for (op, value, expected_value, expected_bits) in cases {
            let (cpu, bus) = assert_program_matches_exact_insns(
                mode,
                d9_sign_and_classify_program(op, value),
                0x0f7f,
                6, // mov, fld, op, fnstsw, fstp, mov -- hlt never retires natively
            );
            assert_eq!(
                u32::from_le_bytes(bus.memory[DATA + 4..DATA + 8].try_into().unwrap()),
                expected_value.to_bits(),
                "mode={mode:?} op={op:#x} value={value}: stored bits (sign included)"
            );
            assert_eq!(
                cpu.fpu.status & (C0 | C1 | C2 | C3),
                expected_bits,
                "mode={mode:?} op={op:#x} value={value}: condition bits"
            );
            assert_eq!(
                cpu.perf_counters().jit_direct_side_exits,
                0,
                "mode={mode:?} op={op:#x} value={value}"
            );
        }
    }
}

fn fsqrt_program(value: f32) -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let code = [
        0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax,1                integer, before
        0xd9, 0x05, 0x00, 0x02, 0x00, 0x00, // fld dword [0x200]  ST(0)=value
        0xd9, 0xfa, // fsqrt
        0xd9, 0x1d, 0x04, 0x02, 0x00, 0x00, // fstp dword [0x204]
        0xba, 0x02, 0x00, 0x00, 0x00, // mov edx,2                integer, after
        0xf4,
    ];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 4].copy_from_slice(&value.to_bits().to_le_bytes());
    memory
}

#[test]
fn fsqrt_matches_the_interpreter_for_non_negative_operands() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for (value, expected) in [
            (4.0f32, 2.0f32),
            (2.0, std::f32::consts::SQRT_2),
            (0.0, 0.0),
        ] {
            let (cpu, bus) = assert_program_matches_exact_insns(
                mode,
                fsqrt_program(value),
                0x0f7f,
                5, // mov, fld, fsqrt, fstp, mov -- hlt never retires natively
            );
            assert_eq!(
                f32::from_bits(u32::from_le_bytes(
                    bus.memory[DATA + 4..DATA + 8].try_into().unwrap()
                )),
                expected,
                "mode={mode:?} value={value}"
            );
            assert_eq!(
                cpu.perf_counters().jit_direct_side_exits,
                0,
                "mode={mode:?} value={value}"
            );
        }
    }
}

/// FSQRT of a negative operand. The interpreter STORES the NaN and raises IE; the resident cache
/// cannot hold a NaN, so the native form must side exit at the result guard with the x87 stack
/// untouched and let the interpreter do both. Only the two instructions ahead of the FSQRT retire
/// natively, and they do so on every pass because the exit is taken every time.
#[test]
fn fsqrt_of_a_negative_operand_exits_before_touching_x87_state() {
    let (cpu, bus) =
        assert_program_matches_exact_insns(GswMode::Gsw586, fsqrt_program(-4.0), 0x0f7f, 2);
    // The FSTP behind the FSQRT carried the NaN out to memory, so that is where it is visible.
    assert!(
        f32::from_bits(u32::from_le_bytes(
            bus.memory[DATA + 4..DATA + 8].try_into().unwrap()
        ))
        .is_nan(),
        "the interpreter computed and stored the NaN"
    );
    assert_ne!(cpu.fpu.status & 0x01, 0, "IE must be recorded");
    assert!(cpu.perf_counters().jit_direct_side_exits > 0);
}

fn fistp_m64_program(value: f64) -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let code = [
        0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax,1                 integer, before
        0xdd, 0x05, 0x00, 0x02, 0x00, 0x00, // fld qword [0x200]   ST(0)=value
        0xdf, 0x3d, 0x08, 0x02, 0x00, 0x00, // fistp qword [0x208] 0xDF /7
        0xba, 0x02, 0x00, 0x00, 0x00, // mov edx,2                 integer, after
        0xf4,
    ];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 8].copy_from_slice(&value.to_bits().to_le_bytes());
    memory
}

/// FISTP m64 across the range the chop guard admits.
///
/// Both signs carry a fraction, so a rounding mode that was not truncate shows in the stored
/// integer, and the magnitudes are past 2^32 so a conversion that had stayed 32-bit wide would
/// wrap rather than merely lose precision.
#[test]
fn fistp_m64_matches_the_interpreter_inside_the_admitted_range() {
    let cases = [
        (3.5f64, 3i64),
        (-3.5, -3),
        (1_234_567_890_123.5, 1_234_567_890_123),
        (-1_234_567_890_123.5, -1_234_567_890_123),
        // Exactly -2^63. In range, and the value the low bound must NOT refuse: the m32 guard's
        // JBE shape would have rejected it, which is why this width uses a strict JB.
        (-9_223_372_036_854_775_808.0, i64::MIN),
    ];
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for (value, expected) in cases {
            let (cpu, bus) = assert_program_matches_exact_insns(
                mode,
                fistp_m64_program(value),
                0x0f7f,
                4, // mov, fld, fistp, mov -- hlt never retires natively
            );
            assert_eq!(
                i64::from_le_bytes(bus.memory[DATA + 8..DATA + 16].try_into().unwrap()),
                expected,
                "mode={mode:?} value={value}"
            );
            assert_eq!(cpu.fpu.top(), 0, "mode={mode:?} value={value}");
            assert_eq!(
                cpu.perf_counters().jit_direct_side_exits,
                0,
                "mode={mode:?} value={value}"
            );
        }
    }
}

/// Out of range on both sides. The interpreter stores the integer indefinite and raises IE; the
/// native form has to leave before the conversion so it can. 2^63 itself is the upper case, one
/// past the largest storable integer, and it is the bound the guard refuses with JAE.
#[test]
fn fistp_m64_out_of_range_exits_before_touching_x87_state() {
    for value in [9_223_372_036_854_775_808.0f64, -1.0e19] {
        let (cpu, bus) = assert_program_matches_exact_insns(
            GswMode::Gsw586,
            fistp_m64_program(value),
            0x0f7f,
            2,
        );
        assert_eq!(
            u64::from_le_bytes(bus.memory[DATA + 8..DATA + 16].try_into().unwrap()),
            0x8000_0000_0000_0000,
            "value={value}: the interpreter stored the integer indefinite"
        );
        assert_ne!(
            cpu.fpu.status & 0x01,
            0,
            "value={value}: IE must be recorded"
        );
        assert!(
            cpu.perf_counters().jit_direct_side_exits > 0,
            "value={value}"
        );
    }
}

/// FLD m64 then FSTP m80 into `at`. The destination displacement is a parameter so the same
/// program can be pointed at an admitted alignment and at a refused one: 0x208 and 0x20c
/// exercise the two admitted cases, and 2-aligned is REFUSED -- see
/// `fstp_m80_at_a_two_aligned_address_exits` for why that cut exists.
fn fstp_m80_program(value: f64, at: u32) -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let mut code = vec![
        0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax,1                integer, before
        0xdd, 0x05, 0x00, 0x02, 0x00, 0x00, // fld qword [0x200]  ST(0)=value
        0xdb, 0x3d, // fstp tbyte [at]                            0xDB /7
    ];
    code.extend_from_slice(&at.to_le_bytes());
    code.extend_from_slice(&[
        0xba, 0x02, 0x00, 0x00, 0x00, // mov edx,2                integer, after
        0xf4,
    ]);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 8].copy_from_slice(&value.to_bits().to_le_bytes());
    memory
}

/// FSTP m80 against the interpreter, plus an INDEPENDENT check of the 80-bit encoding.
///
/// The differential half cannot catch a conversion that is wrong in the same way on both sides,
/// because both sides are this crate. So each case also pins the architectural mantissa and
/// sign-exponent word by hand: 1.0 is 0x8000000000000000 at exponent 0x3FFF, the integer bit is
/// explicit in the 80-bit format where it is implicit in f64, and a zero is all-zero mantissa
/// with an all-zero exponent carrying only the sign.
#[test]
fn fstp_m80_matches_the_interpreter_and_the_extended_encoding() {
    // (value, mantissa, sign-exponent word).
    let cases = [
        (1.0f64, 0x8000_0000_0000_0000u64, 0x3fffu16),
        (-1.0, 0x8000_0000_0000_0000, 0xbfff),
        (2.0, 0x8000_0000_0000_0000, 0x4000),
        (0.5, 0x8000_0000_0000_0000, 0x3ffe),
        (3.0, 0xc000_0000_0000_0000, 0x4000),
        // The zero branch, both signs. -0.0 is the one that separates a sign taken from bit 63
        // from a sign inferred from a comparison.
        (0.0, 0, 0),
        (-0.0, 0, 0x8000),
        // A full 52-bit fraction, so every mantissa bit has to land in the right place.
        (
            f64::from_bits(0x3ff9_2492_4924_9249),
            0xc924_9249_2492_4800,
            0x3fff,
        ),
        // The smallest NORMAL f64: biased == 1, the low edge of the branch that is lowered, one
        // step above the subnormals that side exit.
        // 2^-1022: biased 1, so the extended exponent is 1 - 1023 + 16383 = 15361, NOT 1. The
        // rebias is the whole content of the exponent path and this is where it is visible.
        (f64::MIN_POSITIVE, 0x8000_0000_0000_0000, 0x3c01),
        (f64::MAX, 0xffff_ffff_ffff_f800, 0x43fe),
    ];
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        // 0x208 is 8-aligned, 0x20c is 4-aligned and nothing more. Both are admitted; a merely
        // 2-aligned destination is NOT, and `fstp_m80_at_a_two_aligned_address_exits` below is
        // the positive assertion of that cut.
        for at in [0x208u32, 0x20c] {
            for (value, mantissa, sign_exponent) in cases {
                let (cpu, bus) = assert_program_matches_exact_insns(
                    mode,
                    fstp_m80_program(value, at),
                    0x0f7f,
                    4, // mov, fld, fstp, mov -- hlt never retires natively
                );
                let base = at as usize;
                assert_eq!(
                    u64::from_le_bytes(bus.memory[base..base + 8].try_into().unwrap()),
                    mantissa,
                    "mode={mode:?} at={at:#x} value={value}: mantissa"
                );
                assert_eq!(
                    u16::from_le_bytes(bus.memory[base + 8..base + 10].try_into().unwrap()),
                    sign_exponent,
                    "mode={mode:?} at={at:#x} value={value}: sign and exponent"
                );
                assert_eq!(cpu.fpu.top(), 0, "mode={mode:?} at={at:#x} value={value}");
                assert_eq!(
                    cpu.perf_counters().jit_direct_side_exits,
                    0,
                    "mode={mode:?} at={at:#x} value={value}"
                );
            }
        }
    }
}

/// A ten-byte store that is only 2-aligned is refused at the width's alignment guard.
///
/// Not a byte-correctness cut: the stored bytes would be right. It is a BUS-TIMING cut. The
/// interpreter writes the first eight bytes as two dword transactions, and a dword write only
/// takes the direct-page path when it is 4-aligned; at 2-aligned it falls onto the slow path and
/// is charged clocks the native store never pays. This fixture is the positive assertion of that
/// population cut, and the shared comparison behind it is what proves the exit keeps the two
/// roles in step.
#[test]
fn fstp_m80_at_a_two_aligned_address_exits() {
    let (cpu, _) = assert_program_matches_exact_insns(
        GswMode::Gsw586,
        fstp_m80_program(1.0, 0x20a),
        0x0f7f,
        2, // mov, fld -- the store exits at the alignment guard on every pass
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_exit_cross_page_or_alignment,
        cpu.perf_counters().jit_direct_side_exits,
        "every side exit here is the alignment guard, not some other refusal"
    );
    assert!(cpu.perf_counters().jit_direct_side_exits > 0);
}

/// A subnormal f64 is the one finite input FSTP m80 refuses. The interpreter normalizes it with
/// `log2().floor()` and says in its own comment that the result is scaled rather than exact;
/// reproducing an inexact path exactly is not worth an emitted loop, so the native form leaves
/// and the interpreter does it. Only the two instructions ahead of the store retire natively.
#[test]
fn fstp_m80_of_a_subnormal_exits_to_the_interpreter() {
    let (cpu, bus) = assert_program_matches_exact_insns(
        GswMode::Gsw586,
        fstp_m80_program(f64::from_bits(1), 0x208),
        0x0f7f,
        2,
    );
    assert_ne!(
        u64::from_le_bytes(bus.memory[0x208..0x210].try_into().unwrap()),
        0,
        "the interpreter still stored the subnormal"
    );
    assert!(cpu.perf_counters().jit_direct_side_exits > 0);
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
    assert!(divide_cpu.direct_stall_snapshot().side_exit_x87_eligibility > 0);
    assert!(divide_cpu.fpu.get(0).is_infinite());
    assert_ne!(divide_cpu.fpu.status & 0x04, 0);

    let (fist_cpu, fist_bus) =
        assert_program_matches(GswMode::Gsw586, nearest_fistp_program(), 0x037f);
    assert!(fist_cpu.direct_stall_snapshot().side_exit_x87_eligibility > 0);
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

/// THE ALIGNMENT-WIDTH TEST. The x87 memory site refuses any access not aligned to its
/// `alignment_bytes()`, so a control word pinned at `MemoryWidth::Dword` side-exits at every
/// address that is 2-aligned but not 4-aligned. Quake keeps the saved and the chop-mode word in
/// adjacent 2-byte slots, so one of each pair is always in that state.
///
/// Still true after guard 3, and worth saying why rather than leaving it to be re-derived: that
/// slice relaxed the alignment half of the guard at the two LEAN one-lookup sites only. The x87
/// site is not one of them and refuses exactly as before.
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
    assert_eq!(&direct_bus.memory[..], &memory[..]);

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

// Slice 38: 0xDA m32int arithmetic (`NativeX87Insn::IntBinaryMemory`). Every fixture below places
// the 0xDA form MID-BLOCK, between plain integer instructions, and pins the EXACT number of
// native instructions retired rather than merely `> 0`. A classify or emit regression that made
// the 0xDA slot fall back to the interpreter would otherwise still pass a plain state comparison:
// both sides would just be running the interpreter for that one instruction, and the fixture
// would end up certifying the interpreter against itself.

/// Two FLDs and a register FADD warm the block up, deliberately leaving VALUE0's residue (A+B)
/// different from the true ST(0) (B) the 0xDA op must read. A dropped `emit_load_physical` on the
/// operand (mutation 5 in the design battery) would then compute against the stale residue
/// instead of accidentally landing on the right value, which is what makes the divergence
/// detectable. The 0xDA op itself sits directly between two plain integer MOVs.
fn fi_arith_program(extension: u8, operand: i32) -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let mut code = vec![
        0xd9,
        0x05,
        0x00,
        0x02,
        0x00,
        0x00, // fld dword [0x200]        ST(0)=A
        0xd9,
        0x05,
        0x04,
        0x02,
        0x00,
        0x00, // fld dword [0x204]        ST(0)=B, ST(1)=A
        0xdc,
        0xc1, // fadd st(1),st(0)                                  ST(1)=A+B, ST(0)=B
        0xb8,
        0x01,
        0x00,
        0x00,
        0x00, // mov eax,1
        0xda,
        (extension << 3) | 5,
    ];
    code.extend_from_slice(&(DATA as u32 + 8).to_le_bytes()); // fi<op> dword [0x208], mid-block
    code.extend_from_slice(&[
        0xbb, 0x02, 0x00, 0x00, 0x00, // mov ebx,2
        0xd9, 0x1d, 0x0c, 0x02, 0x00, 0x00, // fstp dword [0x20c]
        0xf4,
    ]);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 4].copy_from_slice(&3.0f32.to_bits().to_le_bytes()); // A
    memory[DATA + 4..DATA + 8].copy_from_slice(&5.0f32.to_bits().to_le_bytes()); // B
    memory[DATA + 8..DATA + 12].copy_from_slice(&operand.to_le_bytes());
    memory
}

/// FIADD, FIMUL and FIDIV against a non-zero m32int operand. Zero is deliberately excluded here:
/// it converts identically whether the operand is read as an i32 or misread as an f32 bit
/// pattern reinterpreted some other way, so a zero operand would let a wrong-convert mutation
/// (swapping `vcvtsi2sd` for `vcvtss2sd`) survive undetected.
#[test]
fn fiadd_fimul_and_fidiv_with_a_nonzero_operand_match_the_interpreter() {
    // (extension, operand, expected ST(0) result). ST(0) entering the 0xDA slot is B = 5.0.
    let cases = [(0u8, 4i32, 9.0f32), (1, 4, 20.0), (6, 4, 1.25)];
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for (extension, operand, expected) in cases {
            let (cpu, bus) = assert_program_matches_exact_insns(
                mode,
                fi_arith_program(extension, operand),
                0x0f7f,
                7, // fld, fld, fadd, mov, fi<op>, mov, fstp -- hlt never retires natively
            );
            assert_eq!(
                f32::from_bits(u32::from_le_bytes(
                    bus.memory[DATA + 12..DATA + 16].try_into().unwrap()
                )),
                expected,
                "mode={mode:?} extension={extension}"
            );
            assert_eq!(
                cpu.perf_counters().jit_direct_side_exits,
                0,
                "mode={mode:?} extension={extension}"
            );
        }
    }
}

/// FICOM's three-way condition bits: ST(0) above, equal to and below the m32int operand. FICOM
/// does not pop, so `top_delta` for this shape stays provably separate from FICOMP's.
#[test]
fn ficom_condition_bits_match_the_interpreter_above_equal_and_below() {
    // ST(0) entering the 0xDA slot is B = 5.0. (operand, expected C3|C2|C0 bits).
    let cases = [
        (3i32, 0u16), // ST(0) > operand: above
        (5, 1 << 14), // ST(0) == operand: equal
        (7, 1 << 8),  // ST(0) < operand: below
    ];
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for (operand, expected_bits) in cases {
            let (cpu, _) = assert_program_matches_exact_insns(
                mode,
                fi_arith_program(2, operand), // /2 FICOM
                0x0f7f,
                7,
            );
            assert_eq!(
                cpu.fpu.status & 0x4500,
                expected_bits,
                "mode={mode:?} operand={operand}"
            );
            assert_eq!(cpu.perf_counters().jit_direct_side_exits, 0);
        }
    }
}

/// The `top_delta` trap: FICOMP pops, so the x87 slot immediately behind it must address the
/// PHYSICAL register the pop left as the new ST(0), not the one that was ST(0) before the pop.
/// If `top_delta` stayed in the `=> 0` group (the mutation this fixture exists to catch), the
/// compile-time TOP tracking used for this follow-on FSTP would stay stale, and it would either
/// address the wrong physical register (corrupting the stored value) or trip the empty-tag guard
/// on the register the real runtime pop just vacated (an unexpected side exit). Either way the
/// exact-retirement gate and the state comparison below catch it; a correct compile does neither.
fn ficomp_followed_by_fstp_program() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let code = [
        0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax,1                    integer, before
        0xd9, 0x05, 0x00, 0x02, 0x00, 0x00, // fld dword [0x200]      ST(0)=A
        0xd9, 0x05, 0x04, 0x02, 0x00, 0x00, // fld dword [0x204]      ST(0)=B, ST(1)=A
        0xda, 0x1d, 0x08, 0x02, 0x00, 0x00, // ficomp dword [0x208]   pops: ST(0)=A now
        0xd9, 0x1d, 0x0c, 0x02, 0x00, 0x00, // fstp dword [0x20c]     the top_delta trap
        0xbb, 0x02, 0x00, 0x00, 0x00, // mov ebx,2                    integer, after
        0xf4,
    ];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 4].copy_from_slice(&3.0f32.to_bits().to_le_bytes()); // A
    memory[DATA + 4..DATA + 8].copy_from_slice(&5.0f32.to_bits().to_le_bytes()); // B
    memory[DATA + 8..DATA + 12].copy_from_slice(&5i32.to_le_bytes()); // ties B, condition bits inert
    memory
}

#[test]
fn ficomp_followed_by_another_x87_slot_addresses_the_popped_stack_correctly() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let (cpu, bus) = assert_program_matches_exact_insns(
            mode,
            ficomp_followed_by_fstp_program(),
            0x0f7f,
            6, // mov, fld, fld, ficomp, fstp, mov -- hlt never retires natively
        );
        assert_eq!(
            f32::from_bits(u32::from_le_bytes(
                bus.memory[DATA + 12..DATA + 16].try_into().unwrap()
            )),
            3.0, // A: what the pop left as the new ST(0)
            "mode={mode:?}"
        );
        assert_eq!(cpu.fpu.top(), 0, "mode={mode:?}");
        assert_eq!(
            cpu.perf_counters().jit_direct_side_exits,
            0,
            "mode={mode:?}"
        );
    }
}

/// FIDIV by an integer zero. The conversion itself is always finite (an integer can never convert
/// to NaN or infinity), so the division is what produces the infinity, and `emit_finite_guard`
/// inside `emit_binary_st0` catches it on the RESULT before any x87 state is touched. The native
/// side exits at that guard; the interpreter re-executes the instruction and records ZE. Only the
/// two integer instructions before the 0xDA slot retire natively every pass: the FIDIV always
/// re-takes the same exit, and the two-instruction tail behind it (mov, hlt) never gets hot enough
/// to compile on its own.
#[test]
fn a_fidiv_by_integer_zero_exits_before_touching_x87_state() {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let code = [
        0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax,1               integer, before
        0xd9, 0x05, 0x00, 0x02, 0x00, 0x00, // fld dword [0x200] ST(0)=5.0, mid-block
        0xda, 0x35, 0x04, 0x02, 0x00, 0x00, // fidiv dword [0x204]  /6, operand=0
        0xbb, 0x02, 0x00, 0x00, 0x00, // mov ebx,2               integer, after (interpreted)
        0xf4,
    ];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 4].copy_from_slice(&5.0f32.to_bits().to_le_bytes());
    memory[DATA + 4..DATA + 8].copy_from_slice(&0i32.to_le_bytes());

    let (cpu, _) =
        assert_program_matches_exact_insns(GswMode::Gsw586, memory, 0x0f7f, 2 /* mov, fld */);
    assert!(
        cpu.direct_stall_snapshot().side_exit_x87_eligibility > 0,
        "the infinite result must side-exit rather than be stored"
    );
    assert_ne!(cpu.fpu.status & 0x04, 0, "the interpreter recorded ZE");
}

// ---------------------------------------------------------------------------------------------
// Slice 39: Tier 2 m64 REAL forms (FLD/FST/FSTP m64, the eight 0xDC m64 arithmetic forms).
// ---------------------------------------------------------------------------------------------

const DATA2: usize = 0x300;

/// FLD m64, FADD m64, FDIV m64 and FSTP m64 (twice) chained in one block, with a value
/// unrepresentable in f32 (1e300) as the FIRST operand. m64 IS the native f64 representation,
/// so unlike `LoadF32` there is no conversion: `read_real64` returns the eight bytes
/// bit-reinterpreted, and a native emitter that copy-pasted `LoadF32`'s `vcvtss2sd` (reading
/// only the low four bytes as an f32) would read garbage instead of 1e300, diverging from the
/// interpreter immediately. The round-trip FSTP at the end stores that same value back out,
/// pinning bit-exact preservation end to end.
fn m64_arith_program() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let mut code = vec![
        0xdd, 0x05, // fld qword [DATA]         ST(0)=A=1e300
    ];
    code.extend_from_slice(&(DATA as u32).to_le_bytes());
    code.extend_from_slice(&[0xdd, 0x05]); // fld qword [DATA+8]       ST(0)=B, ST(1)=A
    code.extend_from_slice(&(DATA as u32 + 8).to_le_bytes());
    code.extend_from_slice(&[0xdc, 0x05]); // fadd qword [DATA+16]     ST(0)=B+C
    code.extend_from_slice(&(DATA as u32 + 16).to_le_bytes());
    code.extend_from_slice(&[0xdc, 0x35]); // fdiv qword [DATA+24]     ST(0)=(B+C)/D
    code.extend_from_slice(&(DATA as u32 + 24).to_le_bytes());
    code.extend_from_slice(&[0xdd, 0x1d]); // fstp qword [DATA2]       store+pop, ST(0)=A again
    code.extend_from_slice(&(DATA2 as u32).to_le_bytes());
    code.extend_from_slice(&[0xdd, 0x1d]); // fstp qword [DATA2+8]     round-trips A, stack empty
    code.extend_from_slice(&(DATA2 as u32 + 8).to_le_bytes());
    code.extend_from_slice(&[
        0xdf, 0xe0, // fnstsw ax
        0x89, 0xc2, // mov edx,eax
        0xf4, // hlt
    ]);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 8].copy_from_slice(&1e300f64.to_bits().to_le_bytes()); // A
    memory[DATA + 8..DATA + 16].copy_from_slice(&2.0f64.to_bits().to_le_bytes()); // B
    memory[DATA + 16..DATA + 24].copy_from_slice(&3.0f64.to_bits().to_le_bytes()); // C
    memory[DATA + 24..DATA + 32].copy_from_slice(&2.0f64.to_bits().to_le_bytes()); // D
    memory
}

#[test]
fn fld_fadd_fdiv_and_fstp_m64_match_the_interpreter_and_preserve_the_full_range_value() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let (cpu, bus) = assert_program_matches_exact_insns(
            mode,
            m64_arith_program(),
            0x037f,
            8, // fld, fld, fadd, fdiv, fstp, fstp, fnstsw, mov -- hlt never retires natively
        );
        assert_eq!(
            f64::from_bits(u64::from_le_bytes(
                bus.memory[DATA2..DATA2 + 8].try_into().unwrap()
            )),
            2.5, // (B + C) / D = (2.0 + 3.0) / 2.0
            "mode={mode:?}: the arithmetic result"
        );
        assert_eq!(
            u64::from_le_bytes(bus.memory[DATA2 + 8..DATA2 + 16].try_into().unwrap()),
            1e300f64.to_bits(),
            "mode={mode:?}: 1e300 must round-trip bit-exact, with no f32 conversion in between"
        );
        assert_eq!(
            cpu.perf_counters().jit_direct_side_exits,
            0,
            "mode={mode:?}"
        );
    }
}

fn fcom_m64_program(operand: f64) -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let mut code = vec![0xb8, 0x01, 0x00, 0x00, 0x00]; // mov eax,1        integer, before
    code.extend_from_slice(&[0xdd, 0x05]); // fld qword [DATA]     ST(0)=5.0, mid-block
    code.extend_from_slice(&(DATA as u32).to_le_bytes());
    code.extend_from_slice(&[0xdc, 0x15]); // fcom qword [DATA+8]  /2, no pop
    code.extend_from_slice(&(DATA as u32 + 8).to_le_bytes());
    code.extend_from_slice(&[
        0xdf, 0xe0, // fnstsw ax
        0x89, 0xc2, // mov edx,eax
        0xbb, 0x02, 0x00, 0x00, 0x00, // mov ebx,2   integer, after
        0xf4,
    ]);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 8].copy_from_slice(&5.0f64.to_bits().to_le_bytes());
    memory[DATA + 8..DATA + 16].copy_from_slice(&operand.to_bits().to_le_bytes());
    memory
}

/// FCOM m64's three-way condition bits: ST(0) above, equal to and below the m64 operand. FCOM
/// does not pop, mirroring FICOM's separation from FCOMP.
#[test]
fn fcom_m64_condition_bits_match_the_interpreter_above_equal_and_below() {
    // ST(0) is 5.0. (operand, expected C3|C2|C0 bits).
    let cases = [
        (3.0, 0u16),    // ST(0) > operand: above
        (5.0, 1 << 14), // ST(0) == operand: equal
        (7.0, 1 << 8),  // ST(0) < operand: below
    ];
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for (operand, expected_bits) in cases {
            let (cpu, _) = assert_program_matches_exact_insns(
                mode,
                fcom_m64_program(operand),
                0x037f,
                6, // mov, fld, fcom, fnstsw, mov, mov -- hlt never retires natively
            );
            assert_eq!(
                cpu.fpu.status & 0x4500,
                expected_bits,
                "mode={mode:?} operand={operand}"
            );
            assert_eq!(cpu.perf_counters().jit_direct_side_exits, 0);
        }
    }
}

/// A 4-aligned-not-8-aligned m64 access: the positive control for the `alignment_bytes`
/// decision. `DATA + 4` is 4-aligned (0x204) but not 8-aligned, and the guard must admit it:
/// the interpreter's `read_qword` requires only 4-alignment per half (`fpu_exec.rs:720-740`),
/// so an 8-byte alignment requirement here would wrongly refuse a legitimate access.
///
/// Guard 3 does not widen this. The x87 site keeps both halves of the guard pointed at its side
/// exit, so a 2-aligned m64 is still refused and this row is still the boundary it was.
fn four_aligned_m64_program() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let mut code = vec![0xdd, 0x05]; // fld qword [DATA + 4]
    code.extend_from_slice(&(DATA as u32 + 4).to_le_bytes());
    code.extend_from_slice(&[0xdd, 0x1d]); // fstp qword [DATA2]
    code.extend_from_slice(&(DATA2 as u32).to_le_bytes());
    code.extend_from_slice(&[
        0x89, 0xc0, // mov eax,eax
        0x89, 0xdb, // mov ebx,ebx
        0xf4,
    ]);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA + 4..DATA + 12].copy_from_slice(&7.5f64.to_bits().to_le_bytes());
    memory
}

#[test]
fn a_four_aligned_not_eight_aligned_m64_access_lowers_and_matches() {
    let (cpu, bus) = assert_program_matches_exact_insns(
        GswMode::Gsw586,
        four_aligned_m64_program(),
        0x037f,
        4, // fld, fstp, mov, mov -- hlt never retires natively
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_exit_cross_page_or_alignment,
        0,
        "a 4-aligned m64 access must not side-exit on alignment"
    );
    assert_eq!(cpu.perf_counters().jit_direct_side_exits, 0);
    assert_eq!(
        f64::from_bits(u64::from_le_bytes(
            bus.memory[DATA2..DATA2 + 8].try_into().unwrap()
        )),
        7.5
    );
}

/// A 2-aligned m64 access: the alignment guard's negative control. `addr & 3 != 0` must side
/// exit before the crossing check ever runs.
fn two_aligned_m64_program() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let mut code = vec![0xdd, 0x05]; // fld qword [DATA + 2]
    code.extend_from_slice(&(DATA as u32 + 2).to_le_bytes());
    code.extend_from_slice(&[0xdd, 0x1d]); // fstp qword [DATA2]
    code.extend_from_slice(&(DATA2 as u32).to_le_bytes());
    code.extend_from_slice(&[
        0x89, 0xc0, // mov eax,eax
        0x89, 0xdb, // mov ebx,ebx
        0xf4,
    ]);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA + 2..DATA + 10].copy_from_slice(&11.25f64.to_bits().to_le_bytes());
    memory
}

#[test]
fn a_two_aligned_m64_access_side_exits_and_matches() {
    let (cpu, bus) = assert_program_matches(GswMode::Gsw586, two_aligned_m64_program(), 0x037f);
    assert!(cpu.perf_counters().jit_direct_exit_cross_page_or_alignment > 0);
    assert_eq!(
        f64::from_bits(u64::from_le_bytes(
            bus.memory[DATA2..DATA2 + 8].try_into().unwrap()
        )),
        11.25
    );
}

/// An m64 access at page offset 0xFFC: the crossing check's positive control, LIVE for Qword
/// only. `addr & 3 == 0` passes (0xFFC is 4-aligned), but the second dword half lands at
/// 0x1000..0x1004, across the page boundary, so `page_offset > 0x1000 - 8` must side exit. The
/// interpreter still completes it (as two separate dword transactions, one per page), so the
/// state and bus clocks must still match exactly.
fn cross_page_m64_program() -> Vec<u8> {
    let mut memory = vec![0; 0x2000];
    memory[ENTRY as usize - 1] = 0x90;
    let code = [
        0xdd, 0x05, 0xfc, 0x0f, 0x00, 0x00, // fld qword [0xffc]
        0xdd, 0x1d, 0x00, 0x03, 0x00, 0x00, // fstp qword [0x300]
        0x89, 0xc0, // mov eax,eax
        0x89, 0xdb, // mov ebx,ebx
        0xf4,
    ];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[0xffc..0x1004].copy_from_slice(&9.375f64.to_bits().to_le_bytes());
    memory
}

#[test]
fn cross_page_m64_memory_exit_reexecutes_precisely() {
    let (cpu, bus) = assert_program_matches(GswMode::Gsw586, cross_page_m64_program(), 0x037f);
    assert!(cpu.perf_counters().jit_direct_exit_cross_page_or_alignment > 0);
    assert_eq!(
        f64::from_bits(u64::from_le_bytes(
            bus.memory[0x300..0x308].try_into().unwrap()
        )),
        9.375
    );
}

fn fadd_m64_nan_program() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let mut code = vec![0xb8, 0x01, 0x00, 0x00, 0x00]; // mov eax,1        integer, before
    code.extend_from_slice(&[0xdd, 0x05]); // fld qword [DATA]     ST(0)=5.0, mid-block
    code.extend_from_slice(&(DATA as u32).to_le_bytes());
    code.extend_from_slice(&[0xdc, 0x05]); // fadd qword [DATA+8]  NaN operand
    code.extend_from_slice(&(DATA as u32 + 8).to_le_bytes());
    code.extend_from_slice(&[
        0xbb, 0x02, 0x00, 0x00, 0x00, // mov ebx,2   integer, after (interpreted)
        0xf4,
    ]);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 8].copy_from_slice(&5.0f64.to_bits().to_le_bytes());
    memory[DATA + 8..DATA + 16].copy_from_slice(&f64::NAN.to_bits().to_le_bytes());
    memory
}

/// FADD m64 with a NaN bit pattern operand. A NaN or infinity is legal in guest memory for a
/// Tier 2 memory operand (unlike LoadF32/BinaryMemory's F32 forms, this is the arm the 0xDA
/// arm's comment warns must carry the guard it omits), so `emit_finite_guard` on the loaded
/// operand catches it and side exits before any x87 state changes; the interpreter finishes the
/// instruction and the result is NaN.
#[test]
fn fadd_m64_with_a_nan_operand_side_exits_and_matches_the_interpreter() {
    let (cpu, _) = assert_program_matches_exact_insns(
        GswMode::Gsw586,
        fadd_m64_nan_program(),
        0x037f,
        2, // mov, fld
    );
    assert!(
        cpu.direct_stall_snapshot().side_exit_x87_eligibility > 0,
        "a NaN operand must side-exit at the finite guard rather than be stored"
    );
    assert!(cpu.fpu.get(0).is_nan(), "the interpreter's result is NaN");
}

fn fcom_m64_nan_program() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let mut code = vec![0xb8, 0x01, 0x00, 0x00, 0x00]; // mov eax,1        integer, before
    code.extend_from_slice(&[0xdd, 0x05]); // fld qword [DATA]     ST(0)=5.0, mid-block
    code.extend_from_slice(&(DATA as u32).to_le_bytes());
    code.extend_from_slice(&[0xdc, 0x15]); // fcom qword [DATA+8]  /2, NaN operand
    code.extend_from_slice(&(DATA as u32 + 8).to_le_bytes());
    code.extend_from_slice(&[
        0xbb, 0x02, 0x00, 0x00, 0x00, // mov ebx,2   integer, after (interpreted)
        0xf4,
    ]);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 8].copy_from_slice(&5.0f64.to_bits().to_le_bytes());
    memory[DATA + 8..DATA + 16].copy_from_slice(&f64::NAN.to_bits().to_le_bytes());
    memory
}

/// THE CONTRACT FIXTURE: FCOM m64 against a NaN operand. Native side exits at the same finite
/// guard the FADD case above does (the guard in `BinaryMemoryF64`'s emit arm is unconditional,
/// covering compares too), so it is the interpreter that writes the unordered condition triple
/// C3=C2=C0, and the differential comparison is what proves native did not instead reach
/// `emit_compare` and write C3 alone.
#[test]
fn fcom_m64_with_a_nan_operand_side_exits_and_the_interpreter_writes_the_unordered_triple() {
    let (cpu, _) = assert_program_matches_exact_insns(
        GswMode::Gsw586,
        fcom_m64_nan_program(),
        0x037f,
        2, // mov, fld
    );
    assert!(cpu.direct_stall_snapshot().side_exit_x87_eligibility > 0);
    assert_eq!(
        cpu.fpu.status & 0x4500,
        (1 << 14) | (1 << 10) | (1 << 8),
        "unordered: C3=C2=C0"
    );
}

/// The `top_delta` trap, mirrored from `ficomp_followed_by_fstp_program` for the 0xDC m64 slice.
/// FCOMP m64 (0xDC extension 3, `ComparePop`) pops, so the x87 slot immediately behind it must
/// address the PHYSICAL register the pop left as the new ST(0), not the one that was ST(0)
/// before the pop. If `BinaryMemoryF64` fell out of `top_delta`'s `op.pops()` group (the mutation
/// this fixture exists to catch), the compile-time TOP tracking used for the follow-on FSTP would
/// stay stale, and it would either address the wrong physical register (corrupting the stored
/// value) or trip the empty-tag guard on the register the real runtime pop just vacated. Either
/// way the exact-retirement gate and the state comparison below catch it; a correct compile does
/// neither.
fn fcomp_m64_followed_by_fstp_program() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let mut code = vec![0xb8, 0x01, 0x00, 0x00, 0x00]; // mov eax,1     integer, before
    code.extend_from_slice(&[0xdd, 0x05]); // fld qword [DATA]       ST(0)=A
    code.extend_from_slice(&(DATA as u32).to_le_bytes());
    code.extend_from_slice(&[0xdd, 0x05]); // fld qword [DATA+8]     ST(0)=B, ST(1)=A
    code.extend_from_slice(&(DATA as u32 + 8).to_le_bytes());
    code.extend_from_slice(&[0xdc, 0x1d]); // fcomp qword [DATA+16]  pops: ST(0)=A now
    code.extend_from_slice(&(DATA as u32 + 16).to_le_bytes());
    code.extend_from_slice(&[0xdd, 0x1d]); // fstp qword [DATA2]     the top_delta trap
    code.extend_from_slice(&(DATA2 as u32).to_le_bytes());
    code.extend_from_slice(&[
        0xbb, 0x02, 0x00, 0x00, 0x00, // mov ebx,2   integer, after
        0xf4,
    ]);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 8].copy_from_slice(&3.0f64.to_bits().to_le_bytes()); // A
    memory[DATA + 8..DATA + 16].copy_from_slice(&5.0f64.to_bits().to_le_bytes()); // B
    memory[DATA + 16..DATA + 24].copy_from_slice(&5.0f64.to_bits().to_le_bytes()); // ties B, condition bits inert
    memory
}

#[test]
fn fcomp_m64_followed_by_another_x87_slot_addresses_the_popped_stack_correctly() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let (cpu, bus) = assert_program_matches_exact_insns(
            mode,
            fcomp_m64_followed_by_fstp_program(),
            0x037f,
            6, // mov, fld, fld, fcomp, fstp, mov -- hlt never retires natively
        );
        assert_eq!(
            f64::from_bits(u64::from_le_bytes(
                bus.memory[DATA2..DATA2 + 8].try_into().unwrap()
            )),
            3.0, // A: what the pop left as the new ST(0)
            "mode={mode:?}"
        );
        assert_eq!(cpu.fpu.top(), 0, "mode={mode:?}");
        assert_eq!(
            cpu.perf_counters().jit_direct_side_exits,
            0,
            "mode={mode:?}"
        );
    }
}

fn fld_m64_nan_program() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let mut code = vec![0xb8, 0x01, 0x00, 0x00, 0x00]; // mov eax,1   integer, before
    code.extend_from_slice(&[0xdd, 0x05]); // fld qword [DATA]   NaN operand, mid-block
    code.extend_from_slice(&(DATA as u32).to_le_bytes());
    code.extend_from_slice(&[
        0xbb, 0x02, 0x00, 0x00, 0x00, // mov ebx,2   integer, after (interpreted)
        0xf4,
    ]);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 8].copy_from_slice(&f64::NAN.to_bits().to_le_bytes());
    memory
}

/// `LoadF64`'s OWN finite guard, as opposed to `BinaryMemoryF64`'s: both existing m64 NaN
/// fixtures (`fadd_m64_nan_program`, `fcom_m64_nan_program`) put the NaN in the SECOND operand,
/// consumed by the 0xDC arithmetic slot's guard, and never touch the guard inside `LoadF64`'s own
/// emit arm. This one FLDs the NaN bit pattern directly, so the only guard standing between it
/// and the resident cache is `LoadF64`'s. The native side must side-exit at the FLD itself, never
/// completing the push; the retirement count proves the FLD did not lower (only the leading `mov`
/// retires natively), and the interpreter, re-executing the FLD, pushes the NaN.
#[test]
fn fld_m64_with_a_nan_operand_side_exits_before_pushing_it() {
    let (cpu, _) = assert_program_matches_exact_insns(
        GswMode::Gsw586,
        fld_m64_nan_program(),
        0x037f,
        1, // mov -- the fld itself must side-exit rather than retire natively
    );
    assert!(
        cpu.direct_stall_snapshot().side_exit_x87_eligibility > 0,
        "a NaN operand must side-exit at LoadF64's own finite guard rather than be pushed"
    );
    assert!(cpu.fpu.get(0).is_nan(), "the interpreter pushed the NaN");
}

// ---------------------------------------------------------------------------------------------
// Slice 40: FILD m64 (0xDF /5). FILD-only scope: `StoreI64` (FISTP m64, 0xDF /7) is deferred, so
// there is no store-side counterpart to any fixture below.
// ---------------------------------------------------------------------------------------------

/// Fixture 6 (the review outcome's corrected list): a follow-on x87 slot AFTER the FILD, pinning
/// `top_delta` at RUNTIME rather than only through the static `stack_effects_advance_every_top_
/// with_wraparound` unit test. FLD pushes A, FILD pushes B on top of it (`top_delta`'s -1, the
/// mutation this fixture exists to catch: mutation 2 in the design's battery), and the FSTP that
/// follows pops and stores ST(0). If `LoadI64` had been left OUT of the `top_delta` push group
/// (defaulting to 0, no stack movement), the compile-time TOP tracking used to address the FSTP
/// slot would stay stale and address the physical register FLD's A still occupies instead of the
/// one FILD's B was pushed into: the FSTP would store A a second time instead of B, and the pop
/// would leave the wrong physical register as the new ST(0). Both are hard state divergences the
/// differential and the exact-retirement gate below catch; a correct compile does neither.
fn fild_m64_followed_by_fstp_program() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let code = [
        0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax,1                    integer, before
        0xd9, 0x05, 0x00, 0x02, 0x00, 0x00, // fld dword [0x200]      ST(0)=A
        0xdf, 0x2d, 0x04, 0x02, 0x00, 0x00, // fild qword [0x204]     ST(0)=B, ST(1)=A
        0xdd, 0x1d, 0x0c, 0x02, 0x00, 0x00, // fstp qword [0x20c]     the top_delta trap
        0xbb, 0x02, 0x00, 0x00, 0x00, // mov ebx,2                    integer, after
        0xf4,
    ];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 4].copy_from_slice(&3.0f32.to_bits().to_le_bytes()); // A
    memory[DATA + 4..DATA + 12].copy_from_slice(&123_456_789_012i64.to_le_bytes()); // B
    memory
}

#[test]
fn fild_m64_followed_by_another_x87_slot_addresses_the_pushed_stack_correctly() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let (cpu, bus) = assert_program_matches_exact_insns(
            mode,
            fild_m64_followed_by_fstp_program(),
            0x0f7f,
            5, // mov, fld, fild, fstp, mov -- hlt never retires natively
        );
        assert_eq!(
            f64::from_bits(u64::from_le_bytes(
                bus.memory[DATA + 12..DATA + 20].try_into().unwrap()
            )),
            123_456_789_012_f64, // B: what the FSTP actually stored
            "mode={mode:?}"
        );
        assert_eq!(
            cpu.fpu.get(0),
            3.0,
            "mode={mode:?}: A must be the new ST(0) after the pop"
        );
        assert_eq!(
            cpu.perf_counters().jit_direct_side_exits,
            0,
            "mode={mode:?}"
        );
    }
}

// The x87 TOP-mismatch retire cap.
//
// A block whose baked entry TOP no longer matches the live TOP must not be entered -- that half is
// unconditional and is asserted below on every round. What the cap governs is the RE-SPECIALIZATION
// that used to follow every refusal: on a guest that cycles TOPs through one key it recompiles
// forever and buys nothing, so each key gets `X87_TOP_RETIRE_CAP` retires and then goes sticky.
//
// The x87 opcode sits eleven instructions into the block, past the entry slot, so the block really
// does execute natively before the mismatch (`jit-fixture-entry-position-trap`), and every round
// asserts the native run happened before it asserts anything about the refusal that follows.
const TOP_CAP_END: u32 = ENTRY + 24;

fn top_cap_memory() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let program = [
        0x89, 0xc0, 0x89, 0xc0, 0x89, 0xc0, 0x89, 0xc0, 0x89, 0xc0, 0x89, 0xc0, 0x89, 0xc0, 0x89,
        0xc0, 0x89, 0xc0, 0x89, 0xc0, 0x89, 0xc0, // eleven mov eax,eax
        0xd9, 0xe8, // fld1 -- lowers TOP from 0 to 7
    ];
    memory[ENTRY as usize..TOP_CAP_END as usize].copy_from_slice(&program);
    memory[TOP_CAP_END as usize] = 0xf4;
    memory
}

/// Compile and install the block at TOP = 0, run it natively (which leaves TOP = 7), then re-enter
/// it at the mismatched TOP. Returns nothing: every invariant is asserted here so each round of the
/// callers' loops is non-vacuous.
fn run_then_mismatch(native: &mut CpuGsw, bus: &mut TestBus, memory: &[u8], round: usize) {
    arm(native, 0x0f7f);
    native.registers.eip = ENTRY;
    bus.memory.copy_from_slice(memory);
    assert_eq!(native.fpu.top(), 0, "round {round}");
    // `install` only accepts a key the cache has already observed, and the first probe is what
    // marks it `Seen`.
    let key = jit::direct::key_for(native, ENTRY, true).expect("block key");
    let _ = native.jit_direct.probe(key);
    let compilation = jit::direct::compile(native, ENTRY, true).expect("x87 block compiles");
    assert_eq!(compilation.x87_entry_top, 0, "round {round}");
    let id = native
        .jit_direct
        .install(&compilation)
        .expect("x87 block installs");
    let block = native
        .jit_direct
        .block(id)
        .expect("installed block is live");

    let insns = native.perf_counters().jit_direct_insns;
    assert!(
        native.try_run_direct_block_for_test(bus, block).unwrap(),
        "round {round}: the block must run NATIVELY at its baked TOP, or the refusal below proves \
         nothing"
    );
    assert!(
        native.perf_counters().jit_direct_insns > insns,
        "round {round}: native run retired no instructions"
    );
    assert_eq!(
        native.fpu.top(),
        7,
        "round {round}: fld1 must have moved TOP"
    );

    // Same key, live TOP 7 against a baked 0.
    let rejects = native.perf_counters().jit_direct_reject_x87_top;
    native.registers.eip = ENTRY;
    assert!(
        !native.try_run_direct_block_for_test(bus, block).unwrap(),
        "round {round}: entry at a mismatched TOP must be refused"
    );
    assert_eq!(
        native.perf_counters().jit_direct_reject_x87_top - rejects,
        1,
        "round {round}: the refusal itself is never capped"
    );
}

#[test]
fn x87_top_mismatch_retires_are_capped_per_key() {
    let memory = top_cap_memory();
    let mut native = x87_cpu(GswMode::Gsw586);
    let mut bus = direct_memory(memory.clone());
    arm(&mut native, 0x0f7f);
    run_to_halt(&mut native, &mut bus);
    let key = jit::direct::key_for(&native, ENTRY, true).expect("block key");

    for round in 0..2 {
        run_then_mismatch(&mut native, &mut bus, &memory, round);
        assert!(
            matches!(
                native.jit_direct.probe(key),
                jit::direct::BlockProbe::Compile
            ),
            "round {round}: a within-budget mismatch must retire the key for recompilation"
        );
        let stalls = native.direct_stall_snapshot();
        assert_eq!(stalls.x87_top_retires_suppressed, 0, "round {round}");
        assert_eq!(
            stalls.x87_top_sticky_crossings,
            u64::from(round == 1),
            "round {round}: the cap is crossed exactly once, on the last retire it allows"
        );
    }

    // Third mismatch on the same key: the budget is spent.
    run_then_mismatch(&mut native, &mut bus, &memory, 2);
    let stalls = native.direct_stall_snapshot();
    assert_eq!(stalls.x87_top_retires_suppressed, 1);
    assert_eq!(
        stalls.x87_top_sticky_crossings, 1,
        "the crossing counter fires on the edge, not on every suppression"
    );
    assert!(
        matches!(
            native.jit_direct.probe(key),
            jit::direct::BlockProbe::Ready(_)
        ),
        "a sticky key keeps its compiled block -- only the demotion is suppressed"
    );
}

#[test]
fn x87_top_retire_budget_is_fresh_after_the_page_is_rewritten() {
    let memory = top_cap_memory();
    let mut native = x87_cpu(GswMode::Gsw586);
    let mut bus = direct_memory(memory.clone());
    arm(&mut native, 0x0f7f);
    run_to_halt(&mut native, &mut bus);
    let key = jit::direct::key_for(&native, ENTRY, true).expect("block key");

    for round in 0..3 {
        run_then_mismatch(&mut native, &mut bus, &memory, round);
    }
    assert_eq!(native.direct_stall_snapshot().x87_top_retires_suppressed, 1);

    // The code at this address is now NEW: it must not inherit its predecessor's stickiness.
    let invalidation = native.jit_direct.invalidate_physical_range(ENTRY, 4, false);
    assert!(
        invalidation.blocks > 0,
        "the rewrite must actually have killed the block"
    );
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));

    run_then_mismatch(&mut native, &mut bus, &memory, 3);
    let stalls = native.direct_stall_snapshot();
    assert_eq!(
        stalls.x87_top_retires_suppressed, 1,
        "the post-rewrite mismatch must be retired, not suppressed"
    );
    assert!(
        matches!(
            native.jit_direct.probe(key),
            jit::direct::BlockProbe::Compile
        ),
        "the rewritten key spends a fresh budget"
    );
}

// =============================================================================================
// The two x87 rows of the tombraid FMV census's loop B, behind `IZARRAVM_FPU_LOOP_ROWS`.
//
// The other three rows of that slice (SAHF, DIV/IDIV memory, SETcc memory) are integer kinds and
// live in `cpu_jit_fpu_loop_rows_test.rs`, together with the gate's own spelling and default
// pins. These two are here because they are `NativeX87Insn` variants emitted by
// `x87_avx2_emit.rs`, and only this file's harness compares `CpuGsw.fpu`, the TOP and `fp_rem`.
//
// EVERY fixture below forces the arm: the shipped default is OFF, so one that read the ambient
// knob would compile a block that stops at the row and compare the interpreter against itself.
// =============================================================================================

/// Force the FPU-loop-row arm for this thread and prove the selection took.
fn select_fpu_loop_rows(enabled: bool) {
    jit::direct::set_fpu_loop_rows_for_test(Some(enabled));
    assert_eq!(
        jit::direct::fpu_loop_rows_enabled(),
        enabled,
        "the fixture override must decide the arm, not the ambient IZARRAVM_FPU_LOOP_ROWS"
    );
}

/// `fld dword [0x200]` / `fwait` / `fstp dword [0x204]` / `fwait`, the FMV loop's shape for the
/// WAIT row: the SECOND WAIT is not the block's first x87 slot, so it emits no gate of its own and
/// exercises the inherited-gate argument in `NativeX87Insn::Wait`.
fn wait_program() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let code = [
        0xd9, 0x05, 0x00, 0x02, 0x00, 0x00, // fld dword [0x200]
        0x9b, // fwait
        0xd9, 0x1d, 0x04, 0x02, 0x00, 0x00, // fstp dword [0x204]
        0x9b, // fwait
        0x89, 0xc0, // mov eax,eax
        0xf4,
    ];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 4].copy_from_slice(&1.5f32.to_bits().to_le_bytes());
    memory
}

/// WAIT retires NATIVELY when nothing is pending, in both the gate-emitting and the
/// gate-inherited position.
///
/// Catches: a `Wait` that never reached `emit_native_x87` at all -- the block would stop short and
/// `Exact(5)` fails. It also catches a `raw_clocks`/`fp_class` mistake, because
/// `assert_program_matches_impl` compares `elapsed_clocks`, `fp_rem` and the per-run `core_clocks`
/// vector, and WAIT is the only user of `FpOpClass::Wait`: copying `Register` there moves the
/// total.
#[test]
fn wait_retires_natively_when_no_exception_is_pending() {
    select_fpu_loop_rows(true);
    let (cpu, bus) = assert_program_matches_exact_insns(GswMode::Gsw586, wait_program(), 0x037f, 5);
    assert_eq!(
        f32::from_le_bytes(bus.memory[DATA + 4..DATA + 8].try_into().unwrap()),
        1.5
    );
    assert_eq!(cpu.fpu.status & 0x3f, 0);
    jit::direct::set_fpu_loop_rows_for_test(None);
}

/// WAIT's ARCHITECTURAL JOB: with an unmasked exception pending and CR0.NE set it must trap, and
/// with CR0.MP + CR0.TS it must raise #NM. Neither may be swallowed by the native form.
///
/// Catches an emit arm that is empty AND unguarded. The comparison is `run_straight_line`'s whole
/// result, so a native run that retired the WAIT where the interpreter faulted differs in the
/// returned fault, in EIP and in the x87 state.
///
/// This is the fixture the `NativeX87Insn::Wait` doc's "conservative but never permissive"
/// paragraph is written against: the native side may side-exit where the interpreter retires, but
/// never the reverse. CR0.MP | CR0.TS is WAIT's OWN #NM pair and is a different pair from the
/// CR0.EM | CR0.TS the shared gate tests, which is why it is exercised here rather than left to
/// `nm_and_mf_gates_match_interpreter_without_touching_x87_or_memory`.
#[test]
fn wait_delivers_the_pending_exception_and_the_task_switch_fault() {
    select_fpu_loop_rows(true);
    let memory = wait_program();
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
            direct.control.cr0 = CR0_MP | CR0_TS;
            interpreter.control.cr0 = CR0_MP | CR0_TS;
        }
        let fpu_before = direct.fpu.clone();
        let memory_before = direct_bus.memory.clone();
        let direct_result = direct.run_straight_line(&mut direct_bus, u64::MAX);
        let interpreter_result = interpreter.run_straight_line(&mut interpreter_bus, u64::MAX);
        assert_eq!(direct_result, interpreter_result, "pending_mf={pending_mf}");
        assert_eq!(direct.registers, interpreter.registers);
        assert_eq!(direct.fpu, interpreter.fpu);
        assert_eq!(direct.fpu, fpu_before, "the trap left x87 state moved");
        assert_eq!(direct_bus.memory, interpreter_bus.memory);
        assert_eq!(direct_bus.memory, memory_before);
        assert_eq!(direct.elapsed_clocks, interpreter.elapsed_clocks);
        assert_eq!(direct.fp_rem, interpreter.fp_rem);
    }
    jit::direct::set_fpu_loop_rows_for_test(None);
}

/// `fld dword [0x200]` / `frndint` / `fstp dword [0x204]`.
fn frndint_program(value: f32) -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let code = [
        0xd9, 0x05, 0x00, 0x02, 0x00, 0x00, // fld dword [0x200]
        0xd9, 0xfc, // frndint
        0xd9, 0x1d, 0x04, 0x02, 0x00, 0x00, // fstp dword [0x204]
        0x89, 0xc0, // mov eax,eax
        0xf4,
    ];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 4].copy_from_slice(&value.to_bits().to_le_bytes());
    memory
}

/// FRNDINT under ALL FOUR rounding-control modes, over the values that separate them.
///
/// Catches, and each is a real mutation of `emit_native_x87`'s `RoundToInt` arm:
///
/// * a BAKED immediate instead of the runtime branch -- three of the four RC arms then round the
///   wrong way, and `+/-2.5`, `+/-0.5` and `+/-1.5` each separate a different pair of modes;
/// * the RC field read from the wrong bits. `>> 10 & 3` is the only correct extraction; a `>> 8`
///   picks up the PRECISION-control field instead, which is why the `precision` loop varies it;
/// * the four immediates permuted -- `fpu_round_rc`'s arms are nearest/floor/ceil/trunc in that
///   order and `vroundsd`'s encoding agrees, so any permutation shows on a negative value;
/// * a missing `emit_store_physical`, which leaves ST(0) unrounded.
///
/// `assert_program_matches_exact_insns` pins FOUR native retirements, so a slot that quietly fell
/// back to the interpreter -- which would compare EQUAL on every value -- fails instead.
#[test]
fn frndint_matches_the_interpreter_under_every_rounding_mode() {
    select_fpu_loop_rows(true);
    for rc in 0..4u16 {
        for precision in [0x0000u16, 0x0300] {
            let control = 0x007f | precision | (rc << 10);
            for value in [
                2.5f32, -2.5, 1.5, -1.5, 0.5, -0.5, 0.0, -0.0, 3.7, -3.7, 4.0, -0.25,
            ] {
                let (_, bus) = assert_program_matches_exact_insns(
                    GswMode::Gsw586,
                    frndint_program(value),
                    control,
                    4,
                );
                let rounded =
                    f32::from_le_bytes(bus.memory[DATA + 4..DATA + 8].try_into().unwrap());
                let expected = match rc {
                    0 => f64::from(value).round_ties_even(),
                    1 => f64::from(value).floor(),
                    2 => f64::from(value).ceil(),
                    _ => f64::from(value).trunc(),
                } as f32;
                assert_eq!(
                    rounded.to_bits(),
                    expected.to_bits(),
                    "rc={rc} precision={precision:#06x} value={value}"
                );
            }
        }
    }
    jit::direct::set_fpu_loop_rows_for_test(None);
}

/// FRNDINT's operand guard: an EMPTY ST(0) must side exit rather than round whatever the resident
/// cache happened to hold.
///
/// Catches an arm that skipped `emit_load_physical` and read the physical XMM directly. The
/// program pops the stack empty first, so the tag word says Empty and only the interpreter's own
/// `fpu.get(0)` path may run.
#[test]
fn frndint_side_exits_on_an_empty_stack_slot() {
    select_fpu_loop_rows(true);
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize - 1] = 0x90;
    let code = [
        0xd9, 0x05, 0x00, 0x02, 0x00, 0x00, // fld dword [0x200]
        0xdd, 0xd8, // fstp st(0) -- pops, leaving ST(0) empty
        0xd9, 0xfc, // frndint
        0x89, 0xc0, // mov eax,eax
        0xf4,
    ];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA..DATA + 4].copy_from_slice(&2.5f32.to_bits().to_le_bytes());
    let (cpu, _) = assert_program_matches(GswMode::Gsw586, memory, 0x037f);
    assert!(
        cpu.perf_counters().jit_direct_side_exits > 0,
        "the empty-slot guard must have exited: {:?}",
        cpu.perf_counters()
    );
    jit::direct::set_fpu_loop_rows_for_test(None);
}
