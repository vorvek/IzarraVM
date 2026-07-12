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
    map_read_page(
        &mut fixture.interpreter,
        &mut fixture.interpreter_bus,
        0x8000,
        0x8000,
        permissions,
    );
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

#[test]
fn group3_non_test_subops_remain_interpreter_only() {
    for code in [[0xf6, 0xd3], [0xf7, 0xdb]] {
        let mut memory = vec![0; 0x1000];
        memory[(ENTRY - 1) as usize] = 0x90;
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
        let mut cpu = flat_cpu(GswMode::Gsw586);
        let mut bus = TestBus::with_memory(memory);
        bus.direct_pages_enabled = true;
        decode_fixture(&mut cpu, &mut bus, &[ENTRY]);
        assert!(jit::direct::compile(&mut cpu, ENTRY, true).is_none());
    }
}
