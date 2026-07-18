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
