// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const ENTRY: u32 = 0x501;
const RAM_TARGET: u32 = 0x3000;
const MODE13_TARGET: u32 = 0x000a_1000;

#[derive(Clone, Copy, Debug)]
enum Width {
    Byte,
    Dword,
}

impl Width {
    const fn bytes(self) -> usize {
        match self {
            Self::Byte => 1,
            Self::Dword => 4,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Form {
    Accumulator,
    GroupRegister,
    GroupMemory,
}

struct Fixture {
    native: CpuGsw,
    interpreter: CpuGsw,
    native_bus: TestBus,
    interpreter_bus: TestBus,
    block: jit::direct::CompiledBlock,
}

fn result_signature(result: ExecResult<CycleOutcome>) -> Result<CycleOutcome, (u8, Option<u32>)> {
    match result {
        Ok(outcome) => Ok(outcome),
        Err(InternalFault::Exception { vector, error_code }) => Err((vector, error_code)),
        Err(other) => panic!("unexpected internal fault: {other:?}"),
    }
}

fn instruction(form: Form, width: Width, imm: u32, target: Option<u32>) -> Vec<u8> {
    match (form, width) {
        (Form::Accumulator, Width::Byte) => vec![0xa8, imm as u8],
        (Form::Accumulator, Width::Dword) => {
            let mut code = vec![0xa9];
            code.extend_from_slice(&imm.to_le_bytes());
            code
        }
        (Form::GroupRegister, Width::Byte) => vec![0xf6, 0xc3, imm as u8],
        (Form::GroupRegister, Width::Dword) => {
            let mut code = vec![0xf7, 0xc3];
            code.extend_from_slice(&imm.to_le_bytes());
            code
        }
        (Form::GroupMemory, Width::Byte) => {
            let mut code = vec![0xf6, 0x05];
            code.extend_from_slice(&target.expect("memory TEST target").to_le_bytes());
            code.push(imm as u8);
            code
        }
        (Form::GroupMemory, Width::Dword) => {
            let mut code = vec![0xf7, 0x05];
            code.extend_from_slice(&target.expect("memory TEST target").to_le_bytes());
            code.extend_from_slice(&imm.to_le_bytes());
            code
        }
    }
}

fn flat_cpu(mode: GswMode) -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.jit_direct.set_fast_map_enabled_for_test(true);
    cpu.set_mode(mode);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers
        .set_segment(SegmentIndex::Cs, SegmentRegister::flat(0x08, 0x9b));
    for segment in [
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
        SegmentIndex::Fs,
        SegmentIndex::Gs,
    ] {
        cpu.registers
            .set_segment(segment, SegmentRegister::flat(0x10, 0x93));
    }
    cpu.set_eip(ENTRY);
    cpu
}

fn paged_cpu(mode: GswMode) -> CpuGsw {
    let mut cpu = flat_cpu(mode);
    cpu.control.cr0 |= CR0_PG | CR0_WP;
    cpu.control.cr3 = 0x3000;
    cpu.cpl = 3;
    cpu.registers
        .set_segment(SegmentIndex::Cs, SegmentRegister::flat(0x0b, 0xfb));
    for segment in [
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
        SegmentIndex::Fs,
        SegmentIndex::Gs,
    ] {
        cpu.registers
            .set_segment(segment, SegmentRegister::flat(0x13, 0xf3));
    }
    cpu
}

fn decode_fixture(cpu: &mut CpuGsw, bus: &mut TestBus, starts: &[u32]) {
    for &linear in starts {
        cpu.set_eip(linear);
        cpu.fetch_decoded(bus, linear).unwrap();
    }
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
    bus.trace = BusTrace::default();
}

fn map_read_page(
    cpu: &mut CpuGsw,
    bus: &mut TestBus,
    linear: u32,
    physical: u32,
    permissions: jit::fast_map::PagePermissions,
) {
    let read = bus
        .direct_page(physical, BusAccessKind::DataRead)
        .unwrap()
        .unwrap();
    assert!(
        cpu.jit_fast_map
            .populate_read(linear, physical, read, permissions)
    );
}

fn install_block(cpu: &mut CpuGsw) -> jit::direct::CompiledBlock {
    let key = jit::direct::key_for(cpu, ENTRY, true).unwrap();
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = jit::direct::compile(cpu, ENTRY, true).expect("TEST block compiles");
    assert_eq!(compilation.span.instructions, 3);
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("TEST block installs");
    cpu.jit_direct.block(id).unwrap()
}

fn arm(cpu: &mut CpuGsw, form: Form, value: u32) {
    cpu.halted = false;
    cpu.interrupt_shadow = false;
    cpu.registers.gpr.fill(0);
    cpu.registers.set_eax(0x55aa_3300);
    cpu.registers.set_ebx(0xaa55_cc00);
    match form {
        Form::Accumulator => cpu.registers.set_eax(value),
        Form::GroupRegister => cpu.registers.set_ebx(value),
        Form::GroupMemory => {}
    }
    cpu.registers.set_esi(0x1234_5678);
    cpu.registers.set_edi(0x89ab_cdef);
    cpu.registers.set_esp(0xc000);
    cpu.registers.eflags = 0x8d7;
    cpu.pending_flags = PendingFlags::default();
    let _ = cpu.alu(0, 0x7fff_ffff, 1, BusWidth::Dword);
    cpu.set_eip(ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
}

fn prepare_flat(
    mode: GswMode,
    form: Form,
    width: Width,
    value: u32,
    imm: u32,
    target: Option<u32>,
    permissions: jit::fast_map::PagePermissions,
) -> Fixture {
    let insn = instruction(form, width, imm, target);
    let memory_len = target
        .map(|address| address as usize + 0x2000)
        .unwrap_or(0x5000)
        .max(0x5000);
    let mut pristine = vec![0; memory_len];
    pristine[(ENTRY - 1) as usize] = 0x90;
    let mut code = insn.clone();
    code.extend_from_slice(&[0x89, 0xf6, 0x89, 0xff, 0xf4]);
    pristine[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    if let Some(target) = target {
        pristine[target as usize..target as usize + width.bytes()]
            .copy_from_slice(&value.to_le_bytes()[..width.bytes()]);
    }

    let mut native = flat_cpu(mode);
    let mut interpreter = flat_cpu(mode);
    let mut native_bus = TestBus::with_memory(pristine.clone());
    let mut interpreter_bus = TestBus::with_memory(pristine);
    for bus in [&mut native_bus, &mut interpreter_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [
        ENTRY,
        ENTRY + insn.len() as u32,
        ENTRY + insn.len() as u32 + 2,
    ];
    decode_fixture(&mut native, &mut native_bus, &starts);
    decode_fixture(&mut interpreter, &mut interpreter_bus, &starts);
    if let Some(target) = target {
        map_read_page(&mut native, &mut native_bus, target, target, permissions);
    }
    let block = install_block(&mut native);
    arm(&mut native, form, value);
    arm(&mut interpreter, form, value);
    native_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();
    Fixture {
        native,
        interpreter,
        native_bus,
        interpreter_bus,
        block,
    }
}

fn finish_and_compare(mut fixture: Fixture, context: &str) -> Fixture {
    let registers = fixture.native.registers.clone();
    let pending = fixture.native.pending_flags;
    let memory = fixture.native_bus.memory.clone();
    assert!(
        !fixture
            .native
            .try_run_direct_block_with_cap_for_test(&mut fixture.native_bus, fixture.block, 1)
            .unwrap(),
        "tight event cap admitted {context}"
    );
    assert_eq!(fixture.native.registers, registers, "cap changed {context}");
    assert_eq!(
        fixture.native.pending_flags, pending,
        "cap changed {context}"
    );
    assert_eq!(fixture.native_bus.memory, memory, "cap changed {context}");

    let retired = fixture.native.perf_counters().jit_direct_insns;
    assert!(
        fixture
            .native
            .try_run_direct_block_for_test(&mut fixture.native_bus, fixture.block)
            .unwrap(),
        "native block did not run: {context}"
    );
    for _ in 0..3 {
        fixture
            .interpreter
            .cycle(&mut fixture.interpreter_bus)
            .unwrap();
    }

    assert_eq!(
        fixture.native.registers, fixture.interpreter.registers,
        "registers differ: {context}"
    );
    assert_eq!(
        fixture.native.pending_flags, fixture.interpreter.pending_flags,
        "lazy flags differ: {context}"
    );
    assert_eq!(
        fixture.native.eflags(),
        fixture.interpreter.eflags(),
        "EFLAGS differ: {context}"
    );
    assert_eq!(
        fixture.native.elapsed_clocks, fixture.interpreter.elapsed_clocks,
        "clock charge differs: {context}"
    );
    assert_eq!(
        fixture.native.timing_rem, fixture.interpreter.timing_rem,
        "timing remainder differs: {context}"
    );
    assert_eq!(
        fixture.native_bus.trace.elapsed_clocks(),
        fixture.interpreter_bus.trace.elapsed_clocks(),
        "bus timing differs: {context}"
    );
    assert_eq!(
        fixture.native_bus.memory, fixture.interpreter_bus.memory,
        "memory differs: {context}"
    );
    assert_eq!(
        fixture.native_bus.mode13_dirty_pages, 0,
        "dirty read: {context}"
    );
    assert_eq!(
        fixture.native_bus.mode13_byte_writes, 0,
        "byte write: {context}"
    );
    assert_eq!(
        fixture.native_bus.mode13_dword_writes, 0,
        "dword write: {context}"
    );
    assert_eq!(fixture.native.perf_counters().jit_direct_insns - retired, 3);
    fixture
}

// 0x84 TEST r/m8, r8 register-form battery (Direct backend Slice A). The dword sibling 0x85
// (DirectKind::Test) is already covered elsewhere; this exercises the byte-width primitives
// (emit_read_store_value's AH/CH/DH/BH shift-right-8 lane, emit_test_preloaded's alu_r8_r8,
// and emit_logic_live_af) that opcode 0x84 newly reaches. Register form only: mod == 3.

fn test_byte_reg_code(rm: u8, reg: u8) -> Vec<u8> {
    vec![0x84, 0xc0 | (reg << 3) | rm]
}

fn arm_test_byte_reg(cpu: &mut CpuGsw, gpr: [u32; 4], eflags: u32, pending: Option<(u32, u32)>) {
    cpu.halted = false;
    cpu.interrupt_shadow = false;
    cpu.registers.gpr.fill(0);
    cpu.registers.set_eax(gpr[0]);
    cpu.registers.set_ecx(gpr[1]);
    cpu.registers.set_edx(gpr[2]);
    cpu.registers.set_ebx(gpr[3]);
    cpu.registers.set_esi(0x1234_5678);
    cpu.registers.set_edi(0x89ab_cdef);
    cpu.registers.set_esp(0xc000);
    cpu.registers.eflags = eflags;
    cpu.pending_flags = PendingFlags::default();
    if let Some((a, b)) = pending {
        // Leaves a pending ADD descriptor whose materialized AF is derived from a/b/result,
        // not from the raw eflags bit just set above: the two can be made to agree or disagree.
        let _ = cpu.alu(0, a, b, BusWidth::Dword);
    }
    cpu.set_eip(ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
}

fn prepare_test_byte_reg(
    mode: GswMode,
    rm: u8,
    reg: u8,
    gpr: [u32; 4],
    eflags: u32,
    pending: Option<(u32, u32)>,
) -> Fixture {
    let insn = test_byte_reg_code(rm, reg);
    let mut pristine = vec![0; 0x5000];
    pristine[(ENTRY - 1) as usize] = 0x90;
    let mut code = insn.clone();
    code.extend_from_slice(&[0x89, 0xf6, 0x89, 0xff, 0xf4]);
    pristine[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut native = flat_cpu(mode);
    let mut interpreter = flat_cpu(mode);
    let mut native_bus = TestBus::with_memory(pristine.clone());
    let mut interpreter_bus = TestBus::with_memory(pristine);
    for bus in [&mut native_bus, &mut interpreter_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [
        ENTRY,
        ENTRY + insn.len() as u32,
        ENTRY + insn.len() as u32 + 2,
    ];
    decode_fixture(&mut native, &mut native_bus, &starts);
    decode_fixture(&mut interpreter, &mut interpreter_bus, &starts);
    let block = install_block(&mut native);
    arm_test_byte_reg(&mut native, gpr, eflags, pending);
    arm_test_byte_reg(&mut interpreter, gpr, eflags, pending);
    native_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();
    Fixture {
        native,
        interpreter,
        native_bus,
        interpreter_bus,
        block,
    }
}

#[test]
fn test_byte_register_lanes_match_the_interpreter_in_586_mode() {
    // All 64 (rm, reg) byte lane pairs (AL/CL/DL/BL/AH/CH/DH/BH on both sides) at one
    // distinguishing value pattern. This is the only genuinely new code path: the high-byte
    // lane (AH/CH/DH/BH) through emit_read_store_value's shift-right-8 arm.
    let gpr = [0xaaaa_aa11, 0xbbbb_bb22, 0xcccc_cc33, 0xdddd_dd44];
    for rm in 0..8u8 {
        for reg in 0..8u8 {
            let context = format!("rm={rm} reg={reg}");
            finish_and_compare(
                prepare_test_byte_reg(GswMode::Gsw586, rm, reg, gpr, 0x202, None),
                &context,
            );
        }
    }
}

#[test]
fn test_byte_value_corners_and_af_pass_through_states() {
    // BL against AL, with the AF-carrying incoming states chosen so raw eflags.AF and the
    // pending descriptor's materialized AF agree in two states and disagree in the other two.
    // emit_logic_live_af must read the LIVE (materialized) AF, not the stale raw bit, so a
    // battery that only ever seeds an agreeing pair (as the existing arm() above does) cannot
    // tell a correct read from an accidentally dropped one.
    let states: [(u32, Option<(u32, u32)>); 4] = [
        (0x202, None),                   // AF=0 raw, no pending: agree at 0.
        (0x8d7, Some((0x7fff_ffff, 1))), // AF=1 raw, pending ADD materializes AF=1: agree at 1.
        (0x8d7, Some((0, 0))),           // AF=1 raw, pending ADD materializes AF=0: disagree.
        (0x8c7, Some((0x7fff_ffff, 1))), // AF=0 raw, pending ADD materializes AF=1: disagree.
    ];
    for (rm_value, reg_value) in [(0x80u32, 0xffu32), (0x7f, 0xff)] {
        // rm_value/reg_value also double as the SF boundary corner: with the high 24 bits
        // zeroed, a dword-width AND (the M5 mutation) reads SF from bit 31, which is always 0
        // here, while the correct byte-width AND reads bit 7, which differs between the two
        // pairs (0x80 sets it, 0x7f clears it).
        let gpr = [reg_value, 0, 0, rm_value];
        for (eflags, pending) in states {
            let context = format!(
                "rm_value={rm_value:#x} reg_value={reg_value:#x} eflags={eflags:#x} pending={pending:?}"
            );
            finish_and_compare(
                prepare_test_byte_reg(GswMode::Gsw586, 3, 0, gpr, eflags, pending),
                &context,
            );
        }
    }
}

#[test]
fn test_byte_register_form_smoke_in_486_mode() {
    let gpr = [0xaaaa_aa80, 0, 0, 0xdddd_ddff];
    finish_and_compare(
        prepare_test_byte_reg(GswMode::Gsw486, 3, 0, gpr, 0x202, None),
        "486 smoke rm=BL reg=AL",
    );
}

// The differential generator's only guard against a mis-widened byte TEST is that its
// terminal Jcc reads the flags 0x84 just defined (cpu_jit_differential_generator_test.rs).
// That invariant is enforced by comment only; nothing stops a later edit from inserting a
// flag-defining instruction between the two. This fixture puts the same shape (byte TEST
// immediately followed by the Jcc consuming its flags) directly next to emit_test_byte, as
// its own two-instruction block, so it cannot be dropped by an edit made anywhere else.

fn prepare_test_byte_terminal_jcc(mode: GswMode, gpr: [u32; 4]) -> Fixture {
    // rm=BL, reg=AL, same SF-boundary corner as test_byte_value_corners_and_af_pass_through_states:
    // AL & BL sets bit 7 of the byte result, while a dword-width AND (the M5 mutation) would
    // read SF from bit 31, which stays 0 here. JS reads that SF.
    let insn = test_byte_reg_code(3, 0);
    let mut pristine = vec![0; 0x5000];
    pristine[(ENTRY - 1) as usize] = 0x90;
    let mut code = insn.clone();
    code.extend_from_slice(&[0x78, 1, 0xf4, 0xf4]);
    pristine[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut native = flat_cpu(mode);
    let mut interpreter = flat_cpu(mode);
    let mut native_bus = TestBus::with_memory(pristine.clone());
    let mut interpreter_bus = TestBus::with_memory(pristine);
    for bus in [&mut native_bus, &mut interpreter_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [ENTRY, ENTRY + insn.len() as u32];
    decode_fixture(&mut native, &mut native_bus, &starts);
    decode_fixture(&mut interpreter, &mut interpreter_bus, &starts);

    let key = jit::direct::key_for(&native, ENTRY, true).expect("direct-eligible key");
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation =
        jit::direct::compile(&mut native, ENTRY, true).expect("TEST+Jcc block compiles");
    assert_eq!(
        compilation.span.instructions, 2,
        "block must end at the Jcc terminal, not run past it"
    );
    let id = native
        .jit_direct
        .install(&compilation)
        .expect("TEST+Jcc block installs");
    let block = native.jit_direct.block(id).unwrap();

    arm_test_byte_reg(&mut native, gpr, 0x202, None);
    arm_test_byte_reg(&mut interpreter, gpr, 0x202, None);
    native_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();
    Fixture {
        native,
        interpreter,
        native_bus,
        interpreter_bus,
        block,
    }
}

#[test]
fn test_byte_terminal_jcc_reads_the_last_flag_producer() {
    let gpr = [0xff, 0, 0, 0x80];
    let mut fixture = prepare_test_byte_terminal_jcc(GswMode::Gsw586, gpr);
    assert!(
        fixture
            .native
            .try_run_direct_block_for_test(&mut fixture.native_bus, fixture.block)
            .unwrap(),
        "native block did not run"
    );
    fixture
        .interpreter
        .cycle(&mut fixture.interpreter_bus)
        .unwrap();
    fixture
        .interpreter
        .cycle(&mut fixture.interpreter_bus)
        .unwrap();
    assert_eq!(
        fixture.native.registers.eip, fixture.interpreter.registers.eip,
        "branch outcome differs: byte TEST must set SF from bit 7, not bit 31"
    );
}

// 0x0FAF IMUL r32, r/m32 register-form battery (Direct backend Slice A companion). Register
// form only: mod == 3. dst is ModRM.reg, src is the r/m register. Unlike TEST, IMUL DEFINES
// CF/OF and PRESERVES SF/ZF/AF/PF exactly as they were, so the states below exist to catch a
// capture mask that is too wide (leaking the host multiply's own, guest-irrelevant SF/ZF/PF into
// the preserved flags) rather than one that is too narrow.

fn imul_reg_code(dst: u8, src: u8) -> Vec<u8> {
    vec![0x0f, 0xaf, 0xc0 | (dst << 3) | src]
}

fn arm_imul_reg(cpu: &mut CpuGsw, gpr: [u32; 8], eflags: u32, pending: Option<(u32, u32)>) {
    cpu.halted = false;
    cpu.interrupt_shadow = false;
    cpu.registers.gpr = gpr;
    cpu.registers.eflags = eflags;
    cpu.pending_flags = PendingFlags::default();
    if let Some((a, b)) = pending {
        // Leaves a pending ADD descriptor whose materialized SF/ZF/AF/PF come from a/b/result,
        // not from the raw eflags bits just set above: the two can be made to agree or disagree,
        // and IMUL must preserve whichever is live rather than recomputing from the product.
        let _ = cpu.alu(0, a, b, BusWidth::Dword);
    }
    cpu.set_eip(ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
}

fn prepare_imul_reg(
    mode: GswMode,
    dst: u8,
    src: u8,
    gpr: [u32; 8],
    eflags: u32,
    pending: Option<(u32, u32)>,
) -> Fixture {
    let insn = imul_reg_code(dst, src);
    let mut pristine = vec![0; 0x5000];
    pristine[(ENTRY - 1) as usize] = 0x90;
    let mut code = insn.clone();
    code.extend_from_slice(&[0x89, 0xf6, 0x89, 0xff, 0xf4]);
    pristine[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut native = flat_cpu(mode);
    let mut interpreter = flat_cpu(mode);
    let mut native_bus = TestBus::with_memory(pristine.clone());
    let mut interpreter_bus = TestBus::with_memory(pristine);
    for bus in [&mut native_bus, &mut interpreter_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [
        ENTRY,
        ENTRY + insn.len() as u32,
        ENTRY + insn.len() as u32 + 2,
    ];
    decode_fixture(&mut native, &mut native_bus, &starts);
    decode_fixture(&mut interpreter, &mut interpreter_bus, &starts);
    let block = install_block(&mut native);
    arm_imul_reg(&mut native, gpr, eflags, pending);
    arm_imul_reg(&mut interpreter, gpr, eflags, pending);
    native_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();
    Fixture {
        native,
        interpreter,
        native_bus,
        interpreter_bus,
        block,
    }
}

#[test]
fn imul_register_lanes_match_the_interpreter_in_586_mode() {
    // All 64 (dst, src) register pairs (EAX..EDI on both sides) at one value pattern mixing
    // overflowing and non-overflowing products, including same-register pairs (dst == src).
    let gpr = [
        0x0000_0002,
        0x7fff_ffff,
        0xffff_ffff,
        0x8000_0000,
        0x0001_0000,
        0x0000_0000,
        0x1234_5678,
        0x0001_0000,
    ];
    for dst in 0..8u8 {
        for src in 0..8u8 {
            let context = format!("dst={dst} src={src}");
            finish_and_compare(
                prepare_imul_reg(GswMode::Gsw586, dst, src, gpr, 0x202, None),
                &context,
            );
        }
    }
}

#[test]
fn imul_overflow_corners_and_af_pass_through_states() {
    // Overflow boundary corners: the REX.W killer (0x1_0000 * 0x1_0000, which sets CF=OF=1 at 32
    // bits but 0 at 64 bits: the mutation this whole battery exists to catch), the signed-max
    // boundary, -1 * -1 (must NOT set CF/OF), INT_MIN * -1, a zero operand, and 1 * INT_MIN.
    // Paired with the same AF-carrying incoming states test_byte uses (agree/disagree twice),
    // so a wrong (too-wide) flags-capture mask is caught by SF/ZF/PF as well as by AF.
    let corners: [(u32, u32); 6] = [
        (0x0001_0000, 0x0001_0000),
        (0x7fff_ffff, 2),
        (0xffff_ffff, 0xffff_ffff),
        (0x8000_0000, 0xffff_ffff),
        (0, 0x1234_5678),
        (1, 0x8000_0000),
    ];
    let states: [(u32, Option<(u32, u32)>); 4] = [
        (0x202, None),                   // no pending: eflags read live.
        (0x8d7, Some((0x7fff_ffff, 1))), // pending materializes AF=1: agrees with raw AF=1.
        (0x8d7, Some((0, 0))),           // pending materializes AF=0: disagrees with raw AF=1.
        (0x8c7, Some((0x7fff_ffff, 1))), // pending materializes AF=1: disagrees with raw AF=0.
    ];
    for (dst_value, src_value) in corners {
        let gpr = [dst_value, src_value, 0, 0, 0, 0, 0x1234_5678, 0x89ab_cdef];
        for (eflags, pending) in states {
            let context = format!(
                "dst_value={dst_value:#x} src_value={src_value:#x} eflags={eflags:#x} pending={pending:?}"
            );
            finish_and_compare(
                prepare_imul_reg(GswMode::Gsw586, 0, 1, gpr, eflags, pending),
                &context,
            );
        }
    }
}

#[test]
fn imul_register_form_smoke_in_486_mode() {
    let gpr = [
        0x0001_0000,
        0x0001_0000,
        0,
        0,
        0,
        0,
        0x1234_5678,
        0x89ab_cdef,
    ];
    finish_and_compare(
        prepare_imul_reg(GswMode::Gsw486, 0, 1, gpr, 0x202, None),
        "486 smoke dst=EAX src=ECX overflow corner",
    );
}

// A fixed two-instruction fixture, next to the IMUL battery above, that compiles nothing but
// IMUL immediately followed by the JO consuming its overflow flag. The differential generator's
// only guard against a mis-widened IMUL is that its terminal Jcc reads the flags 0x0FAF just
// defined; that invariant is enforced by comment only there, so this fixture puts the same shape
// directly under test as its own two-instruction block, immune to an edit made anywhere else.

fn prepare_imul_terminal_jo(mode: GswMode, gpr: [u32; 8]) -> Fixture {
    // dst=EAX, src=ECX, values chosen so the product overflows and JO is taken.
    let insn = imul_reg_code(0, 1);
    let mut pristine = vec![0; 0x5000];
    pristine[(ENTRY - 1) as usize] = 0x90;
    let mut code = insn.clone();
    code.extend_from_slice(&[0x70, 1, 0xf4, 0xf4]);
    pristine[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut native = flat_cpu(mode);
    let mut interpreter = flat_cpu(mode);
    let mut native_bus = TestBus::with_memory(pristine.clone());
    let mut interpreter_bus = TestBus::with_memory(pristine);
    for bus in [&mut native_bus, &mut interpreter_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [ENTRY, ENTRY + insn.len() as u32];
    decode_fixture(&mut native, &mut native_bus, &starts);
    decode_fixture(&mut interpreter, &mut interpreter_bus, &starts);

    let key = jit::direct::key_for(&native, ENTRY, true).expect("direct-eligible key");
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation =
        jit::direct::compile(&mut native, ENTRY, true).expect("IMUL+Jcc block compiles");
    assert_eq!(
        compilation.span.instructions, 2,
        "block must end at the Jcc terminal, not run past it"
    );
    let id = native
        .jit_direct
        .install(&compilation)
        .expect("IMUL+Jcc block installs");
    let block = native.jit_direct.block(id).unwrap();

    arm_imul_reg(&mut native, gpr, 0x202, None);
    arm_imul_reg(&mut interpreter, gpr, 0x202, None);
    native_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();
    Fixture {
        native,
        interpreter,
        native_bus,
        interpreter_bus,
        block,
    }
}

#[test]
fn imul_terminal_jcc_reads_the_last_flag_producer() {
    let gpr = [0x7fff_ffff, 2, 0, 0, 0, 0, 0x1234_5678, 0x89ab_cdef];
    let mut fixture = prepare_imul_terminal_jo(GswMode::Gsw586, gpr);
    assert!(
        fixture
            .native
            .try_run_direct_block_for_test(&mut fixture.native_bus, fixture.block)
            .unwrap(),
        "native block did not run"
    );
    fixture
        .interpreter
        .cycle(&mut fixture.interpreter_bus)
        .unwrap();
    fixture
        .interpreter
        .cycle(&mut fixture.interpreter_bus)
        .unwrap();
    assert_eq!(
        fixture.native.registers.eip, fixture.interpreter.registers.eip,
        "branch outcome differs: IMUL must set OF from the 32-bit truncated product, not the \
         64-bit one"
    );
}

// Guard rail for classify's IMUL arm: it must stay narrow to DWORD operand size. The memory form
// is lowered as of the ImulMem slice, and its guards live beside the other memory forms in
// cpu_jit_direct_timing_test.rs, which is where the direct-page mapping those fixtures need
// already exists. Neither the differential generator nor the battery above ever emits a
// 0x66-prefixed 0x0FAF, so nothing else catches this arm being widened by a later edit.

fn assert_imul_form_falls_to_interpreter(mode: GswMode, insn: &[u8], context: &str) {
    let mut pristine = vec![0; 0x5000];
    pristine[(ENTRY - 1) as usize] = 0x90;
    let mut code = insn.to_vec();
    code.extend_from_slice(&[0x89, 0xf6, 0x89, 0xff, 0xf4]);
    pristine[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut native = flat_cpu(mode);
    let mut native_bus = TestBus::with_memory(pristine);
    native_bus.direct_pages_enabled = true;
    native_bus.direct_page_clocks = true;
    // Warm every instruction boundary, not just the entry. Warming only the entry makes the walk
    // stop at a decode miss on slot 1 and return Retry, so the refusal assert below would pass
    // for the wrong reason no matter what classify does.
    let tail = ENTRY + insn.len() as u32;
    decode_fixture(
        &mut native,
        &mut native_bus,
        &[ENTRY, tail, tail + 2, tail + 4],
    );

    // classify must return None for the leading slot, so the whole leading run stays empty and
    // the block is unadmittable (design section 3: "slots.is_empty() is not relaxable"). If a
    // later edit widened the arm to accept this form, `compile` would start succeeding here.
    assert!(
        jit::direct::compile(&mut native, ENTRY, true).is_none(),
        "{context}: must fall to the interpreter, not compile as a native IMUL"
    );

    // Positive control. Without it the assertion above passes vacuously whenever the harness
    // stops compiling anything at all, which would silently retire both guard rails. The same
    // fixture with the accepted register form must still compile.
    assert_imul_register_form_still_compiles(mode, context);
}

/// Companion to `assert_imul_form_falls_to_interpreter`: proves the fixture can compile an IMUL
/// at all, so a refusal above is attributable to the operand form and not to a broken harness.
fn assert_imul_register_form_still_compiles(mode: GswMode, context: &str) {
    let mut pristine = vec![0; 0x5000];
    pristine[(ENTRY - 1) as usize] = 0x90;
    // 0F AF C1 is IMUL EAX, ECX, the accepted mod==3 dword form.
    let code = [0x0f, 0xaf, 0xc1, 0x89, 0xf6, 0x89, 0xff, 0xf4];
    pristine[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut native = flat_cpu(mode);
    let mut native_bus = TestBus::with_memory(pristine);
    native_bus.direct_pages_enabled = true;
    native_bus.direct_page_clocks = true;
    decode_fixture(
        &mut native,
        &mut native_bus,
        &[ENTRY, ENTRY + 3, ENTRY + 5, ENTRY + 7],
    );

    assert!(
        jit::direct::compile(&mut native, ENTRY, true).is_some(),
        "{context}: positive control failed, the register form must still compile here"
    );
}

#[test]
fn imul_word_operand_size_falls_to_the_interpreter() {
    // 66 0F AF C1: IMUL AX, CX. If the arm above the Word gate ever moved, or 0x0faf were added
    // to the gate's allowlist, this would silently lower as a 32-bit multiply: the destination's
    // high 16 bits would be clobbered instead of preserved, and CF/OF would be computed against
    // the wrong width. Must stay below the Word gate at classify.rs:26-30.
    assert_imul_form_falls_to_interpreter(
        GswMode::Gsw586,
        &[0x66, 0x0f, 0xaf, 0xc1],
        "word IMUL (66 0F AF /r)",
    );
}

#[test]
fn immediate_test_forms_match_the_interpreter_in_486_and_586_modes() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for form in [Form::Accumulator, Form::GroupRegister, Form::GroupMemory] {
            for (width, cases) in [
                (Width::Byte, [(0x55aa_3380, 0x80), (0x55aa_3355, 0xaa)]),
                (
                    Width::Dword,
                    [(0x8000_0001, 0x8000_0000), (0x55aa_3355, 0xaa00_ccaa)],
                ),
            ] {
                for (value, imm) in cases {
                    let target = matches!(form, Form::GroupMemory).then_some(RAM_TARGET);
                    let context = format!(
                        "mode={mode:?} form={form:?} width={width:?} value={value:#x} imm={imm:#x}"
                    );
                    finish_and_compare(
                        prepare_flat(
                            mode,
                            form,
                            width,
                            value,
                            imm,
                            target,
                            jit::fast_map::PagePermissions::UNPAGED,
                        ),
                        &context,
                    );
                }
            }
        }
    }
}

#[test]
fn mode13_test_reads_use_native_timing_without_writes_or_dirty_pages() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for width in [Width::Byte, Width::Dword] {
            finish_and_compare(
                prepare_flat(
                    mode,
                    Form::GroupMemory,
                    width,
                    0x8000_0080,
                    0x8000_0080,
                    Some(MODE13_TARGET),
                    jit::fast_map::PagePermissions::UNPAGED,
                ),
                &format!("Mode13 mode={mode:?} width={width:?}"),
            );
        }
    }
}

#[test]
fn read_only_and_watched_memory_is_read_without_store_side_effects() {
    for width in [Width::Byte, Width::Dword] {
        let mut fixture = prepare_flat(
            GswMode::Gsw586,
            Form::GroupMemory,
            width,
            0x8000_0080,
            0x8000_0080,
            Some(RAM_TARGET),
            jit::fast_map::PagePermissions {
                writable: false,
                user: true,
            },
        );
        fixture
            .native
            .decode_cache
            .mark_code_range(RAM_TARGET, width.bytes() as u8);
        let watch_exits = fixture.native.perf_counters().jit_direct_exit_code_watch;
        let invalidations = fixture.native.perf_counters().code_invalidations;
        let fixture = finish_and_compare(fixture, &format!("watched read-only {width:?}"));
        assert_eq!(
            fixture.native.perf_counters().jit_direct_exit_code_watch,
            watch_exits
        );
        assert_eq!(
            fixture.native.perf_counters().code_invalidations,
            invalidations
        );
        assert!(
            fixture
                .native
                .decode_cache
                .range_hits_code(RAM_TARGET, width.bytes() as u32)
        );
    }
}

#[test]
fn repeated_memory_test_root_obeys_the_memory_slot_and_host_page_caps() {
    const COUNT: usize = 32;
    let insn = instruction(
        Form::GroupMemory,
        Width::Dword,
        0x8000_0000,
        Some(RAM_TARGET),
    );
    let mut memory = vec![0; 0x5000];
    memory[(ENTRY - 1) as usize] = 0x90;
    let mut starts = Vec::with_capacity(COUNT);
    let mut cursor = ENTRY as usize;
    for _ in 0..COUNT {
        starts.push(cursor as u32);
        memory[cursor..cursor + insn.len()].copy_from_slice(&insn);
        cursor += insn.len();
    }
    memory[cursor] = 0xf4;
    memory[RAM_TARGET as usize..RAM_TARGET as usize + 4]
        .copy_from_slice(&0x8000_0000u32.to_le_bytes());

    let mut cpu = flat_cpu(GswMode::Gsw586);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    decode_fixture(&mut cpu, &mut bus, &starts);
    map_read_page(
        &mut cpu,
        &mut bus,
        RAM_TARGET,
        RAM_TARGET,
        jit::fast_map::PagePermissions::UNPAGED,
    );
    let compilation = jit::direct::compile(&mut cpu, ENTRY, true).unwrap();
    assert_eq!(compilation.span.instructions, 3);
    assert!(compilation.code.len() <= jit::exec_mem::host_page_len());
}

fn prepare_paged_case(
    target: u32,
    target_pte: u32,
    permissions: jit::fast_map::PagePermissions,
) -> Fixture {
    let insn = instruction(Form::GroupMemory, Width::Dword, 0x8000_0001, Some(target));
    let mut pristine = vec![0; 0xb000];
    pristine[(ENTRY - 1) as usize] = 0x90;
    let mut code = insn.clone();
    code.extend_from_slice(&[0x89, 0xf6, 0x89, 0xff, 0xf4]);
    pristine[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    pristine[0x3000..0x3004].copy_from_slice(&0x4027u32.to_le_bytes());
    pristine[0x4000..0x4004].copy_from_slice(&0x0027u32.to_le_bytes());
    pristine[0x4020..0x4024].copy_from_slice(&target_pte.to_le_bytes());
    pristine[0x4024..0x4028].copy_from_slice(&0x9027u32.to_le_bytes());
    pristine[target as usize..target as usize + 4].copy_from_slice(&0x8000_0001u32.to_le_bytes());

    let mut native = paged_cpu(GswMode::Gsw586);
    let mut interpreter = paged_cpu(GswMode::Gsw586);
    let mut native_bus = TestBus::with_memory(pristine.clone());
    let mut interpreter_bus = TestBus::with_memory(pristine);
    for bus in [&mut native_bus, &mut interpreter_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [
        ENTRY,
        ENTRY + insn.len() as u32,
        ENTRY + insn.len() as u32 + 2,
    ];
    decode_fixture(&mut native, &mut native_bus, &starts);
    decode_fixture(&mut interpreter, &mut interpreter_bus, &starts);
    map_read_page(
        &mut native,
        &mut native_bus,
        target & !0x0fff,
        target & !0x0fff,
        permissions,
    );
    let block = install_block(&mut native);
    arm(&mut native, Form::GroupMemory, 0);
    arm(&mut interpreter, Form::GroupMemory, 0);
    native_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();
    Fixture {
        native,
        interpreter,
        native_bus,
        interpreter_bus,
        block,
    }
}

#[test]
fn paged_read_only_memory_runs_natively_at_cpl3() {
    let permissions = jit::fast_map::PagePermissions {
        writable: false,
        user: true,
    };
    let mut fixture = prepare_paged_case(0x8000, 0x8025, permissions);
    for (cpu, bus) in [
        (&mut fixture.native, &mut fixture.native_bus),
        (&mut fixture.interpreter, &mut fixture.interpreter_bus),
    ] {
        assert_eq!(
            cpu.read_memory_sized(
                bus,
                SegmentIndex::Ds,
                0x8000,
                OperandSize::Dword,
                BusAccessKind::DataRead,
            )
            .unwrap(),
            0x8000_0001
        );
        assert_eq!(cpu.tlb.lookup(0x8000 >> 12).unwrap().phys, 0x8000);
        assert!(cpu.jit_fast_map.has_read_mapping(0x8000, 0x8000));
        bus.trace.clear();
    }
    finish_and_compare(fixture, "paged read-only TEST");
}

#[test]
fn paging_permission_and_cross_page_exits_precede_flags_and_operand_changes() {
    for (target, target_pte, permissions, cross, permission_fault) in [
        (
            0x8fff,
            0x8027,
            jit::fast_map::PagePermissions::UNPAGED,
            true,
            false,
        ),
        (
            0x8000,
            0x8023,
            jit::fast_map::PagePermissions {
                writable: true,
                user: false,
            },
            false,
            true,
        ),
    ] {
        let mut fixture = prepare_paged_case(target, target_pte, permissions);
        let registers = fixture.native.registers.clone();
        let pending = fixture.native.pending_flags;
        let memory = fixture.native_bus.memory.clone();
        let cross_exits = fixture
            .native
            .perf_counters()
            .jit_direct_exit_cross_page_or_alignment;
        let permission_exits = fixture.native.perf_counters().jit_direct_exit_permission;

        assert!(
            fixture
                .native
                .try_run_direct_block_for_test(&mut fixture.native_bus, fixture.block)
                .unwrap()
        );
        assert_eq!(fixture.native.registers, registers);
        assert_eq!(fixture.native.pending_flags, pending);
        assert_eq!(fixture.native_bus.memory, memory);
        assert_eq!(
            fixture
                .native
                .perf_counters()
                .jit_direct_exit_cross_page_or_alignment
                - cross_exits,
            u64::from(cross)
        );
        assert_eq!(
            fixture.native.perf_counters().jit_direct_exit_permission - permission_exits,
            u64::from(permission_fault)
        );

        fixture.native.jit_fast_map.invalidate_page(target);
        let native_decoded = fixture.native.decode_cache.get(ENTRY, true).unwrap();
        let interpreter_decoded = fixture.interpreter.decode_cache.get(ENTRY, true).unwrap();
        let native_result = fixture
            .native
            .execute_decoded(&native_decoded, &mut fixture.native_bus);
        let interpreter_result = fixture
            .interpreter
            .execute_decoded(&interpreter_decoded, &mut fixture.interpreter_bus);
        assert_eq!(
            result_signature(native_result),
            result_signature(interpreter_result),
            "target={target:#x}"
        );
        assert_eq!(fixture.native.registers, fixture.interpreter.registers);
        assert_eq!(
            fixture.native.pending_flags,
            fixture.interpreter.pending_flags
        );
        assert_eq!(fixture.native.eflags(), fixture.interpreter.eflags());
        assert_eq!(fixture.native.control.cr2, fixture.interpreter.control.cr2);
        assert_eq!(fixture.native_bus.memory, fixture.interpreter_bus.memory);
    }
}

// NEG leaves a LAZY descriptor rather than committing eflags, so the batteries above compare
// pending_flags and would still pass if the RBP host-flag shadow went stale: nothing in a
// three-slot block reads it. An in-block Jcc does, through emit_load_host_flags. This fixture
// pins that shape as its own two-instruction block next to the emitter, so the coverage cannot
// be lost by an edit elsewhere. JB reads CF, which NEG defines from the operand being non-zero,
// so this is what catches a capture mask narrowed away from CF. Note it canNOT catch a swapped
// a/b in the descriptor: the Jcc reads the RBP shadow through emit_load_host_flags, never the
// descriptor, and the shadow comes from the real host SUB either way.
fn prepare_neg_terminal_jcc(mode: GswMode, dst: u8, seed: u32) -> Fixture {
    let insn = vec![0xf7u8, 0xd8 | dst];
    let mut pristine = vec![0; 0x5000];
    pristine[(ENTRY - 1) as usize] = 0x90;
    let mut code = insn.clone();
    code.extend_from_slice(&[0x72, 1, 0xf4, 0xf4]);
    pristine[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut native = flat_cpu(mode);
    let mut interpreter = flat_cpu(mode);
    let mut native_bus = TestBus::with_memory(pristine.clone());
    let mut interpreter_bus = TestBus::with_memory(pristine);
    for bus in [&mut native_bus, &mut interpreter_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [ENTRY, ENTRY + insn.len() as u32];
    decode_fixture(&mut native, &mut native_bus, &starts);
    decode_fixture(&mut interpreter, &mut interpreter_bus, &starts);

    let key = jit::direct::key_for(&native, ENTRY, true).expect("direct-eligible key");
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation =
        jit::direct::compile(&mut native, ENTRY, true).expect("NEG+Jcc block compiles");
    assert_eq!(
        compilation.span.instructions, 2,
        "block must end at the Jcc terminal, not run past it"
    );
    let id = native
        .jit_direct
        .install(&compilation)
        .expect("NEG+Jcc block installs");
    let block = native.jit_direct.block(id).unwrap();

    arm_neg_reg(&mut native, dst, seed, 0x202, None);
    arm_neg_reg(&mut interpreter, dst, seed, 0x202, None);
    native_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();
    Fixture {
        native,
        interpreter,
        native_bus,
        interpreter_bus,
        block,
    }
}

#[test]
fn neg_terminal_jcc_reads_the_last_flag_producer() {
    // seed 0 gives CF=0 (JB not taken); any non-zero seed gives CF=1 (taken). Both directions,
    // so a shadow that is stale in one polarity cannot hide behind the other.
    for (seed, taken) in [(0x0000_0000u32, false), (0x8000_0000, true)] {
        let mut fixture = prepare_neg_terminal_jcc(GswMode::Gsw586, 3, seed);
        assert!(
            fixture
                .native
                .try_run_direct_block_for_test(&mut fixture.native_bus, fixture.block)
                .unwrap(),
            "seed {seed:#010x} must run natively"
        );
        fixture
            .interpreter
            .cycle(&mut fixture.interpreter_bus)
            .unwrap();
        fixture
            .interpreter
            .cycle(&mut fixture.interpreter_bus)
            .unwrap();
        // Pin the concrete destination, not just agreement: two seeds that both fell through
        // would agree with each other and prove nothing about the branch reading CF at all.
        let expected = if taken { ENTRY + 5 } else { ENTRY + 4 };
        assert_eq!(
            fixture.native.registers.eip,
            expected,
            "seed {seed:#010x}: JB must be {} here",
            if taken { "taken" } else { "not taken" }
        );
        assert_eq!(
            fixture.native.registers.eip, fixture.interpreter.registers.eip,
            "seed {seed:#010x}: the Jcc must branch on the flags NEG just defined"
        );
    }
}

/// NEG followed by an instruction that COMMITS the RBP flag shadow to eflags.
///
/// The batteries above compare pending_flags and cannot see a stale RBP, because nothing in a
/// block of plain moves ever reads it. IMUL does: it captures only CF/OF and then stores the
/// whole RBP word to eflags, which is how it reproduces the interpreter's
/// `set_flag(FLAG_CF|FLAG_OF, ..)` materialize-then-write. So if NEG updates RBP with too narrow
/// a mask, the AF it failed to refresh is published here as the guest's AF while the interpreter
/// materializes the correct one from NEG's descriptor.
///
/// Seed 0x08 makes NEG produce AF=1 while the incoming eflags carry AF=0, so a stale shadow is
/// the opposite of the right answer rather than accidentally equal to it.
fn prepare_neg_then_imul(mode: GswMode, seed: u32) -> Fixture {
    // neg eax; imul ebx, ecx; mov esi,esi; hlt
    let insn: Vec<u8> = vec![0xf7, 0xd8];
    let mut pristine = vec![0; 0x5000];
    pristine[(ENTRY - 1) as usize] = 0x90;
    let mut code = insn.clone();
    code.extend_from_slice(&[0x0f, 0xaf, 0xd9, 0x89, 0xf6, 0xf4]);
    pristine[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut native = flat_cpu(mode);
    let mut interpreter = flat_cpu(mode);
    let mut native_bus = TestBus::with_memory(pristine.clone());
    let mut interpreter_bus = TestBus::with_memory(pristine);
    for bus in [&mut native_bus, &mut interpreter_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [ENTRY, ENTRY + 2, ENTRY + 5];
    decode_fixture(&mut native, &mut native_bus, &starts);
    decode_fixture(&mut interpreter, &mut interpreter_bus, &starts);

    let key = jit::direct::key_for(&native, ENTRY, true).expect("direct-eligible key");
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation =
        jit::direct::compile(&mut native, ENTRY, true).expect("NEG+IMUL block compiles");
    assert_eq!(compilation.span.instructions, 3);
    let id = native
        .jit_direct
        .install(&compilation)
        .expect("NEG+IMUL block installs");
    let block = native.jit_direct.block(id).unwrap();

    arm_neg_reg(&mut native, 0, seed, 0x202, None);
    arm_neg_reg(&mut interpreter, 0, seed, 0x202, None);
    native_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();
    Fixture {
        native,
        interpreter,
        native_bus,
        interpreter_bus,
        block,
    }
}

#[test]
fn neg_refreshes_every_arithmetic_flag_in_the_shadow_for_a_later_committer() {
    for seed in [0x0000_0008u32, 0x0000_0010, 0x8000_0000, 0xffff_ffff] {
        let context = format!("seed={seed:#010x}");
        finish_and_compare(prepare_neg_then_imul(GswMode::Gsw586, seed), &context);
    }
}

fn arm_neg_reg(cpu: &mut CpuGsw, dst: u8, seed: u32, eflags: u32, pending: Option<(u32, u32)>) {
    cpu.halted = false;
    cpu.interrupt_shadow = false;
    // Every register gets a non-zero high half. NEG writes the FULL 32 bits, so a fixture that
    // zeroed the destination first could not tell a correct lowering from one that merged into
    // the low 8 or 16 bits, which is the likeliest implementation slip.
    for index in 0..8usize {
        cpu.registers.gpr[index] = 0xdead_0000 | index as u32;
    }
    // Before the seed, not after. ESP is gpr[4], so setting it afterwards would silently discard
    // the seed for dst == 4 and collapse all nine of that destination's cases onto one value.
    // The block touches no stack, so the value here is arbitrary.
    cpu.registers.set_esp(0xc000);
    cpu.registers.gpr[usize::from(dst)] = seed;
    cpu.registers.eflags = eflags;
    cpu.pending_flags = PendingFlags::default();
    if let Some((a, b)) = pending {
        let _ = cpu.alu(0, a, b, BusWidth::Dword);
    }
    cpu.set_eip(ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
}

fn prepare_neg_reg(
    mode: GswMode,
    dst: u8,
    seed: u32,
    eflags: u32,
    pending: Option<(u32, u32)>,
) -> Fixture {
    let insn = vec![0xf7u8, 0xd8 | dst];
    let mut pristine = vec![0; 0x5000];
    pristine[(ENTRY - 1) as usize] = 0x90;
    let mut code = insn.clone();
    code.extend_from_slice(&[0x89, 0xf6, 0x89, 0xff, 0xf4]);
    pristine[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut native = flat_cpu(mode);
    let mut interpreter = flat_cpu(mode);
    let mut native_bus = TestBus::with_memory(pristine.clone());
    let mut interpreter_bus = TestBus::with_memory(pristine);
    for bus in [&mut native_bus, &mut interpreter_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [
        ENTRY,
        ENTRY + insn.len() as u32,
        ENTRY + insn.len() as u32 + 2,
    ];
    decode_fixture(&mut native, &mut native_bus, &starts);
    decode_fixture(&mut interpreter, &mut interpreter_bus, &starts);
    let block = install_block(&mut native);
    arm_neg_reg(&mut native, dst, seed, eflags, pending);
    arm_neg_reg(&mut interpreter, dst, seed, eflags, pending);
    native_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();
    Fixture {
        native,
        interpreter,
        native_bus,
        interpreter_bus,
        block,
    }
}

#[test]
fn neg_register_form_matches_the_interpreter_across_destinations_and_corners() {
    // The seeds are chosen to discriminate, not to cover. 0 and 1 prove almost nothing: 0 gives
    // a = b = result = 0 with every flag clear, and 1 gives 0xffff_ffff whose ZF/SF read the
    // same at byte, word and dword width, so neither can catch a wrong width in the descriptor
    // tag. Each seed below breaks a specific mutation:
    //   0x0000_0100  a Byte-width tag (0x8000_0001) materializes ZF=1 instead of 0
    //   0x0001_0000  a Word-width tag (0x8000_0101) does the same
    //   0x8000_0000  the only OF=1 input; also CF=1, SF=1
    //   0xffff_ffff  result 1: CF=1, OF=0, SF=0, AF=1
    //   0x0000_0010  AF=0, the complement of the case above
    //   0x7fff_ffff / 0x0000_0080 / 0x0000_00ff  PF and SF spread
    // A swapped a/b in the descriptor inverts CF on every non-zero seed.
    let seeds = [
        0x0000_0100u32,
        0x0001_0000,
        0x8000_0000,
        0xffff_ffff,
        0x0000_0010,
        0x7fff_ffff,
        0x0000_0080,
        0x0000_00ff,
        0x0000_0000,
    ];
    for dst in 0..8u8 {
        for seed in seeds {
            let context = format!("dst={dst} seed={seed:#010x}");
            finish_and_compare(
                prepare_neg_reg(GswMode::Gsw586, dst, seed, 0x202, None),
                &context,
            );
        }
    }
}

#[test]
fn neg_register_form_preserves_an_incoming_pending_descriptor_shape() {
    // NEG overwrites pending_flags wholesale, so an incoming descriptor must not survive and
    // must not leak into the outgoing one. Seeded both with and without a live ADD descriptor,
    // and with raw eflags.AF set, so a lowering that merged rather than replaced would diverge.
    let states: [(u32, Option<(u32, u32)>); 3] = [
        (0x202, None),
        (0x8d7, Some((0x7fff_ffff, 1))),
        (0x202, Some((0x0000_00ff, 1))),
    ];
    for (eflags, pending) in states {
        for seed in [0x8000_0000u32, 0xffff_ffff, 0x0000_0100] {
            let context = format!("eflags={eflags:#x} pending={pending:?} seed={seed:#010x}");
            finish_and_compare(
                prepare_neg_reg(GswMode::Gsw586, 3, seed, eflags, pending),
                &context,
            );
        }
    }
}

#[test]
fn neg_register_form_matches_the_interpreter_in_486_mode() {
    finish_and_compare(
        prepare_neg_reg(GswMode::Gsw486, 0, 0x8000_0000, 0x202, None),
        "486 dst=0",
    );
}

fn arm_ror_reg(cpu: &mut CpuGsw, dst: u8, seed: u32, eflags: u32, pending: Option<(u32, u32)>) {
    cpu.halted = false;
    cpu.interrupt_shadow = false;
    for index in 0..8usize {
        cpu.registers.gpr[index] = 0xdead_0000 | index as u32;
    }
    // ESP BEFORE the seed. ESP is gpr[4], so setting it afterwards silently discards the seed for
    // dst == 4 and collapses that destination's whole case list onto one value. See arm_neg_reg.
    cpu.registers.set_esp(0xc000);
    cpu.registers.gpr[usize::from(dst)] = seed;
    cpu.registers.eflags = eflags;
    cpu.pending_flags = PendingFlags::default();
    if let Some((a, b)) = pending {
        // An ADD, which leaves a live Add descriptor with no CF override.
        let _ = cpu.alu(0, a, b, BusWidth::Dword);
    }
    cpu.set_eip(ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
}

fn prepare_ror_reg(
    mode: GswMode,
    dst: u8,
    count: u8,
    seed: u32,
    eflags: u32,
    pending: Option<(u32, u32)>,
) -> Fixture {
    // C1 /1 ib: mod 11, reg 1, rm dst. 0xc8 is mod 11 with reg 1.
    let insn = vec![0xc1u8, 0xc8 | dst, count];
    let mut pristine = vec![0; 0x5000];
    pristine[(ENTRY - 1) as usize] = 0x90;
    let mut code = insn.clone();
    code.extend_from_slice(&[0x89, 0xf6, 0x89, 0xff, 0xf4]);
    pristine[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut native = flat_cpu(mode);
    let mut interpreter = flat_cpu(mode);
    let mut native_bus = TestBus::with_memory(pristine.clone());
    let mut interpreter_bus = TestBus::with_memory(pristine);
    for bus in [&mut native_bus, &mut interpreter_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [
        ENTRY,
        ENTRY + insn.len() as u32,
        ENTRY + insn.len() as u32 + 2,
    ];
    decode_fixture(&mut native, &mut native_bus, &starts);
    decode_fixture(&mut interpreter, &mut interpreter_bus, &starts);
    let block = install_block(&mut native);
    arm_ror_reg(&mut native, dst, seed, eflags, pending);
    arm_ror_reg(&mut interpreter, dst, seed, eflags, pending);
    native_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();
    Fixture {
        native,
        interpreter,
        native_bus,
        interpreter_bus,
        block,
    }
}

// Counts chosen to hit every compile-time shape and both sides of the five-bit mask:
//   0  and 32  the no-op shape, 32 only via `& 0x1f`, so a missing mask is caught here
//   1          the materialising shape, the only count that defines OF
//   2, 7, 31   the in-place CF-override shape
//   16         the byte-swap idiom a rasterizer actually emits
const ROR_COUNTS: [u8; 7] = [0, 1, 2, 7, 16, 31, 32];

// Seeds chosen to discriminate rather than to cover. For a right rotate by n, CF is bit n-1 of the
// input, and at count 1 OF is the XOR of the result's top two bits:
//   0x0000_0001  ror 1 -> 0x8000_0000, top two bits 1 and 0, so OF=1 and CF=1
//   0x0000_0002  ror 1 -> 0x0000_0001, top two bits 0 and 0, so OF=0 and CF=0
//   0xc000_0000  ror 1 -> 0x6000_0000, top two bits 0 and 1, so OF=1 and CF=0
//   0x8000_0001  ror 1 -> 0xc000_0000, top two bits 1 and 1, so OF=0 and CF=1
// All four OF/CF combinations, so a lowering that tied OF to CF cannot pass.
//   0x0000_8000  bit 15 set, so it flips CF at count 16 against the seed below
//   0xffff_ffff  every rotate is a fixed point; only the flags can differ
//   0x1234_5678  an asymmetric value, so a rotate in the wrong DIRECTION is caught
const ROR_SEEDS: [u32; 8] = [
    0x0000_0001,
    0x0000_0002,
    0xc000_0000,
    0x8000_0001,
    0x0000_8000,
    0xffff_ffff,
    0x1234_5678,
    0x0000_0000,
];

#[test]
fn ror_register_form_matches_the_interpreter_across_destinations_counts_and_corners() {
    // eflags 0x202 has CF clear and 0x203 has it set, so the no-descriptor path is exercised in
    // both polarities: a lowering that never wrote CF passes one and fails the other.
    for dst in 0..8u8 {
        for count in ROR_COUNTS {
            for seed in ROR_SEEDS {
                for eflags in [0x202u32, 0x203] {
                    let context =
                        format!("dst={dst} count={count} seed={seed:#010x} eflags={eflags:#x}");
                    finish_and_compare(
                        prepare_ror_reg(GswMode::Gsw586, dst, count, seed, eflags, None),
                        &context,
                    );
                }
            }
        }
    }
}

#[test]
fn ror_register_form_updates_a_live_descriptor_in_place_without_materialising() {
    // THE case this slice turns on. At counts 2 through 31 the interpreter calls set_flag with a
    // mask of exactly FLAG_CF, which flips the descriptor's override bits IN PLACE and leaves
    // SF/ZF/PF/AF deferred. finish_and_compare asserts the raw pending_flags word, so a lowering
    // that materialised instead, or set the wrong override bit, or cleared the descriptor,
    // diverges here even though eflags() would still agree in some of those cases.
    let pendings = [
        Some((0x7fff_ffffu32, 1u32)),
        Some((0x0000_00ff, 1)),
        Some((0xffff_ffff, 1)),
        Some((0x0000_0000, 0)),
    ];
    for pending in pendings {
        for count in [2u8, 7, 16, 31] {
            for seed in ROR_SEEDS {
                for eflags in [0x202u32, 0x203] {
                    let context = format!(
                        "count={count} seed={seed:#010x} pending={pending:?} eflags={eflags:#x}"
                    );
                    finish_and_compare(
                        prepare_ror_reg(GswMode::Gsw586, 3, count, seed, eflags, pending),
                        &context,
                    );
                }
            }
        }
    }
}

#[test]
fn ror_register_form_materialises_a_live_descriptor_at_count_one() {
    // Count 1 is the other side of the compile-time split. Here the CF call sets the override and
    // the following OF call materialises WITH that override applied, then writes OF live and
    // clears the descriptor. Getting the order backwards publishes a CF taken from the stale
    // eflags instead of from the rotate.
    for pending in [
        Some((0x7fff_ffffu32, 1u32)),
        Some((0x0000_00ff, 1)),
        Some((0xffff_ffff, 1)),
    ] {
        for seed in ROR_SEEDS {
            for eflags in [0x202u32, 0x203] {
                let context = format!("seed={seed:#010x} pending={pending:?} eflags={eflags:#x}");
                finish_and_compare(
                    prepare_ror_reg(GswMode::Gsw586, 3, 1, seed, eflags, pending),
                    &context,
                );
            }
        }
    }
}

#[test]
fn ror_register_form_count_zero_touches_nothing_at_all() {
    // A zero count returns before the value write-back and before any flag. Both encodings of it
    // are checked: a literal 0, and 32, which is only zero after the five-bit mask. Asserted
    // against the pre-run state directly rather than only against the interpreter, so a lowering
    // that perturbed the guest identically on both sides could not hide.
    for count in [0u8, 32] {
        for pending in [None, Some((0x7fff_ffffu32, 1u32))] {
            let fixture = prepare_ror_reg(GswMode::Gsw586, 3, count, 0x1234_5678, 0x203, pending);
            let before_gpr = fixture.native.registers.gpr;
            let before_eflags = fixture.native.registers.eflags;
            let before_pending = fixture.native.pending_flags;
            let context = format!("count={count} pending={pending:?}");
            let after = finish_and_compare(fixture, &context);
            assert_eq!(
                after.native.registers.gpr[3], before_gpr[3],
                "{context}: a zero count must not touch the destination"
            );
            assert_eq!(
                after.native.registers.eflags, before_eflags,
                "{context}: a zero count must not touch eflags"
            );
            assert_eq!(
                after.native.pending_flags, before_pending,
                "{context}: a zero count must not touch the pending descriptor"
            );
        }
    }
}

#[test]
fn ror_register_form_matches_the_interpreter_in_486_mode() {
    for count in [1u8, 16] {
        finish_and_compare(
            prepare_ror_reg(GswMode::Gsw486, 0, count, 0x0000_8001, 0x202, None),
            &format!("486 count={count}"),
        );
    }
}

/// ROR at counts above 1 leaves a LAZY descriptor and only flips its override, so the batteries
/// above compare pending_flags and would still pass if the RBP host-flag shadow went stale:
/// nothing in a block of plain moves reads it. An in-block Jcc does, through emit_load_host_flags.
/// JB reads CF, which is the one flag a rotate defines at every count.
fn prepare_ror_terminal_jcc(dst: u8, count: u8, seed: u32) -> Fixture {
    let insn = vec![0xc1u8, 0xc8 | dst, count];
    let mut pristine = vec![0; 0x5000];
    pristine[(ENTRY - 1) as usize] = 0x90;
    let mut code = insn.clone();
    code.extend_from_slice(&[0x72, 1, 0xf4, 0xf4]);
    pristine[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut native = flat_cpu(GswMode::Gsw586);
    let mut interpreter = flat_cpu(GswMode::Gsw586);
    let mut native_bus = TestBus::with_memory(pristine.clone());
    let mut interpreter_bus = TestBus::with_memory(pristine);
    for bus in [&mut native_bus, &mut interpreter_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [ENTRY, ENTRY + insn.len() as u32];
    decode_fixture(&mut native, &mut native_bus, &starts);
    decode_fixture(&mut interpreter, &mut interpreter_bus, &starts);

    let key = jit::direct::key_for(&native, ENTRY, true).expect("direct-eligible key");
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation =
        jit::direct::compile(&mut native, ENTRY, true).expect("ROR+Jcc block compiles");
    assert_eq!(
        compilation.span.instructions, 2,
        "block must end at the Jcc terminal"
    );
    let id = native
        .jit_direct
        .install(&compilation)
        .expect("ROR+Jcc block installs");
    let block = native.jit_direct.block(id).unwrap();

    arm_ror_reg(&mut native, dst, seed, 0x202, None);
    arm_ror_reg(&mut interpreter, dst, seed, 0x202, None);
    native_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();
    Fixture {
        native,
        interpreter,
        native_bus,
        interpreter_bus,
        block,
    }
}

#[test]
fn ror_terminal_jcc_reads_the_carry_the_rotate_just_defined() {
    // For a right rotate by n, CF is bit n-1 of the input, so these pairs put a 0 and a 1 there at
    // both a count-1 and a multi-count shape. Both branch directions at both shapes.
    for (count, seed, taken) in [
        (1u8, 0x0000_0001u32, true),
        (1, 0x0000_0002, false),
        (16, 0x0000_8000, true),
        (16, 0x0000_0001, false),
    ] {
        let mut fixture = prepare_ror_terminal_jcc(3, count, seed);
        assert!(
            fixture
                .native
                .try_run_direct_block_for_test(&mut fixture.native_bus, fixture.block)
                .unwrap(),
            "count {count} seed {seed:#010x} must run natively"
        );
        for _ in 0..2 {
            fixture
                .interpreter
                .cycle(&mut fixture.interpreter_bus)
                .unwrap();
        }
        // Pin the concrete destination, not just agreement: two cases that both fell through would
        // agree with each other and prove nothing about the branch reading CF at all.
        let expected = if taken { ENTRY + 6 } else { ENTRY + 5 };
        assert_eq!(
            fixture.native.registers.eip,
            expected,
            "count {count} seed {seed:#010x}: JB must be {} here",
            if taken { "taken" } else { "not taken" }
        );
        assert_eq!(
            fixture.native.registers.eip, fixture.interpreter.registers.eip,
            "count {count} seed {seed:#010x}: the Jcc must branch on the carry ROR defined"
        );
    }
}

/// ROR followed by an instruction that COMMITS the whole RBP shadow to eflags. A rotate at counts
/// above 1 must leave SF, ZF, PF and AF alone in the shadow, which the batteries cannot see
/// because nothing there reads it. IMUL captures only CF and OF and then stores the entire RBP
/// word, so a capture mask widened past CF publishes the host rotate's leftovers as guest flags.
fn prepare_ror_then_imul(count: u8, seed: u32, pending: Option<(u32, u32)>) -> Fixture {
    // ror eax, count; imul ebx, ecx; mov esi,esi; hlt
    let insn = vec![0xc1u8, 0xc8, count];
    let mut pristine = vec![0; 0x5000];
    pristine[(ENTRY - 1) as usize] = 0x90;
    let mut code = insn.clone();
    code.extend_from_slice(&[0x0f, 0xaf, 0xd9, 0x89, 0xf6, 0xf4]);
    pristine[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut native = flat_cpu(GswMode::Gsw586);
    let mut interpreter = flat_cpu(GswMode::Gsw586);
    let mut native_bus = TestBus::with_memory(pristine.clone());
    let mut interpreter_bus = TestBus::with_memory(pristine);
    for bus in [&mut native_bus, &mut interpreter_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [ENTRY, ENTRY + 3, ENTRY + 6];
    decode_fixture(&mut native, &mut native_bus, &starts);
    decode_fixture(&mut interpreter, &mut interpreter_bus, &starts);

    let key = jit::direct::key_for(&native, ENTRY, true).expect("direct-eligible key");
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation =
        jit::direct::compile(&mut native, ENTRY, true).expect("ROR+IMUL block compiles");
    assert_eq!(compilation.span.instructions, 3);
    let id = native
        .jit_direct
        .install(&compilation)
        .expect("ROR+IMUL block installs");
    let block = native.jit_direct.block(id).unwrap();

    arm_ror_reg(&mut native, 0, seed, 0x202, pending);
    arm_ror_reg(&mut interpreter, 0, seed, 0x202, pending);
    native_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();
    Fixture {
        native,
        interpreter,
        native_bus,
        interpreter_bus,
        block,
    }
}

#[test]
fn ror_leaves_the_untouched_shadow_flags_alone_for_a_later_committer() {
    for count in [1u8, 2, 16, 31] {
        for pending in [None, Some((0x7fff_ffffu32, 1u32)), Some((0x0000_00ff, 1))] {
            for seed in [0x0000_0001u32, 0x0000_8000, 0xffff_ffff] {
                let context = format!("count={count} seed={seed:#010x} pending={pending:?}");
                finish_and_compare(prepare_ror_then_imul(count, seed, pending), &context);
            }
        }
    }
}

#[test]
fn group2_non_lowered_rotates_remain_interpreter_only() {
    for code in [
        vec![0xc1, 0xc3, 0x05], // /0 ROL r/m32, imm8: zero measured rejects, deliberately out
        vec![0xc1, 0xd3, 0x05], // /2 RCL: takes the incoming CF as a rotate input
        vec![0xc1, 0xdb, 0x05], // /3 RCR: same
        vec![0xd1, 0xc3],       // /0 ROL by 1
        vec![0xd1, 0xd3],       // /2 RCL by 1
        vec![0xd1, 0xdb],       // /3 RCR by 1
        vec![0xc0, 0xcb, 0x05], // the BYTE rotate group, not lowered at any sub-opcode
        vec![0xd0, 0xcb],       // byte ROR by 1
        vec![0xd2, 0xcb],       // byte ROR by CL
        vec![0xd3, 0xcb],       // /1 ROR by CL: a runtime count, deliberately out of this slice
        // ROR dword [disp32], the MEMORY form of the very sub-opcode this slice lowers. Without
        // this case, replacing the register-only `let-else` with a defaulting match would rotate
        // EAX instead and survive every register battery.
        vec![0xc1, 0x0d, 0x00, 0x50, 0x00, 0x00, 0x05],
        // 66-prefixed ROR r/m16. The OperandSize::Word allowlist is the only thing stopping this
        // from being lowered as a 32-bit rotate, which would smear the high half into the low one.
        vec![0x66, 0xc1, 0xcb, 0x05],
    ] {
        assert!(
            compile_leading_block(&code).is_none(),
            "group 2 {code:02x?} must stay interpreter-only"
        );
    }
}

#[test]
fn group2_dword_ror_register_form_is_lowered() {
    // The positive half of the guard above, and the ONLY test that can detect the new classify arm
    // being unreachable. Placing it below the existing `matches!(m.reg, 4..=7)` guard would make
    // it dead code, and every negative assertion above would still pass.
    for code in [
        vec![0xc1u8, 0xcb, 0x10], // ror ebx, 16
        vec![0xc1, 0xcb, 0x01],   // ror ebx, 1
        vec![0xc1, 0xcb, 0x00],   // ror ebx, 0, the no-op shape still has to ADMIT
        vec![0xd1, 0xcb],         // ror ebx, 1 via the 0xD1 encoding
    ] {
        assert_eq!(
            compile_leading_block(&code),
            Some(3),
            "ROR {code:02x?} must admit and carry the whole three-slot block"
        );
    }
}

/// Compile an instruction as slot 0 of a three-slot block, with every slot's decode line
/// warmed. The three-slot shape matters: warming only the entry line makes slot 1 miss, the walk
/// stops at Retry, and the fewer-than-three-slots gate reports the same `is_none()` as a genuine
/// structural reject. A negative assertion on that shape passes whether or not the opcode is
/// lowered, which is exactly how this test would have gone vacuous when NEG was admitted.
fn compile_leading_block(code: &[u8]) -> Option<u8> {
    let mut memory = vec![0; 0x5000];
    memory[(ENTRY - 1) as usize] = 0x90;
    let mut block = code.to_vec();
    block.extend_from_slice(&[0x89, 0xf6, 0x89, 0xff, 0xf4]);
    memory[ENTRY as usize..ENTRY as usize + block.len()].copy_from_slice(&block);
    let mut cpu = flat_cpu(GswMode::Gsw586);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    let starts = [
        ENTRY,
        ENTRY + code.len() as u32,
        ENTRY + code.len() as u32 + 2,
    ];
    decode_fixture(&mut cpu, &mut bus, &starts);
    let outcome = jit::direct::compile(&mut cpu, ENTRY, true);
    outcome
        .is_some()
        .then(|| outcome.unwrap().span.instructions)
}

fn arm_mul_reg(
    cpu: &mut CpuGsw,
    src: u8,
    eax_seed: u32,
    src_seed: u32,
    eflags: u32,
    pending: Option<(u32, u32)>,
) {
    cpu.halted = false;
    cpu.interrupt_shadow = false;
    // Distinct non-zero filler everywhere first, so a lowering that forgot to write EDX at all is
    // caught by EDX still holding filler rather than by it accidentally already being the answer.
    for index in 0..8usize {
        cpu.registers.gpr[index] = 0xfeed_0000 | index as u32;
    }
    // ESP FIRST, then the seeds. ESP is gpr[4], so setting it afterwards would silently discard
    // the multiplicand for src == 4 and collapse every one of that source's cases onto one value.
    // This is the exact ordering bug that voided nine cases of the NEG battery; see arm_neg_reg.
    // The block touches no stack, so the value here is arbitrary.
    cpu.registers.set_esp(0xc000);
    cpu.registers.gpr[0] = eax_seed;
    // AFTER eax, deliberately. For src == 0 this overwrites the accumulator seed, which is right:
    // `mul eax` squares one register and there is no second operand to hold. The seeds below are
    // chosen so the square still discriminates.
    cpu.registers.gpr[usize::from(src)] = src_seed;
    cpu.registers.eflags = eflags;
    cpu.pending_flags = PendingFlags::default();
    if let Some((a, b)) = pending {
        let _ = cpu.alu(0, a, b, BusWidth::Dword);
    }
    cpu.set_eip(ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
}

fn prepare_mul_reg(
    mode: GswMode,
    src: u8,
    eax_seed: u32,
    src_seed: u32,
    eflags: u32,
    pending: Option<(u32, u32)>,
) -> Fixture {
    let insn = vec![0xf7u8, 0xe0 | src];
    let mut pristine = vec![0; 0x5000];
    pristine[(ENTRY - 1) as usize] = 0x90;
    let mut code = insn.clone();
    code.extend_from_slice(&[0x89, 0xf6, 0x89, 0xff, 0xf4]);
    pristine[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut native = flat_cpu(mode);
    let mut interpreter = flat_cpu(mode);
    let mut native_bus = TestBus::with_memory(pristine.clone());
    let mut interpreter_bus = TestBus::with_memory(pristine);
    for bus in [&mut native_bus, &mut interpreter_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [
        ENTRY,
        ENTRY + insn.len() as u32,
        ENTRY + insn.len() as u32 + 2,
    ];
    decode_fixture(&mut native, &mut native_bus, &starts);
    decode_fixture(&mut interpreter, &mut interpreter_bus, &starts);
    let block = install_block(&mut native);
    arm_mul_reg(&mut native, src, eax_seed, src_seed, eflags, pending);
    arm_mul_reg(&mut interpreter, src, eax_seed, src_seed, eflags, pending);
    native_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();
    Fixture {
        native,
        interpreter,
        native_bus,
        interpreter_bus,
        block,
    }
}

#[test]
fn mul_register_form_matches_the_interpreter_across_sources_and_corners() {
    // Each pair breaks a specific mutation rather than merely covering the space:
    //   (0x0001_0000, 0x0001_0000)  product 0x1_0000_0000: EDX=1, EAX=0, CF=OF=1. The only pair
    //                               whose square ALSO overflows, so it still discriminates at
    //                               src == 0 where both operands collapse onto the multiplicand.
    //   (0xffff_ffff, 0x0000_0002)  product 0x1_ffff_fffe: EDX=1, EAX=0xfffffffe, CF=OF=1
    //                               unsigned. A SIGNED one-operand IMUL (the /5 sibling one modrm
    //                               bit away) computes -1 * 2 = -2 here, giving EDX=0xffffffff and
    //                               CF=OF=0, so this pair is what separates MUL from IMUL.
    //   (0x0000_ffff, 0x0000_0003)  product 0x2fffd: fits, EDX=0, CF=OF=0. The no-overflow case,
    //                               and EDX=0 differs from the 0xfeed_0002 filler, so a lowering
    //                               that never wrote EDX fails here rather than passing by luck.
    //   (0x8000_0000, 0x0000_0002)  product 0x1_0000_0000: EDX=1, EAX=0. Sign-bit input with an
    //                               unsigned result a signed multiply would get wrong.
    //   (0x1234_5678, 0x9abc_def0)  a pair whose high and low halves are both non-trivial and
    //                               unequal, so swapping the EAX and EDX writes cannot pass.
    let pairs = [
        (0x0001_0000u32, 0x0001_0000u32),
        (0xffff_ffff, 0x0000_0002),
        (0x0000_ffff, 0x0000_0003),
        (0x8000_0000, 0x0000_0002),
        (0x1234_5678, 0x9abc_def0),
    ];
    for src in 0..8u8 {
        for (eax_seed, src_seed) in pairs {
            let context = format!("src={src} eax={eax_seed:#010x} operand={src_seed:#010x}");
            finish_and_compare(
                prepare_mul_reg(GswMode::Gsw586, src, eax_seed, src_seed, 0x202, None),
                &context,
            );
        }
    }
}

#[test]
fn mul_register_form_materializes_an_incoming_pending_descriptor() {
    // The battery above runs with no descriptor live, where materialize_flags is a no-op and the
    // whole RBP-shadow argument is untested. MUL ends in set_flag(FLAG_CF | FLAG_OF, ..), a
    // multi-bit mask that cannot take the CF-override shortcut, so it MUST materialize whatever
    // was pending into eflags and then write only those two bits. Seeded with live ADD descriptors
    // whose SF/ZF/AF/PF disagree with both the raw eflags and with anything a host multiply leaves
    // behind, so capturing too wide a mask, or failing to commit the shadow, diverges here.
    let states: [(u32, Option<(u32, u32)>); 4] = [
        (0x202, Some((0x7fff_ffff, 1))),
        (0x8d7, Some((0x0000_00ff, 1))),
        (0x202, Some((0xffff_ffff, 1))),
        (0x8d7, None),
    ];
    for (eflags, pending) in states {
        // The third pair is the signedness discriminator. Without it this test survives a mutation
        // that emits the signed one-operand IMUL (/5) instead of MUL (/4), because the first two
        // pairs are small positives where the two agree. Measured: that mutation failed the other
        // three MUL tests and passed this one until this pair was added.
        for (eax_seed, src_seed) in [
            (0x0001_0000u32, 0x0001_0000u32),
            (0x0000_ffff, 0x0000_0003),
            (0xffff_ffff, 0x0000_0002),
        ] {
            let context = format!("eflags={eflags:#x} pending={pending:?} eax={eax_seed:#010x}");
            finish_and_compare(
                prepare_mul_reg(GswMode::Gsw586, 3, eax_seed, src_seed, eflags, pending),
                &context,
            );
        }
    }
}

#[test]
fn mul_register_form_reads_edx_and_esp_sources_before_writing_them() {
    // src == 2 supplies the multiplicand from EDX, the register MUL is about to overwrite with the
    // high half, so this pins that the emitted multiply reads the source home before either write.
    // src == 4 is ESP, the destination index whose seed the NEG battery once silently discarded.
    // Both use a multiplicand whose product overflows, so EDX genuinely changes rather than
    // happening to keep its value.
    for src in [2u8, 4] {
        for (eax_seed, src_seed) in [(0xffff_ffffu32, 0x0000_0002u32), (0x0001_0000, 0x0001_0000)] {
            let context = format!("src={src} eax={eax_seed:#010x} operand={src_seed:#010x}");
            let fixture = prepare_mul_reg(GswMode::Gsw586, src, eax_seed, src_seed, 0x202, None);
            // Positive control on the fixture itself: if a future edit reordered the seeding so
            // set_esp clobbered the multiplicand, this asserts at the cause instead of letting the
            // comparison pass on a collapsed case.
            assert_eq!(
                fixture.interpreter.registers.gpr[usize::from(src)],
                src_seed,
                "{context}: the multiplicand seed must survive into the source register"
            );
            finish_and_compare(fixture, &context);
        }
    }
}

#[test]
fn mul_register_form_matches_the_interpreter_in_486_mode() {
    finish_and_compare(
        prepare_mul_reg(GswMode::Gsw486, 3, 0xffff_ffff, 0x0000_0002, 0x202, None),
        "486 src=3",
    );
}

#[test]
fn group3_non_test_subops_remain_interpreter_only() {
    // Everything in group 3 except TEST (/0), the lowered dword NEG (/3), MUL (/4), IMUL (/5) and
    // the lowered dword DIV (/6) and IDIV (/7) REGISTER forms. The byte group 0xf6 stays entirely
    // interpreter-only, including its own /3 and /4.
    for code in [
        vec![0xf7, 0xcb], // /1 TEST alias, undocumented
        vec![0xf7, 0xd3], // /2 NOT r/m32
        vec![0xf6, 0xcb], // /1 byte
        vec![0xf6, 0xd3], // /2 NOT r/m8
        vec![0xf6, 0xdb], // /3 NEG r/m8, the byte form is NOT lowered
        vec![0xf6, 0xe3], // /4 MUL r/m8
        vec![0xf6, 0xeb], // /5 IMUL r/m8
        vec![0xf6, 0xf3], // /6 DIV r/m8
        vec![0xf6, 0xfb], // /7 IDIV r/m8
        // NEG dword [disp32]: the MEMORY form of the very sub-opcode this slice lowers. Without
        // this case, replacing the register-only `let-else` in classify with a defaulting match
        // would lower it as `NEG EAX` and survive the whole battery, including the differential
        // generator, which only ever emits the register encoding.
        vec![0xf7, 0x1d, 0x00, 0x50, 0x00, 0x00],
        // 66-prefixed NEG r/m16. The classify comment names the OperandSize::Word allowlist as
        // the only thing stopping this from being lowered as a 32-bit NEG; this pins it.
        vec![0x66, 0xf7, 0xdb],
        // MUL dword [disp32]: the MEMORY form of the sub-opcode this slice lowers. Without it,
        // replacing the register-only `let-else` in the /4 arm with a defaulting match would
        // multiply by EAX instead of by the memory operand and survive every register battery.
        vec![0xf7, 0x25, 0x00, 0x50, 0x00, 0x00],
        // 66-prefixed MUL r/m16, pinning the same OperandSize::Word allowlist for /4. A 16-bit MUL
        // writes DX and AX as halves of the existing EDX and EAX rather than replacing them, so
        // lowering it as the 32-bit form would clobber both high halves.
        vec![0x66, 0xf7, 0xe3],
        // DIV/IDIV dword [disp32]: the MEMORY forms of the two sub-opcodes the F7 slice lowered.
        // Both are register-only on purpose (a memory divide has two independent side-exit causes
        // at one slot, and every fixture's memory row is under the campaign's 100k floor), so
        // replacing the register-only `let-else` in that arm with a defaulting match would divide
        // by EAX instead of by the memory operand and survive every register battery.
        vec![0xf7, 0x35, 0x00, 0x50, 0x00, 0x00],
        vec![0xf7, 0x3d, 0x00, 0x50, 0x00, 0x00],
        // 66-prefixed DIV/IDIV r/m16, pinning the OperandSize::Word allowlist for /6 and /7 the
        // way the two entries above pin it for /3 and /4. A 16-bit DIV divides DX:AX and writes
        // only the low halves of EAX and EDX, so the 32-bit lowering would clobber both.
        vec![0x66, 0xf7, 0xf3],
        vec![0x66, 0xf7, 0xfb],
    ] {
        assert!(
            compile_leading_block(&code).is_none(),
            "group 3 {code:02x?} must stay interpreter-only"
        );
    }
}

#[test]
fn group3_dword_neg_register_form_is_lowered() {
    // The positive half of the guard above. Without it the negative list cannot distinguish
    // "rejected because this sub-opcode stayed out" from "rejected for an unrelated reason",
    // and a fixture that stopped compiling anything at all would still pass.
    assert_eq!(
        compile_leading_block(&[0xf7, 0xe3]),
        Some(3),
        "MUL EBX must admit and carry the whole three-slot block"
    );
    assert_eq!(
        compile_leading_block(&[0xf7, 0xdb]),
        Some(3),
        "NEG EBX must admit and carry the whole three-slot block"
    );
    // The F7 slice's three. Their differential cover is `cpu_jit_f7_group_test.rs`; what this
    // pins is ADMISSION, which is what the negative list above would otherwise be silent about.
    for (code, name) in [
        ([0xf7u8, 0xebu8], "IMUL EBX"),
        ([0xf7, 0xf3], "DIV EBX"),
        ([0xf7, 0xfb], "IDIV EBX"),
    ] {
        assert_eq!(
            compile_leading_block(&code),
            Some(3),
            "{name} must admit and carry the whole three-slot block"
        );
    }
}

fn warm_exact_poll(
    code: &[u8],
    entry: u32,
    starts: &[u32],
    ebx: u32,
    ecx: u32,
    edx: u32,
) -> (CpuGsw, TestBus) {
    let mut memory = vec![0xf4; 0x3000];
    memory[entry as usize..entry as usize + code.len()].copy_from_slice(code);
    let mut cpu = flat_cpu(GswMode::Gsw586);
    cpu.set_native_backend_enabled(false);
    cpu.registers.set_ebx(ebx);
    cpu.registers.set_ecx(ecx);
    cpu.registers.set_edx(edx);
    let mut bus = TestBus::with_memory(memory);
    bus.lazy_io_reads = true;
    for offset in starts {
        cpu.set_eip(entry + offset);
        cpu.fetch_decoded(&mut bus, entry + offset)
            .expect("poll instruction decode");
    }
    cpu.set_eip(entry);
    (cpu, bus)
}

fn exact_setup_poll_code(ecx: bool, paired: bool, mask: u8, jz: bool) -> Vec<u8> {
    let mut code = vec![
        0x89,
        if ecx { 0xca } else { 0xda },
        0x29,
        0xc0,
        0xec,
        0xa8,
        mask,
        if jz { 0x74 } else { 0x75 },
        if paired { 0x02 } else { 0xf7 },
    ];
    if paired {
        code.extend_from_slice(&[0xeb, 0xf5]);
    }
    code
}

fn exact_setup_poll_starts(paired: bool) -> &'static [u32] {
    if paired {
        &[0, 2, 4, 5, 7, 9]
    } else {
        &[0, 2, 4, 5, 7]
    }
}

#[test]
fn exact_current_dx_poll_shapes_cover_masks_and_branch_senses() {
    for mask in [0x01, 0x08] {
        for jz in [false, true] {
            let code = [0xec, 0xa8, mask, if jz { 0x74 } else { 0x75 }, 0xfb];
            let (mut cpu, _) = warm_exact_poll(&code, ENTRY, &[0, 1, 3], 0, 0, 0xaaaa_03da);
            let poll = cpu.poll_loop().expect("exact CurrentDx poll");
            assert_eq!(poll.diagnostic_class(), 0);
            assert_eq!(poll.raw_core_clocks(), 17);
            assert_eq!(poll.fetch_count(), 3);
            assert_eq!(poll.resolved_port(&cpu), 0x03da);
            assert_eq!(poll.status_mask(), mask);
            assert_eq!(poll.fresh_iteration_spins(0), jz);
            assert_eq!(poll.fresh_iteration_spins(mask), !jz);
        }
    }
}

#[test]
fn exact_setup_poll_shapes_cover_sources_senses_and_every_phase() {
    for ecx in [false, true] {
        for paired in [false, true] {
            for mask in [0x01, 0x08] {
                for jz in [false, true] {
                    let code = exact_setup_poll_code(ecx, paired, mask, jz);
                    let starts = exact_setup_poll_starts(paired);
                    let ebx = 0x1234_03da;
                    let ecx_value = 0x5678_03da;
                    let (mut cpu, _) =
                        warm_exact_poll(&code, ENTRY, starts, ebx, ecx_value, 0xaaaa_03da);
                    let poll = cpu.poll_loop().expect("exact setup poll");
                    assert_eq!(poll.diagnostic_class(), if paired { 2 } else { 1 });
                    assert_eq!(poll.raw_core_clocks(), if paired { 28 } else { 21 });
                    assert_eq!(poll.fetch_count(), starts.len());
                    assert_eq!(poll.status_mask(), mask);
                    assert_eq!(
                        poll.resolved_port(&cpu),
                        if ecx { ecx_value } else { ebx } as u16
                    );
                    assert_eq!(poll.fresh_iteration_spins(0), jz != paired);
                    assert_eq!(poll.fresh_iteration_spins(mask), jz == paired);
                    for (index, offset) in starts.iter().enumerate() {
                        let expected_len = if *offset == 4 { 1 } else { 2 };
                        assert_eq!(
                            poll.fetch(index),
                            Some((ENTRY + offset, ENTRY + offset, expected_len))
                        );
                        cpu.set_eip(ENTRY + offset);
                        let phase = cpu.poll_loop().expect("certified phase membership");
                        assert_eq!(phase.at_head(), index == 0, "offset={offset}");
                    }
                }
            }
        }
    }
}

#[test]
fn exact_setup_poll_rejects_mismatch_cold_prefix_mode_targets_page_cs_and_smc() {
    let (mut direct3, _) = warm_exact_poll(
        &[0xec, 0xa8, 0x08, 0x74, 0xfb],
        ENTRY,
        &[0, 1, 3],
        0,
        0,
        0x03da,
    );
    let mut direct3_cs = direct3.registers.cs();
    direct3_cs.default_size_32 = false;
    direct3.registers.set_segment(SegmentIndex::Cs, direct3_cs);
    assert!(
        direct3.poll_loop().is_none(),
        "the direct 3-slot form is 32-bit-code only"
    );

    const DIRECT: &[u8] = &[0x89, 0xda, 0x29, 0xc0, 0xec, 0xa8, 0x08, 0x74, 0xf7];
    const PAIRED: &[u8] = &[
        0x89, 0xda, 0x29, 0xc0, 0xec, 0xa8, 0x08, 0x74, 0x02, 0xeb, 0xf5,
    ];
    let direct_starts = exact_setup_poll_starts(false);
    let paired_starts = exact_setup_poll_starts(true);

    let (mut cpu, _) = warm_exact_poll(DIRECT, ENTRY, direct_starts, 0x1234, 0, 0xaaaa_03da);
    let mismatch = cpu
        .poll_loop()
        .expect("shape is independent of live source value");
    assert_eq!(cpu.registers.edx() as u16, 0x03da);
    assert_ne!(mismatch.resolved_port(&cpu), 0x03da);

    let mut cold = flat_cpu(GswMode::Gsw586);
    cold.set_native_backend_enabled(false);
    cold.registers.set_ebx(0x03da);
    cold.set_eip(ENTRY);
    assert!(cold.poll_loop().is_none());

    let mut cs = cpu.registers.cs();
    cs.limit = ENTRY + 7;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    assert!(cpu.poll_loop().is_none(), "setup direct live CS limit");
    cs.limit = u32::MAX;
    cs.default_size_32 = false;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    assert!(cpu.poll_loop().is_none(), "setup direct 16-bit code mode");

    let (mut cpu, _) = warm_exact_poll(PAIRED, ENTRY, paired_starts, 0x03da, 0, 0);
    let mut cs = cpu.registers.cs();
    cs.limit = ENTRY + 9;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    assert!(
        cpu.poll_loop().is_none(),
        "paired JMP exceeds live CS limit"
    );

    let prefixed = [0x66, 0x89, 0xda, 0x29, 0xc0, 0xec, 0xa8, 0x08, 0x74, 0xf6];
    let (mut cpu, _) = warm_exact_poll(&prefixed, ENTRY, &[0, 3, 5, 6, 8], 0x03da, 0, 0);
    assert!(cpu.poll_loop().is_none());

    let malformed: Vec<(&str, Vec<u8>, &[u32])> = vec![
        (
            "no-setup paired form",
            vec![0xec, 0xa8, 0x08, 0x74, 0x02, 0xeb, 0xf9],
            &[0, 1, 3, 5],
        ),
        (
            "wrong MOV source",
            vec![0x89, 0xd2, 0x29, 0xc0, 0xec, 0xa8, 0x08, 0x74, 0xf7],
            direct_starts,
        ),
        (
            "wrong MOV destination",
            vec![0x89, 0xd8, 0x29, 0xc0, 0xec, 0xa8, 0x08, 0x74, 0xf7],
            direct_starts,
        ),
        (
            "altered SUB",
            vec![0x89, 0xda, 0x29, 0xc9, 0xec, 0xa8, 0x08, 0x74, 0xf7],
            direct_starts,
        ),
        (
            "unsupported mask",
            vec![0x89, 0xda, 0x29, 0xc0, 0xec, 0xa8, 0x04, 0x74, 0xf7],
            direct_starts,
        ),
        (
            "wrong direct Jcc target",
            vec![0x89, 0xda, 0x29, 0xc0, 0xec, 0xa8, 0x08, 0x74, 0xf8],
            direct_starts,
        ),
        (
            "wrong paired Jcc target",
            vec![
                0x89, 0xda, 0x29, 0xc0, 0xec, 0xa8, 0x08, 0x74, 0x01, 0xeb, 0xf5,
            ],
            paired_starts,
        ),
        (
            "wrong paired JMP target",
            vec![
                0x89, 0xda, 0x29, 0xc0, 0xec, 0xa8, 0x08, 0x74, 0x02, 0xeb, 0xf4,
            ],
            paired_starts,
        ),
        (
            "non-short paired JMP",
            vec![
                0x89, 0xda, 0x29, 0xc0, 0xec, 0xa8, 0x08, 0x74, 0x02, 0xe9, 0xf2, 0xff, 0xff, 0xff,
            ],
            paired_starts,
        ),
    ];
    for (name, code, starts) in malformed {
        let (mut cpu, _) = warm_exact_poll(&code, ENTRY, starts, 0x03da, 0, 0);
        assert!(cpu.poll_loop().is_none(), "accepted {name}");
    }

    let (mut cpu, _) = warm_exact_poll(PAIRED, ENTRY, direct_starts, 0x03da, 0, 0);
    assert!(cpu.poll_loop().is_none(), "paired JMP remained cold");

    let (mut cpu, _) = warm_exact_poll(PAIRED, 0x0ff8, paired_starts, 0x03da, 0, 0);
    assert!(cpu.poll_loop().is_none());

    for (paired, mutations) in [
        (
            false,
            &[(1u32, 0xd2), (3, 0xc1), (4, 0xed), (6, 0x04), (8, 0xf8)][..],
        ),
        (
            true,
            &[
                (1u32, 0xd2),
                (3, 0xc1),
                (4, 0xed),
                (6, 0x04),
                (8, 0x01),
                (10, 0xf4),
            ][..],
        ),
    ] {
        let code = if paired { PAIRED } else { DIRECT };
        let starts = exact_setup_poll_starts(paired);
        for &(mutation, replacement) in mutations {
            let (mut cpu, mut bus) = warm_exact_poll(code, ENTRY, starts, 0x03da, 0, 0);
            assert!(cpu.poll_loop().is_some());
            bus.memory[(ENTRY + mutation) as usize] = replacement;
            assert!(cpu.note_code_write(ENTRY + mutation, 1));
            for offset in starts {
                cpu.set_eip(ENTRY + offset);
                cpu.fetch_decoded(&mut bus, ENTRY + offset)
                    .expect("restamped mutated poll decode");
            }
            cpu.set_eip(ENTRY);
            assert!(
                cpu.poll_loop().is_none(),
                "accepted paired={paired} mutation at offset {mutation}"
            );
        }
    }
}

/// Storage-layer semantics for the poll-classification negative cache: a live negative is keyed
/// on (lin, d) AND the page's insert generation, so `put` (a warm-line install, the one mutation
/// that can turn a structural negative into a match) retires every negative on its page, while
/// removals (narrow kills, whole-cache generation flushes) leave negatives live since they can
/// only shrink what would match, never grow it. Lives here rather than cpu_test.rs, which is at
/// its line-policy ceiling.
#[cfg(feature = "jit")]
#[test]
fn poll_negative_cache_page_generation_semantics() {
    let (mut nop_cpu, nop_mem) = real_mode_cpu(&[0x90], 0x10);
    let mut nop_bus = TestBus::with_memory(nop_mem);
    nop_cpu.begin_instruction();
    let insn = nop_cpu.decode(&mut nop_bus).expect("0x90 NOP decodes");

    let mut cache = DecodeCache::new(1024);
    let lin = 0x0010_2340u32;
    assert!(!cache.poll_negative_live(lin, true));
    cache.record_poll_negative(lin, true);
    assert!(cache.poll_negative_live(lin, true));
    // d is part of the key.
    assert!(!cache.poll_negative_live(lin, false));
    // A put on the SAME page retires the negative.
    let _ = cache.put(lin + 8, insn, true, lin + 8);
    assert!(!cache.poll_negative_live(lin, true));
    // Re-record, then a put on a DIFFERENT page whose generation slot does not alias lin's
    // (POLL_NEG_GEN_SLOTS is 1024 = 2^10, so an offset that is itself a multiple of 2^22 bytes
    // wraps the slot back to the same index; 0x10_0000 (256 pages) does not) leaves it live.
    cache.record_poll_negative(lin, true);
    let far = lin + 0x10_0000;
    let _ = cache.put(far, insn, true, far);
    assert!(cache.poll_negative_live(lin, true));
    // A whole-cache generation flush does NOT retire negatives (removals are benign); only
    // inserts do.
    cache.generation = cache.generation.wrapping_add(1);
    assert!(cache.poll_negative_live(lin, true));

    // Exercise the packed d bit directly: forge an entry for (lin2, d=false) in lin2's own slot,
    // then probe (lin2, true); if the probe's slot happens to differ the check is vacuous, so
    // probe (lin2, false) too to pin the packing round-trip.
    let lin2 = 0x0020_0000u32;
    cache.record_poll_negative(lin2, false);
    assert!(cache.poll_negative_live(lin2, false));
    assert!(!cache.poll_negative_live(lin2, true));
    // Flip only the packed d bit in that same slot: the probe for (lin2, false) must now miss,
    // pinning that the packed d gates the hit, not just the slot.
    let slot = DecodeCache::poll_neg_slot(lin2, false);
    cache.poll_neg[slot] ^= 1u64 << 32;
    assert!(!cache.poll_negative_live(lin2, false));
}

/// IZARRAVM_POLL_SKIP_NEG_CACHE policy is default-on with a kill switch: the cache runs unless
/// the env var is explicitly "0" or "" (unset, i.e. `None`, means ON). The machine crate's
/// `poll_skip_requested` now shares this exact truth table (`None` also means ON there); this
/// test pins the neg-cache policy's table on its own so a future change to either policy cannot
/// silently drift from the other.
#[cfg(feature = "jit")]
#[test]
fn poll_neg_cache_policy_default_on_with_kill_switch() {
    assert!(poll_neg_cache_policy(None));
    assert!(poll_neg_cache_policy(Some("1")));
    assert!(poll_neg_cache_policy(Some("yes")));
    assert!(!poll_neg_cache_policy(Some("0")));
    assert!(!poll_neg_cache_policy(Some("")));
}

const MEMORY_POLL_CELL: u32 = 0x4000;

/// `CMP EAX,DS:[disp32]; Jcc rel8` back to entry: the certified M1 shape, the
/// exact form the 2026-07-17 runtime probe confirmed at Doom's maketic loop
/// (0x473849/0x47384F). `jz` selects the branch sense (0x74/JE spins-while-
/// equal, 0x75/JNE spins-while-not-equal); the real loop uses 0x75.
fn memory_poll_code(cell_disp: u32, jz: bool) -> Vec<u8> {
    let mut code = vec![0x3b, 0x05];
    code.extend_from_slice(&cell_disp.to_le_bytes());
    code.push(if jz { 0x74 } else { 0x75 });
    code.push(0xf8); // rel8 -8: CMP (6 bytes) + Jcc (2 bytes) = 8, back to entry.
    code
}

fn memory_poll_starts() -> &'static [u32] {
    &[0, 6]
}

fn warm_exact_memory_poll(
    jz: bool,
    cell_disp: u32,
    eax: u32,
    cell_value: u32,
) -> (CpuGsw, TestBus) {
    let code = memory_poll_code(cell_disp, jz);
    let mut memory = vec![0xf4; 0x6000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[cell_disp as usize..cell_disp as usize + 4].copy_from_slice(&cell_value.to_le_bytes());
    let mut cpu = flat_cpu(GswMode::Gsw586);
    cpu.set_native_backend_enabled(false);
    cpu.registers.set_eax(eax);
    let mut bus = TestBus::with_memory(memory);
    bus.lazy_io_reads = true;
    for offset in memory_poll_starts() {
        cpu.set_eip(ENTRY + offset);
        cpu.fetch_decoded(&mut bus, ENTRY + offset)
            .expect("memory poll instruction decode");
    }
    cpu.set_eip(ENTRY);
    (cpu, bus)
}

/// Structural recognition (test 1) and the prefilter completeness tripwire
/// (test 2): every slot start classifies `Found` in every phase, for both
/// branch senses. If 0x3B were missing from `poll_head_possible`'s set, the
/// offset-0 (CMP) phase would be rejected by the prefilter and this test
/// would fail exactly like the io shapes' equivalent coverage test does.
#[cfg(feature = "jit")]
#[test]
fn exact_memory_poll_shape_covers_senses_and_every_phase() {
    for jz in [false, true] {
        for cell_value in [0x0000_0011u32, 0x0000_0099u32] {
            let eax = 0x0000_0099u32;
            let (mut cpu, _) = warm_exact_memory_poll(jz, MEMORY_POLL_CELL, eax, cell_value);
            let poll = cpu.poll_loop().expect("exact memory poll");
            assert_eq!(poll.family(), PollFamily::Memory);
            assert_eq!(poll.fetch_count(), 2);
            assert_eq!(poll.memory_cell_linear(), Some(MEMORY_POLL_CELL));
            assert_eq!(poll.memory_cell_width(), Some(4));
            assert_eq!(poll.memory_comparand(&cpu), Some(eax));
            let equal = cell_value == eax;
            let expected_spin = if jz { equal } else { !equal };
            assert_eq!(
                poll.memory_spin_predicate(cell_value, eax),
                Some(expected_spin)
            );
            for offset in memory_poll_starts() {
                cpu.set_eip(ENTRY + offset);
                let phase = cpu.poll_loop().expect("certified phase membership");
                assert_eq!(phase.at_head(), *offset == 0, "offset={offset}");
            }
        }
    }
}

/// R6a: a certified memory-poll loop entered with the comparand already equal
/// to the cell (JNE: about to exit) must report `memory_spin_predicate` false
/// in both senses, so the executor does not commit a phantom skip.
#[cfg(feature = "jit")]
#[test]
fn memory_poll_spin_predicate_false_at_exit_both_senses() {
    for jz in [false, true] {
        // JNE (jz=false) spins while NOT equal, so equal values mean "about to
        // exit" -> predicate must be false. JE (jz=true) spins while equal, so
        // equal values mean "still spinning" -> predicate must be true; use
        // unequal values there to hit its own exit case instead.
        let (cell_value, eax, expect_spin) = if jz { (5, 9, false) } else { (7, 7, false) };
        let (mut cpu, _) = warm_exact_memory_poll(jz, MEMORY_POLL_CELL, eax, cell_value);
        let poll = cpu.poll_loop().expect("exact memory poll");
        assert_eq!(
            poll.memory_spin_predicate(cell_value, eax),
            Some(expect_spin),
            "jz={jz}"
        );
    }
}

/// Structural/register-dependent rejects for the memory shape, mirroring
/// `exact_setup_poll_rejects_mismatch_cold_prefix_mode_targets_page_cs_and_smc`.
#[cfg(feature = "jit")]
#[test]
fn exact_memory_poll_rejects_non_bare_disp32_and_narrow_cs_limit() {
    // A base register (`[eax+disp32]`, ModRM mod=10 rm=0 with a disp32) is a
    // different addressing form (base present); structurally not the
    // certified bare-disp32 shape.
    let based = vec![0x3b, 0x80, 0x00, 0x40, 0x00, 0x00, 0x75, 0xf8];
    let (mut cpu, _) = warm_exact_poll(&based, ENTRY, &[0, 6], 0, 0, 0);
    assert!(cpu.poll_loop().is_none(), "base register must be rejected");

    // A 16-bit operand-size CMP (0x66 prefix) is prefixed, already rejected by
    // build_block's unprefixed requirement, exercised here for the shape.
    let word_form = vec![0x66, 0x3b, 0x05, 0x00, 0x40, 0x00, 0x00, 0x75, 0xf7];
    let (mut cpu, _) = warm_exact_poll(&word_form, ENTRY, &[0, 7], 0, 0, 0);
    assert!(cpu.poll_loop().is_none(), "16-bit CMP must be rejected");

    // Live CS limit shrunk below the loop's end: a register/segment-dependent
    // (NegativeVolatile) rejection, not a structural one.
    let (mut cpu, _) = warm_exact_memory_poll(false, MEMORY_POLL_CELL, 1, 2);
    assert!(cpu.poll_loop().is_some(), "sanity: unshrunk CS certifies");
    let mut cs = cpu.registers.cs();
    cs.limit = ENTRY + 6;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    assert!(
        cpu.poll_loop().is_none(),
        "narrow CS limit must reject the memory shape too"
    );
}

/// R2: `probe_linear_read_physical` is TLB-hit-only and non-mutating. Unpaged
/// mode returns the linear identity; paged mode requires an already-warm TLB
/// entry (never walks) and declines on a miss or a user-mode protection
/// mismatch, without ever touching CR2.
#[cfg(feature = "jit")]
#[test]
fn probe_linear_read_physical_is_tlb_hit_only_and_pure() {
    let cpu = flat_cpu(GswMode::Gsw586);
    assert_eq!(cpu.control.cr2, 0);
    assert_eq!(cpu.probe_linear_read_physical(0x1234), Some(0x1234));

    let mut cpu = paged_cpu(GswMode::Gsw586);
    // No warm TLB entry at this linear page yet: decline, zero perturbation.
    let cr2_before = cpu.control.cr2;
    assert_eq!(cpu.probe_linear_read_physical(0x0000_7000), None);
    assert_eq!(cpu.control.cr2, cr2_before, "a decline must not set CR2");

    // Warm the TLB by hand for a user-accessible page (linear page 5) and
    // confirm the probe then serves it without walking (no bus needed: cr3
    // is never consulted once the TLB entry is present).
    cpu.tlb.insert(5, 0x0000_9000, true, true, true);
    assert_eq!(
        cpu.probe_linear_read_physical(0x0000_5abc),
        Some(0x0000_9abc)
    );

    // A supervisor-only page (linear page 6) declines for a CPL-3 accessor.
    cpu.cpl = 3;
    cpu.tlb.insert(6, 0x0000_a000, true, false, true);
    assert_eq!(cpu.probe_linear_read_physical(0x0000_6000), None);
    assert_eq!(cpu.control.cr2, cr2_before, "a decline must not set CR2");
}
