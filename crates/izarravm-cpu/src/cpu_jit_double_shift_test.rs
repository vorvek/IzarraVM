// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const ENTRY: u32 = 0x501;
const RAM_TARGET: u32 = 0x3000;
const MODE13_TARGET: u32 = 0x000a_1000;
const DEST: u32 = 0x8123_4567;
const SOURCE: u32 = 0x89ab_cdef;

#[derive(Clone, Copy, Debug)]
enum CountSource {
    Immediate,
    Cl,
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

fn double_shift_instruction(
    left: bool,
    count_source: CountSource,
    count: u8,
    target: Option<u32>,
) -> Vec<u8> {
    let second = match (left, count_source) {
        (true, CountSource::Immediate) => 0xa4,
        (true, CountSource::Cl) => 0xa5,
        (false, CountSource::Immediate) => 0xac,
        (false, CountSource::Cl) => 0xad,
    };
    let mut instruction = vec![0x0f, second];
    if let Some(target) = target {
        instruction.push(0x15); // source EDX, destination dword [disp32]
        instruction.extend_from_slice(&target.to_le_bytes());
    } else {
        instruction.push(0xd0); // source EDX, destination EAX
    }
    if matches!(count_source, CountSource::Immediate) {
        instruction.push(count);
    }
    instruction
}

fn flat_cpu(mode: GswMode, entry: u32) -> CpuGsw {
    let mut cpu = CpuGsw::default();
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
    cpu.set_eip(entry);
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

fn map_direct_page(
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
    let write = bus
        .direct_page(physical, BusAccessKind::DataWrite)
        .unwrap()
        .unwrap();
    assert!(
        cpu.jit_fast_map
            .populate_write(linear, physical, write, permissions)
    );
}

fn install_block(cpu: &mut CpuGsw, linear: u32) -> jit::direct::CompiledBlock {
    let key = jit::direct::key_for(cpu, linear, true).unwrap();
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = jit::direct::compile(cpu, linear, true).expect("double shift compiles");
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("double shift installs");
    cpu.jit_direct.block(id).unwrap()
}

fn arm(cpu: &mut CpuGsw, count: u8) {
    cpu.halted = false;
    cpu.interrupt_shadow = false;
    cpu.registers.gpr.fill(0);
    cpu.registers.set_eax(DEST);
    cpu.registers.set_ecx(u32::from(count));
    cpu.registers.set_edx(SOURCE);
    cpu.registers.set_ebx(0x55aa_33cc);
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
    left: bool,
    count_source: CountSource,
    count: u8,
    target: Option<u32>,
) -> Fixture {
    let instruction = double_shift_instruction(left, count_source, count, target);
    let memory_len = target
        .map(|target| target as usize + 0x2000)
        .unwrap_or(0x5000)
        .max(0x5000);
    let mut pristine = vec![0; memory_len];
    pristine[(ENTRY - 1) as usize] = 0x90;
    let mut code = instruction.clone();
    code.extend_from_slice(&[0x89, 0xf6, 0x89, 0xff, 0xf4]);
    pristine[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    if let Some(target) = target {
        pristine[target as usize..target as usize + 4].copy_from_slice(&DEST.to_le_bytes());
    }

    let mut native = flat_cpu(mode, ENTRY);
    let mut interpreter = flat_cpu(mode, ENTRY);
    let mut native_bus = TestBus::with_memory(pristine.clone());
    let mut interpreter_bus = TestBus::with_memory(pristine);
    for bus in [&mut native_bus, &mut interpreter_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [
        ENTRY,
        ENTRY + instruction.len() as u32,
        ENTRY + instruction.len() as u32 + 2,
    ];
    decode_fixture(&mut native, &mut native_bus, &starts);
    decode_fixture(&mut interpreter, &mut interpreter_bus, &starts);
    if let Some(target) = target {
        map_direct_page(
            &mut native,
            &mut native_bus,
            target,
            target,
            jit::fast_map::PagePermissions::UNPAGED,
        );
    }
    let block = install_block(&mut native, ENTRY);
    arm(&mut native, count);
    arm(&mut interpreter, count);
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
        fixture.native_bus.memory, fixture.interpreter_bus.memory,
        "memory differs: {context}"
    );
    assert_eq!(
        fixture.native_bus.trace.elapsed_clocks(),
        fixture.interpreter_bus.trace.elapsed_clocks(),
        "bus timing differs: {context}"
    );
    assert_eq!(
        fixture.native_bus.mode13_dirty_pages, fixture.interpreter_bus.mode13_dirty_pages,
        "Mode13 dirty pages differ: {context}"
    );
    assert_eq!(
        fixture.native_bus.mode13_dword_writes, fixture.interpreter_bus.mode13_dword_writes,
        "Mode13 writes differ: {context}"
    );
    assert_eq!(fixture.native.perf_counters().jit_direct_insns - retired, 3);
    fixture
}

#[test]
fn register_and_memory_double_shifts_match_in_486_and_586_for_every_count_class() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for left in [true, false] {
            for count_source in [CountSource::Immediate, CountSource::Cl] {
                for count in [0, 1, 31, 32, 33] {
                    for target in [None, Some(RAM_TARGET)] {
                        let context = format!(
                            "mode={mode:?} left={left} count_source={count_source:?} count={count} memory={}",
                            target.is_some()
                        );
                        finish_and_compare(
                            prepare_flat(mode, left, count_source, count, target),
                            &context,
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn cl_count_is_captured_before_ecx_destination_changes() {
    let instruction = [0x0f, 0xa5, 0xc9]; // shld ecx,ecx,cl
    let mut memory = vec![0; 0x5000];
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + 8].copy_from_slice(&[
        instruction[0],
        instruction[1],
        instruction[2],
        0x89,
        0xf6,
        0x89,
        0xff,
        0xf4,
    ]);
    let mut native = flat_cpu(GswMode::Gsw586, ENTRY);
    let mut interpreter = flat_cpu(GswMode::Gsw586, ENTRY);
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interpreter_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interpreter_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [ENTRY, ENTRY + 3, ENTRY + 5];
    decode_fixture(&mut native, &mut native_bus, &starts);
    decode_fixture(&mut interpreter, &mut interpreter_bus, &starts);
    let block = install_block(&mut native, ENTRY);
    arm(&mut native, 1);
    arm(&mut interpreter, 1);
    finish_and_compare(
        Fixture {
            native,
            interpreter,
            native_bus,
            interpreter_bus,
            block,
        },
        "SHLD ECX,ECX,CL",
    );
}

#[test]
fn mode13_double_shift_accounts_read_write_and_dirty_page() {
    for (left, count_source) in [(true, CountSource::Immediate), (false, CountSource::Cl)] {
        let fixture = finish_and_compare(
            prepare_flat(GswMode::Gsw586, left, count_source, 1, Some(MODE13_TARGET)),
            "Mode13 double shift",
        );
        assert_eq!(fixture.native_bus.mode13_dword_writes, 1);
        assert_eq!(fixture.native_bus.mode13_dirty_pages, 1 << 1);
    }
}

#[test]
fn watched_double_shift_writes_exit_transactionally() {
    for count in [0, 1] {
        let mut fixture = prepare_flat(
            GswMode::Gsw586,
            true,
            CountSource::Immediate,
            count,
            Some(RAM_TARGET),
        );
        fixture.native.decode_cache.mark_code_range(RAM_TARGET, 4);
        let registers = fixture.native.registers.clone();
        let pending = fixture.native.pending_flags;
        let memory = fixture.native_bus.memory.clone();
        let exits = fixture.native.perf_counters().jit_direct_exit_code_watch;

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
            fixture.native.perf_counters().jit_direct_exit_code_watch - exits,
            1
        );
        for _ in 0..3 {
            fixture.native.cycle(&mut fixture.native_bus).unwrap();
        }
        for _ in 0..3 {
            fixture
                .interpreter
                .cycle(&mut fixture.interpreter_bus)
                .unwrap();
        }
        assert_eq!(fixture.native.registers, fixture.interpreter.registers);
        assert_eq!(
            fixture.native.pending_flags,
            fixture.interpreter.pending_flags
        );
        assert_eq!(fixture.native.eflags(), fixture.interpreter.eflags());
        assert_eq!(fixture.native_bus.memory, fixture.interpreter_bus.memory);
    }
}

#[test]
fn repeated_memory_double_shift_root_splits_below_one_host_page() {
    const COUNT: usize = 32;
    let instruction = double_shift_instruction(true, CountSource::Immediate, 1, Some(RAM_TARGET));
    let mut memory = vec![0; 0x5000];
    memory[(ENTRY - 1) as usize] = 0x90;
    let mut starts = Vec::with_capacity(COUNT);
    let mut cursor = ENTRY as usize;
    for _ in 0..COUNT {
        starts.push(cursor as u32);
        memory[cursor..cursor + instruction.len()].copy_from_slice(&instruction);
        cursor += instruction.len();
    }
    memory[cursor] = 0xf4;
    memory[RAM_TARGET as usize..RAM_TARGET as usize + 4].copy_from_slice(&DEST.to_le_bytes());

    let mut cpu = flat_cpu(GswMode::Gsw586, ENTRY);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    decode_fixture(&mut cpu, &mut bus, &starts);
    map_direct_page(
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

fn paged_cpu(mode: GswMode) -> CpuGsw {
    let mut cpu = flat_cpu(mode, ENTRY);
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

#[test]
fn paging_permission_alignment_and_cross_page_exits_are_transactional() {
    for (target, user_page, cross_or_unaligned, permission) in [
        (0x8001u32, true, true, false),
        (0x8fffu32, true, true, false),
        (0x8000u32, false, false, true),
    ] {
        let instruction = double_shift_instruction(false, CountSource::Immediate, 1, Some(target));
        let mut pristine = vec![0; 0xa000];
        pristine[(ENTRY - 1) as usize] = 0x90;
        let mut code = instruction.clone();
        code.extend_from_slice(&[0x89, 0xf6, 0x89, 0xff, 0xf4]);
        pristine[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
        pristine[0x3000..0x3004].copy_from_slice(&0x4007u32.to_le_bytes());
        pristine[0x4000..0x4004].copy_from_slice(&0x0007u32.to_le_bytes());
        pristine[0x4020..0x4024]
            .copy_from_slice(&(if user_page { 0x8007u32 } else { 0x8003u32 }).to_le_bytes());
        pristine[0x8ffc..0x9000].copy_from_slice(&DEST.to_le_bytes());

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
            ENTRY + instruction.len() as u32,
            ENTRY + instruction.len() as u32 + 2,
        ];
        decode_fixture(&mut native, &mut native_bus, &starts);
        decode_fixture(&mut interpreter, &mut interpreter_bus, &starts);
        let page = target & !0x0fff;
        map_direct_page(
            &mut native,
            &mut native_bus,
            page,
            page,
            jit::fast_map::PagePermissions {
                writable: true,
                user: user_page,
            },
        );
        let block = install_block(&mut native, ENTRY);
        arm(&mut native, 1);
        arm(&mut interpreter, 1);
        let registers = native.registers.clone();
        let pending = native.pending_flags;
        let memory = native_bus.memory.clone();
        let cross_exits = native
            .perf_counters()
            .jit_direct_exit_cross_page_or_alignment;
        let permission_exits = native.perf_counters().jit_direct_exit_permission;

        assert!(
            native
                .try_run_direct_block_for_test(&mut native_bus, block)
                .unwrap()
        );
        assert_eq!(native.registers, registers);
        assert_eq!(native.pending_flags, pending);
        assert_eq!(native_bus.memory, memory);
        assert_eq!(
            native
                .perf_counters()
                .jit_direct_exit_cross_page_or_alignment
                - cross_exits,
            u64::from(cross_or_unaligned)
        );
        assert_eq!(
            native.perf_counters().jit_direct_exit_permission - permission_exits,
            u64::from(permission)
        );

        // The test seeded the native fast-map entry directly, bypassing the page walk that sets
        // architectural A/D bits. Remove that synthetic entry before the precise fallback so both
        // CPUs take the same real page-walker path.
        native.jit_fast_map.invalidate_page(target);
        let native_decoded = native.decode_cache.get(ENTRY, true).unwrap();
        let interpreter_decoded = interpreter.decode_cache.get(ENTRY, true).unwrap();
        let native_result = native.execute_decoded(&native_decoded, &mut native_bus);
        let interpreter_result =
            interpreter.execute_decoded(&interpreter_decoded, &mut interpreter_bus);
        assert_eq!(
            result_signature(native_result),
            result_signature(interpreter_result),
            "target={target:#x}"
        );
        assert_eq!(native.registers, interpreter.registers);
        assert_eq!(native.pending_flags, interpreter.pending_flags);
        assert_eq!(native.eflags(), interpreter.eflags());
        assert_eq!(native.control.cr2, interpreter.control.cr2);
        assert_eq!(native_bus.memory, interpreter_bus.memory);
    }
}

#[test]
fn nonflat_segment_falls_back_before_the_precise_limit_fault() {
    let mut fixture = prepare_flat(GswMode::Gsw486, true, CountSource::Cl, 1, Some(RAM_TARGET));
    for cpu in [&mut fixture.native, &mut fixture.interpreter] {
        let mut ds = cpu.registers.segment(SegmentIndex::Ds);
        ds.limit = RAM_TARGET - 1;
        cpu.registers.set_segment(SegmentIndex::Ds, ds);
    }
    let registers = fixture.native.registers.clone();
    let pending = fixture.native.pending_flags;
    let memory = fixture.native_bus.memory.clone();
    assert!(
        !fixture
            .native
            .try_run_direct_block_for_test(&mut fixture.native_bus, fixture.block)
            .unwrap()
    );
    assert_eq!(fixture.native.registers, registers);
    assert_eq!(fixture.native.pending_flags, pending);
    assert_eq!(fixture.native_bus.memory, memory);

    let native_decoded = fixture.native.decode_cache.get(ENTRY, true).unwrap();
    let interpreter_decoded = fixture.interpreter.decode_cache.get(ENTRY, true).unwrap();
    let native_result = fixture
        .native
        .execute_decoded(&native_decoded, &mut fixture.native_bus);
    let interpreter_result = fixture
        .interpreter
        .execute_decoded(&interpreter_decoded, &mut fixture.interpreter_bus);
    let native_result = result_signature(native_result);
    let interpreter_result = result_signature(interpreter_result);
    assert_eq!(native_result, interpreter_result);
    assert_eq!(native_result, Err((13, Some(0))));
    assert_eq!(fixture.native.registers, fixture.interpreter.registers);
    assert_eq!(
        fixture.native.pending_flags,
        fixture.interpreter.pending_flags
    );
    assert_eq!(fixture.native_bus.memory, fixture.interpreter_bus.memory);
}
