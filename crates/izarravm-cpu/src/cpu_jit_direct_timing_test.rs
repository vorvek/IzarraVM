// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn mode13_read_self_loop_respects_the_tight_native_deadline() {
    const ENTRY: u32 = 0x101;
    const MODE13: u32 = 0x000a_0000;

    let mut memory = vec![0; 0x000b_0000];
    memory[ENTRY as usize..ENTRY as usize + 11].copy_from_slice(&[
        0xa0, 0x00, 0x00, 0x0a, 0x00, // mov al,[0xa0000]
        0x83, 0xe9, 0x01, // sub ecx,1
        0x75, 0xf6, // jnz ENTRY
        0xf4, // hlt
    ]);
    memory[MODE13 as usize] = 0x5a;

    let mut native = fresh();
    let mut interp = fresh();
    make_data_segments_flat(&mut native);
    make_data_segments_flat(&mut interp);
    native.registers.eip = ENTRY;
    interp.registers.eip = ENTRY;
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        bus.report_batch_clocks = true;
        bus.uniform_native_fetches = true;
    }
    let starts = [ENTRY, ENTRY + 5, ENTRY + 8];
    decode_fixture(&mut native, &mut native_bus, &starts);
    decode_fixture(&mut interp, &mut interp_bus, &starts);
    for (cpu, bus) in [
        (&mut native, &mut native_bus),
        (&mut interp, &mut interp_bus),
    ] {
        map_direct_page(
            cpu,
            bus,
            MODE13,
            MODE13,
            jit::fast_map::PagePermissions::UNPAGED,
            true,
            false,
        );
    }
    let block = install_fixture_block(&mut native, ENTRY);
    assert!(block.is_self_loop());
    assert_eq!(block.byte_reads(), 1);
    assert_eq!(block.word_reads(), 0);
    assert_eq!(block.dword_reads(), 0);

    let (num, den) = level_timing(native.persona());
    let fp_core_upper = u64::from(block.weighted_fp_clocks())
        .saturating_add(u64::from(FP_TIMING_DEN) - 1)
        / u64::from(FP_TIMING_DEN);
    let scaled_core_upper = u64::from(block.raw_clocks())
        .saturating_add(fp_core_upper)
        .saturating_mul(u64::from(num))
        .saturating_add(u64::from(den) - 1)
        / u64::from(den);
    let ram_read_upper = native_bus.jit_data_cost_clocks(BusWidth::Byte);
    let mode13_read_upper = native_bus.jit_mode13_data_cost_clocks(BusWidth::Byte);
    assert!(mode13_read_upper > ram_read_upper);
    let fetch_upper = native_bus
        .jit_fetch_cost_clocks()
        .saturating_mul(u64::from(block.span().instructions));
    let ram_only_iteration_upper = scaled_core_upper.saturating_add(
        native_bus.jit_scale_bus_cost_upper(fetch_upper.saturating_add(ram_read_upper)),
    );
    let iteration_upper = scaled_core_upper.saturating_add(
        native_bus.jit_scale_bus_cost_upper(fetch_upper.saturating_add(mode13_read_upper)),
    );
    assert!(iteration_upper > ram_only_iteration_upper);

    for cpu in [&mut native, &mut interp] {
        cpu.halted = false;
        cpu.interrupt_shadow = false;
        cpu.registers.gpr.fill(0);
        cpu.registers.set_ecx(2);
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        cpu.registers.eip = ENTRY;
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    native_bus.trace = BusTrace::default();
    interp_bus.trace = BusTrace::default();
    let start_registers = native.registers.clone();
    let start_pending = native.pending_flags;
    let zero_budget_rejects = native.perf_counters().jit_direct_reject_zero_budget;

    assert!(
        !native
            .try_run_direct_block_with_cap_for_test(&mut native_bus, block, iteration_upper)
            .unwrap()
    );
    assert_eq!(native.registers, start_registers);
    assert_eq!(native.pending_flags, start_pending);
    assert_eq!(native.elapsed_clocks, 0);
    assert_eq!(native.timing_rem, 0);
    assert_eq!(native_bus.trace.elapsed_clocks(), 0);
    assert_eq!(
        native.perf_counters().jit_direct_reject_zero_budget - zero_budget_rejects,
        1
    );

    let loads = native.perf_counters().jit_native_load_hits;
    assert!(
        native
            .try_run_direct_block_with_cap_for_test(&mut native_bus, block, iteration_upper + 1,)
            .unwrap()
    );
    for _ in 0..3 {
        interp.cycle(&mut interp_bus).unwrap();
    }

    assert_eq!(native.registers, interp.registers);
    assert_eq!(native.pending_flags, interp.pending_flags);
    assert_eq!(native.registers.eip, ENTRY);
    assert_eq!(native.registers.ecx(), 1);
    assert_eq!(native.registers.eax() & 0xff, 0x5a);
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
    assert_eq!(native.timing_rem, interp.timing_rem);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert!(
        native
            .elapsed_clocks
            .saturating_add(native_bus.trace.elapsed_clocks())
            < iteration_upper + 1
    );
    assert_eq!(native.perf_counters().jit_native_load_hits - loads, 1);
}

struct DirectTimingCase {
    name: &'static str,
    opcode: &'static [u8],
    expected_raw_clocks: u32,
    terminal: bool,
    eflags: u32,
}

fn direct_timing_cases() -> Vec<DirectTimingCase> {
    vec![
        DirectTimingCase {
            name: "mov register",
            opcode: &[0x89, 0xc8],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "mov byte register",
            opcode: &[0x88, 0xcc],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "mov immediate",
            opcode: &[0xb8, 0x78, 0x56, 0x34, 0x12],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "mov byte immediate",
            opcode: &[0xb4, 0x7f],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "lea",
            opcode: &[0x8d, 0x44, 0x8b, 0x10],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "inc register",
            opcode: &[0x40],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x203,
        },
        DirectTimingCase {
            name: "alu register",
            opcode: &[0x01, 0xc8],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "alu immediate",
            opcode: &[0x83, 0xc0, 0x01],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "alu byte immediate",
            opcode: &[0x80, 0xc4, 0x01],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "alu memory source",
            opcode: &[0x03, 0x05, 0x00, 0x30, 0x00, 0x00],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "alu dword memory destination",
            opcode: &[0x01, 0x0d, 0x00, 0x30, 0x00, 0x00],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "alu byte memory destination",
            opcode: &[0x80, 0x05, 0x00, 0x30, 0x00, 0x00, 0x01],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "compare memory destination",
            opcode: &[0x83, 0x3d, 0x00, 0x30, 0x00, 0x00, 0x01],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "test register",
            opcode: &[0x85, 0xc0],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x203,
        },
        DirectTimingCase {
            name: "shift register",
            opcode: &[0xc1, 0xe8, 0x01],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "load byte",
            opcode: &[0x8a, 0x05, 0x00, 0x30, 0x00, 0x00],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "load dword",
            opcode: &[0x8b, 0x05, 0x00, 0x30, 0x00, 0x00],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "store byte",
            opcode: &[0x88, 0x0d, 0x00, 0x30, 0x00, 0x00],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "store dword",
            opcode: &[0x89, 0x0d, 0x00, 0x30, 0x00, 0x00],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "moffs load byte",
            opcode: &[0xa0, 0x00, 0x30, 0x00, 0x00],
            expected_raw_clocks: 8,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "moffs load dword",
            opcode: &[0xa1, 0x00, 0x30, 0x00, 0x00],
            expected_raw_clocks: 8,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "moffs store byte",
            opcode: &[0xa2, 0x00, 0x30, 0x00, 0x00],
            expected_raw_clocks: 8,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "moffs store dword",
            opcode: &[0xa3, 0x00, 0x30, 0x00, 0x00],
            expected_raw_clocks: 8,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "store byte immediate",
            opcode: &[0xc6, 0x05, 0x00, 0x30, 0x00, 0x00, 0x7f],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "store dword immediate",
            opcode: &[0xc7, 0x05, 0x00, 0x30, 0x00, 0x00, 0x78, 0x56, 0x34, 0x12],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "memory inc",
            opcode: &[0xff, 0x05, 0x00, 0x30, 0x00, 0x00],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x203,
        },
        DirectTimingCase {
            name: "push register",
            opcode: &[0x50],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "push immediate",
            opcode: &[0x68, 0x78, 0x56, 0x34, 0x12],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "pop register",
            opcode: &[0x58],
            expected_raw_clocks: 8,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "call relative",
            opcode: &[0xe8, 0x20, 0x00, 0x00, 0x00],
            expected_raw_clocks: 7,
            terminal: true,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "jump near",
            opcode: &[0xe9, 0x20, 0x00, 0x00, 0x00],
            expected_raw_clocks: 7,
            terminal: true,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "jump short",
            opcode: &[0xeb, 0x20],
            expected_raw_clocks: 7,
            terminal: true,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "return",
            opcode: &[0xc3],
            expected_raw_clocks: 10,
            terminal: true,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "return and release",
            opcode: &[0xc2, 0x08, 0x00],
            expected_raw_clocks: 10,
            terminal: true,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "short jcc fallthrough",
            opcode: &[0x74, 0x20],
            expected_raw_clocks: 3,
            terminal: true,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "short jcc taken",
            opcode: &[0x74, 0x20],
            expected_raw_clocks: 3,
            terminal: true,
            eflags: 0x242,
        },
        DirectTimingCase {
            name: "near jcc fallthrough",
            opcode: &[0x0f, 0x85, 0x20, 0x00, 0x00, 0x00],
            expected_raw_clocks: 3,
            terminal: true,
            eflags: 0x242,
        },
        DirectTimingCase {
            name: "near jcc taken",
            opcode: &[0x0f, 0x85, 0x20, 0x00, 0x00, 0x00],
            expected_raw_clocks: 3,
            terminal: true,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "x87 register",
            opcode: &[0xd8, 0xc0],
            expected_raw_clocks: 4,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "x87 memory load",
            opcode: &[0xd9, 0x05, 0x00, 0x30, 0x00, 0x00],
            expected_raw_clocks: 4,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "x87 memory store and pop",
            opcode: &[0xd9, 0x1d, 0x00, 0x30, 0x00, 0x00],
            expected_raw_clocks: 4,
            terminal: false,
            eflags: 0x202,
        },
    ]
}

fn run_direct_timing_case(mode: GswMode, uniform_fetches: bool, case: &DirectTimingCase) {
    const ENTRY: u32 = 0x101;
    const DATA: usize = 0x3000;
    const STACK: usize = 0x5000;

    let mut code = case.opcode.to_vec();
    let mut starts = vec![ENTRY];
    if !case.terminal {
        starts.push(ENTRY + code.len() as u32);
        code.extend_from_slice(&[0x89, 0xf6]);
        starts.push(ENTRY + code.len() as u32);
        code.extend_from_slice(&[0x89, 0xff]);
    }
    starts.push(ENTRY + code.len() as u32);
    code.extend_from_slice(&[0x66, 0x87, 0xc0]);
    let mut pristine = vec![0; 0x7000];
    pristine[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    pristine[DATA..DATA + 4].copy_from_slice(&2.5f32.to_bits().to_le_bytes());
    pristine[STACK..STACK + 4].copy_from_slice(&0x180u32.to_le_bytes());

    let mut direct = flat_stack_cpu(ENTRY);
    let mut interpreter = flat_stack_cpu(ENTRY);
    direct.set_mode(mode);
    interpreter.set_mode(mode);
    let mut direct_bus = TestBus::with_memory(pristine.clone());
    let mut interpreter_bus = TestBus::with_memory(pristine);
    for bus in [&mut direct_bus, &mut interpreter_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        bus.uniform_native_fetches = uniform_fetches;
    }
    decode_fixture(&mut direct, &mut direct_bus, &starts);
    decode_fixture(&mut interpreter, &mut interpreter_bus, &starts);
    for (cpu, bus) in [
        (&mut direct, &mut direct_bus),
        (&mut interpreter, &mut interpreter_bus),
    ] {
        for page in [0x3000, 0x4000, 0x5000] {
            map_direct_page(
                cpu,
                bus,
                page,
                page,
                jit::fast_map::PagePermissions::UNPAGED,
                true,
                true,
            );
        }
    }
    if case
        .opcode
        .first()
        .is_some_and(|opcode| (0xd8..=0xdf).contains(opcode))
    {
        direct.fpu.push(1.25);
    }
    let block = install_fixture_block(&mut direct, ENTRY);
    assert_eq!(
        block.raw_clocks(),
        case.expected_raw_clocks,
        "{} {mode:?} raw core table",
        case.name
    );
    assert_eq!(
        block.span().instructions,
        if case.terminal { 1 } else { 3 },
        "{} {mode:?} block shape",
        case.name
    );

    for cpu in [&mut direct, &mut interpreter] {
        cpu.halted = false;
        cpu.interrupt_shadow = false;
        cpu.registers.gpr = [
            0x1122_3344,
            3,
            0x5566_7788,
            0x3000,
            STACK as u32,
            0,
            0x40,
            0x80,
        ];
        cpu.registers.eflags = case.eflags;
        cpu.pending_flags = PendingFlags::default();
        cpu.fpu = X87::default();
        cpu.fpu.push(1.25);
        cpu.set_eip(ENTRY);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.fp_rem = 3;
        cpu.core_clocks_so_far = 0;
    }
    direct_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();

    assert!(
        direct
            .try_run_direct_block_for_test(&mut direct_bus, block)
            .unwrap(),
        "{} {mode:?} did not run directly",
        case.name
    );
    for _ in 0..block.span().instructions {
        interpreter.cycle(&mut interpreter_bus).unwrap();
    }

    assert_eq!(
        direct.elapsed_clocks, interpreter.elapsed_clocks,
        "{} {mode:?} scaled core clocks",
        case.name
    );
    assert_eq!(
        direct.timing_rem, interpreter.timing_rem,
        "{} {mode:?} core remainder",
        case.name
    );
    assert_eq!(
        direct.fp_rem, interpreter.fp_rem,
        "{} {mode:?} x87 remainder",
        case.name
    );
    assert_eq!(
        direct_bus.trace.elapsed_clocks(),
        interpreter_bus.trace.elapsed_clocks(),
        "{} {mode:?} bus clocks",
        case.name
    );
    assert_eq!(
        direct
            .elapsed_clocks
            .saturating_add(direct_bus.trace.elapsed_clocks()),
        interpreter
            .elapsed_clocks
            .saturating_add(interpreter_bus.trace.elapsed_clocks()),
        "{} {mode:?} combined clocks",
        case.name
    );
    assert_eq!(
        direct.registers, interpreter.registers,
        "{} {mode:?}",
        case.name
    );
    assert_eq!(
        direct.pending_flags, interpreter.pending_flags,
        "{} {mode:?} pending flags",
        case.name
    );
    assert_eq!(
        direct.eflags(),
        interpreter.eflags(),
        "{} {mode:?} EFLAGS",
        case.name
    );
    assert_eq!(
        direct.fpu, interpreter.fpu,
        "{} {mode:?} x87 state",
        case.name
    );
    assert_eq!(
        direct_bus.memory, interpreter_bus.memory,
        "{} {mode:?} memory",
        case.name
    );
}

#[test]
fn direct_family_core_and_bus_timing_matches_interpreter_in_486_and_586_modes() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for uniform_fetches in [false, true] {
            for case in direct_timing_cases() {
                run_direct_timing_case(mode, uniform_fetches, &case);
            }
        }
    }
}

const QUAKE_SEGMENT_BASE: u32 = 0x1000_0000;
const QUAKE_CS_LIMIT: u32 = 0x016e_ffff;

fn quake_segment_cpu(entry: u32, paging: bool) -> CpuGsw {
    let mut cpu = flat_stack_cpu(entry);
    if paging {
        cpu.control.cr0 |= CR0_PG | CR0_WP;
        cpu.control.cr3 = 0x3000;
    }
    cpu.cpl = 3;
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x00a7,
            base: QUAKE_SEGMENT_BASE,
            limit: QUAKE_CS_LIMIT,
            access: 0xfb,
            default_size_32: true,
        },
    );
    for segment in [SegmentIndex::Ds, SegmentIndex::Ss, SegmentIndex::Es] {
        cpu.registers.set_segment(
            segment,
            SegmentRegister {
                selector: 0x00af,
                base: QUAKE_SEGMENT_BASE,
                limit: u32::MAX,
                access: 0xf3,
                default_size_32: true,
            },
        );
    }
    for segment in [SegmentIndex::Fs, SegmentIndex::Gs] {
        cpu.registers.set_segment(
            segment,
            SegmentRegister {
                selector: 0x00cf,
                base: 0,
                limit: 0x00ff_ffff,
                access: 0xf3,
                default_size_32: true,
            },
        );
    }
    cpu.set_eip(entry);
    cpu
}

fn decode_segmented_fixture(cpu: &mut CpuGsw, bus: &mut TestBus, offsets: &[u32]) {
    let cs_base = cpu.registers.cs().base;
    for &offset in offsets {
        cpu.set_eip(offset);
        cpu.fetch_decoded(bus, cs_base.wrapping_add(offset))
            .unwrap();
    }
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
    bus.trace = BusTrace::default();
}

fn high_segment_page_tables(memory: &mut [u8]) {
    memory[0x3100..0x3104].copy_from_slice(&0x0000_4067u32.to_le_bytes());
    memory[0x4000..0x4004].copy_from_slice(&0x0000_8067u32.to_le_bytes());
}

#[test]
fn quake_descriptors_admit_a_finite_cs_register_loop_natively() {
    const ENTRY: u32 = 0x101;
    let mut memory = vec![0; 0xc000];
    high_segment_page_tables(&mut memory);
    memory[0x8101..0x8109].copy_from_slice(&[
        0x83, 0xc0, 0x01, // add eax,1
        0x83, 0xe9, 0x01, // sub ecx,1
        0x75, 0xf8, // jnz ENTRY
    ]);
    let mut native = quake_segment_cpu(ENTRY, true);
    let mut interp = quake_segment_cpu(ENTRY, true);
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [ENTRY, ENTRY + 3, ENTRY + 6];
    decode_segmented_fixture(&mut native, &mut native_bus, &starts);
    decode_segmented_fixture(&mut interp, &mut interp_bus, &starts);
    let block = install_fixture_block(&mut native, QUAKE_SEGMENT_BASE + ENTRY);
    for cpu in [&mut native, &mut interp] {
        cpu.registers.set_eax(5);
        cpu.registers.set_ecx(4);
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        cpu.set_eip(ENTRY);
    }
    let entries = native.perf_counters().jit_direct_entries;
    let retired = native.perf_counters().jit_direct_insns;

    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    for _ in 0..12 {
        interp.cycle(&mut interp_bus).unwrap();
    }

    assert_eq!(native.registers, interp.registers);
    assert_eq!(native.pending_flags, interp.pending_flags);
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
    assert_eq!(native.perf_counters().jit_direct_entries - entries, 1);
    assert_eq!(native.perf_counters().jit_direct_insns - retired, 12);
}

#[test]
fn paged_quake_ds_ss_bases_match_load_store_and_call() {
    const ENTRY: u32 = 0x201;
    const TARGET: u32 = 0x240;
    let mut memory = vec![0; 0xc000];
    high_segment_page_tables(&mut memory);
    memory[0x4004..0x4008].copy_from_slice(&0x0000_6067u32.to_le_bytes());
    memory[0x4008..0x400c].copy_from_slice(&0x0000_7067u32.to_le_bytes());
    memory[0x8201..0x8210].copy_from_slice(&[
        0xa1, 0x00, 0x10, 0x00, 0x00, // mov eax,[0x1000]
        0xa3, 0x04, 0x10, 0x00, 0x00, // mov [0x1004],eax
        0xe8, 0x30, 0x00, 0x00, 0x00, // call TARGET
    ]);
    memory[0x6000..0x6004].copy_from_slice(&0x7654_3210u32.to_le_bytes());
    let mut native = quake_segment_cpu(ENTRY, true);
    let mut interp = quake_segment_cpu(ENTRY, true);
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [ENTRY, ENTRY + 5, ENTRY + 10];
    decode_segmented_fixture(&mut native, &mut native_bus, &starts);
    decode_segmented_fixture(&mut interp, &mut interp_bus, &starts);
    let permissions = jit::fast_map::PagePermissions {
        writable: true,
        user: true,
    };
    map_direct_page(
        &mut native,
        &mut native_bus,
        QUAKE_SEGMENT_BASE + 0x1000,
        0x6000,
        permissions,
        true,
        true,
    );
    map_direct_page(
        &mut native,
        &mut native_bus,
        QUAKE_SEGMENT_BASE + 0x2000,
        0x7000,
        permissions,
        false,
        true,
    );
    let block = install_fixture_block(&mut native, QUAKE_SEGMENT_BASE + ENTRY);
    for cpu in [&mut native, &mut interp] {
        cpu.registers.set_esp(0x2004);
        cpu.registers.eflags = 0x246;
        cpu.pending_flags = PendingFlags::default();
        cpu.set_eip(ENTRY);
    }

    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    for _ in 0..3 {
        interp.cycle(&mut interp_bus).unwrap();
    }

    assert_eq!(native.registers, interp.registers);
    assert_eq!(native.pending_flags, interp.pending_flags);
    assert_eq!(native.registers.eip, TARGET);
    assert_eq!(native.registers.esp(), 0x2000);
    assert_eq!(
        &native_bus.memory[0x6000..0x6008],
        &interp_bus.memory[0x6000..0x6008]
    );
    assert_eq!(
        &native_bus.memory[0x7000..0x7004],
        &interp_bus.memory[0x7000..0x7004]
    );
    assert_eq!(
        u32::from_le_bytes(native_bus.memory[0x7000..0x7004].try_into().unwrap()),
        0x210
    );
}

#[test]
fn finite_cs_near_returns_run_directly_and_match_interpreter() {
    const ENTRY: u32 = 0x301;
    const RET: u32 = ENTRY + 7;
    const TARGET: u32 = 0x380;
    const INITIAL_ESP: u32 = 0x2000;

    for (return_bytes, release) in [(&[0xc3][..], 0u32), (&[0xc2, 0x08, 0x00][..], 8)] {
        let mut memory = vec![0; 0xc000];
        high_segment_page_tables(&mut memory);
        memory[0x4008..0x400c].copy_from_slice(&0x0000_7067u32.to_le_bytes());
        let mut code = vec![
            0xb8, 0x44, 0x33, 0x22, 0x11, // mov eax,0x11223344
            0x89, 0xc1, // mov ecx,eax
        ];
        code.extend_from_slice(return_bytes);
        memory[0x8301..0x8301 + code.len()].copy_from_slice(&code);
        memory[0x7000..0x7004].copy_from_slice(&TARGET.to_le_bytes());

        let mut native = quake_segment_cpu(ENTRY, true);
        let mut interp = quake_segment_cpu(ENTRY, true);
        let mut native_bus = TestBus::with_memory(memory.clone());
        let mut interp_bus = TestBus::with_memory(memory);
        for bus in [&mut native_bus, &mut interp_bus] {
            bus.direct_pages_enabled = true;
            bus.direct_page_clocks = true;
        }
        let starts = [ENTRY, ENTRY + 5, RET];
        decode_segmented_fixture(&mut native, &mut native_bus, &starts);
        decode_segmented_fixture(&mut interp, &mut interp_bus, &starts);
        for (cpu, bus) in [
            (&mut native, &mut native_bus),
            (&mut interp, &mut interp_bus),
        ] {
            map_direct_page(
                cpu,
                bus,
                QUAKE_SEGMENT_BASE + INITIAL_ESP,
                0x7000,
                jit::fast_map::PagePermissions {
                    writable: true,
                    user: true,
                },
                true,
                false,
            );
        }
        let block = install_fixture_block(&mut native, QUAKE_SEGMENT_BASE + ENTRY);
        assert_eq!(block.span().instructions, 3);
        arm_stack_fixture(&mut native, ENTRY, INITIAL_ESP);
        arm_stack_fixture(&mut interp, ENTRY, INITIAL_ESP);

        assert!(
            native
                .try_run_direct_block_for_test(&mut native_bus, block)
                .unwrap()
        );
        for _ in 0..3 {
            interp.cycle(&mut interp_bus).unwrap();
        }

        assert_eq!(native.registers, interp.registers);
        assert_eq!(native.pending_flags, interp.pending_flags);
        assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
        assert_eq!(
            native_bus.trace.elapsed_clocks(),
            interp_bus.trace.elapsed_clocks()
        );
        assert_eq!(native.registers.eip, TARGET);
        assert_eq!(native.registers.esp(), INITIAL_ESP + 4 + release);
    }
}

#[test]
fn finite_cs_ret_limit_exit_preserves_restart_state_and_faults_precisely() {
    for stack_physical in [0x7000, 0x000a_0000] {
        finite_cs_ret_limit_exit_case(stack_physical);
    }
}

fn finite_cs_ret_limit_exit_case(stack_physical: u32) {
    const ENTRY: u32 = 0x301;
    const RET: u32 = ENTRY + 7;
    const INITIAL_ESP: u32 = 0x2000;
    let mut memory = vec![0; 0x000b_0000];
    high_segment_page_tables(&mut memory);
    memory[0x4008..0x400c].copy_from_slice(&(stack_physical | 0x67).to_le_bytes());
    memory[0x8301..0x8309].copy_from_slice(&[
        0xb8, 0x44, 0x33, 0x22, 0x11, // mov eax,0x11223344
        0x89, 0xc1, // mov ecx,eax
        0xc3, // ret
    ]);
    let stack = stack_physical as usize;
    memory[stack..stack + 4].copy_from_slice(&(QUAKE_CS_LIMIT + 1).to_le_bytes());

    let mut native = quake_segment_cpu(ENTRY, true);
    let mut interp = quake_segment_cpu(ENTRY, true);
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [ENTRY, ENTRY + 5, RET];
    decode_segmented_fixture(&mut native, &mut native_bus, &starts);
    decode_segmented_fixture(&mut interp, &mut interp_bus, &starts);
    map_direct_page(
        &mut native,
        &mut native_bus,
        QUAKE_SEGMENT_BASE + INITIAL_ESP,
        stack_physical,
        jit::fast_map::PagePermissions {
            writable: true,
            user: true,
        },
        true,
        false,
    );
    let block = install_fixture_block(&mut native, QUAKE_SEGMENT_BASE + ENTRY);
    assert_eq!(block.span().instructions, 3);
    arm_stack_fixture(&mut native, ENTRY, INITIAL_ESP);
    arm_stack_fixture(&mut interp, ENTRY, INITIAL_ESP);
    let side_exits = native.perf_counters().jit_direct_side_exits;
    let other_exits = native.perf_counters().jit_direct_exit_other;

    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    interp.cycle(&mut interp_bus).unwrap();
    interp.cycle(&mut interp_bus).unwrap();
    assert_eq!(native.registers, interp.registers);
    assert_eq!(native.registers.eip, RET);
    assert_eq!(native.registers.esp(), INITIAL_ESP);
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert_eq!(native.perf_counters().jit_direct_side_exits - side_exits, 1);
    assert_eq!(
        native.perf_counters().jit_direct_exit_other - other_exits,
        1
    );

    let native_ret = native
        .decode_cache
        .get(QUAKE_SEGMENT_BASE + RET, true)
        .unwrap();
    let interp_ret = interp
        .decode_cache
        .get(QUAKE_SEGMENT_BASE + RET, true)
        .unwrap();
    let native_fault = native.execute_decoded(&native_ret, &mut native_bus);
    let interp_fault = interp.execute_decoded(&interp_ret, &mut interp_bus);
    for fault in [native_fault, interp_fault] {
        assert!(matches!(
            fault,
            Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(0)
            })
        ));
    }
    assert_eq!(native.registers, interp.registers);
    assert_eq!(native.registers.eip, RET);
    assert_eq!(native.registers.esp(), INITIAL_ESP);
    assert_eq!(native_bus.memory, interp_bus.memory);
}

#[test]
fn nonflat_segment_limit_and_permission_fallbacks_are_transactional() {
    const ENTRY: u32 = 0x201;
    const STORE: u32 = ENTRY + 9;
    const TARGET: usize = 0x11000;
    let mut pristine = vec![0; 0x13000];
    pristine[ENTRY as usize..ENTRY as usize + 14].copy_from_slice(&[
        0xb8, 0x44, 0x33, 0x22, 0x11, // mov eax,0x11223344
        0x89, 0xc1, // mov ecx,eax
        0x89, 0xca, // mov edx,ecx
        0x89, 0x06, // mov [esi],eax
        0x89, 0xc3, // mov ebx,eax
        0xf4,
    ]);

    for (limit, access, emitted_limit_guard) in [(0x1002, 0x93, true), (u32::MAX, 0x91, false)] {
        let make_cpu = || {
            let mut cpu = flat_stack_cpu(ENTRY);
            cpu.registers.set_segment(
                SegmentIndex::Ds,
                SegmentRegister {
                    selector: 0x10,
                    base: 0x10000,
                    limit,
                    access,
                    default_size_32: true,
                },
            );
            cpu.registers.set_esi(0x1000);
            cpu
        };
        let mut native = make_cpu();
        let mut interp = make_cpu();
        let mut native_bus = TestBus::with_memory(pristine.clone());
        let mut interp_bus = TestBus::with_memory(pristine.clone());
        for bus in [&mut native_bus, &mut interp_bus] {
            bus.direct_pages_enabled = true;
            bus.direct_page_clocks = true;
        }
        let starts = [ENTRY, ENTRY + 5, ENTRY + 7, STORE, STORE + 2];
        decode_fixture(&mut native, &mut native_bus, &starts);
        decode_fixture(&mut interp, &mut interp_bus, &starts);
        map_direct_page(
            &mut native,
            &mut native_bus,
            TARGET as u32,
            TARGET as u32,
            jit::fast_map::PagePermissions::UNPAGED,
            false,
            true,
        );
        let block = install_fixture_block(&mut native, ENTRY);
        for cpu in [&mut native, &mut interp] {
            cpu.registers.gpr.fill(0);
            cpu.registers.set_esi(0x1000);
            cpu.registers.eflags = 0x202;
            cpu.pending_flags = PendingFlags::default();
            cpu.set_eip(ENTRY);
            cpu.elapsed_clocks = 0;
            cpu.timing_rem = 0;
            cpu.core_clocks_so_far = 0;
        }
        let side_exits = native.perf_counters().jit_direct_side_exits;
        assert!(
            native
                .try_run_direct_block_for_test(&mut native_bus, block)
                .unwrap()
        );
        for _ in 0..3 {
            interp.cycle(&mut interp_bus).unwrap();
        }
        assert_eq!(native.registers, interp.registers);
        assert_eq!(native.registers.eip, STORE);
        assert_eq!(native.pending_flags, interp.pending_flags);
        assert_eq!(&native_bus.memory[TARGET..TARGET + 4], &[0; 4]);
        assert_eq!(
            native.perf_counters().jit_direct_side_exits - side_exits,
            u64::from(emitted_limit_guard)
        );

        let native_decoded = native.decode_cache.get(STORE, true).unwrap();
        let interp_decoded = interp.decode_cache.get(STORE, true).unwrap();
        let native_fault = native.execute_decoded(&native_decoded, &mut native_bus);
        let interp_fault = interp.execute_decoded(&interp_decoded, &mut interp_bus);
        for fault in [native_fault, interp_fault] {
            assert!(matches!(
                fault,
                Err(InternalFault::Exception {
                    vector: 13,
                    error_code: Some(0)
                })
            ));
        }
        assert_eq!(native.registers, interp.registers);
        assert_eq!(native.pending_flags, interp.pending_flags);
        assert_eq!(&native_bus.memory[TARGET..TARGET + 4], &[0; 4]);
        assert_eq!(&interp_bus.memory[TARGET..TARGET + 4], &[0; 4]);
    }
}

#[test]
fn descriptor_change_selectively_recompiles_and_does_not_keep_a_stale_link() {
    const SOURCE: u32 = 0x101;
    const TARGET: u32 = 0x120;
    const END: u32 = 0x130;
    let mut memory = vec![0; 0x3000];
    memory[SOURCE as usize..SOURCE as usize + 10].copy_from_slice(&[
        0xa1, 0x00, 0x02, 0x00, 0x00, // mov eax,[0x200]
        0xe9, 0x15, 0x00, 0x00, 0x00, // jmp TARGET
    ]);
    memory[TARGET as usize..TARGET as usize + 11].copy_from_slice(&[
        0x8b, 0x0d, 0x04, 0x02, 0x00, 0x00, // mov ecx,[0x204]
        0x83, 0xc1, 0x01, // add ecx,1
        0xeb, 0x05, // jmp END
    ]);
    memory[0x200..0x208].copy_from_slice(&[1, 0, 0, 0, 2, 0, 0, 0]);
    memory[0x1200..0x1208].copy_from_slice(&[3, 0, 0, 0, 4, 0, 0, 0]);
    let mut cpu = flat_stack_cpu(SOURCE);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    decode_fixture(
        &mut cpu,
        &mut bus,
        &[SOURCE, SOURCE + 5, TARGET, TARGET + 6, TARGET + 9],
    );
    map_direct_page(
        &mut cpu,
        &mut bus,
        0,
        0,
        jit::fast_map::PagePermissions::UNPAGED,
        true,
        false,
    );
    let old_source = install_fixture_block(&mut cpu, SOURCE);
    let old_target = install_fixture_block(&mut cpu, TARGET);
    assert!(cpu.jit_direct.has_linked_successor(old_source));

    let mut changed_ds = cpu.registers.segment(SegmentIndex::Ds);
    changed_ds.base = 0x1000;
    cpu.registers.set_segment(SegmentIndex::Ds, changed_ds);
    map_direct_page(
        &mut cpu,
        &mut bus,
        0x1000,
        0x1000,
        jit::fast_map::PagePermissions::UNPAGED,
        true,
        false,
    );
    cpu.set_eip(SOURCE);
    assert!(
        !cpu.try_run_direct_block_for_test(&mut bus, old_source)
            .unwrap()
    );
    let source_key = old_source.span().key;
    assert!(matches!(
        cpu.jit_direct.probe(source_key),
        jit::direct::BlockProbe::Compile
    ));
    let source_compilation = jit::direct::compile(&mut cpu, SOURCE, true).unwrap();
    let source_id = cpu.jit_direct.install(&source_compilation).unwrap();
    let new_source = cpu.jit_direct.block(source_id).unwrap();
    assert!(!cpu.jit_direct.has_linked_successor(new_source));

    cpu.set_eip(TARGET);
    assert!(
        !cpu.try_run_direct_block_for_test(&mut bus, old_target)
            .unwrap()
    );
    let target_key = old_target.span().key;
    assert!(matches!(
        cpu.jit_direct.probe(target_key),
        jit::direct::BlockProbe::Compile
    ));
    let target_compilation = jit::direct::compile(&mut cpu, TARGET, true).unwrap();
    let target_id = cpu.jit_direct.install(&target_compilation).unwrap();
    assert!(cpu.jit_direct.block(target_id).is_some());
    assert!(cpu.jit_direct.has_linked_successor(new_source));

    cpu.registers.set_eax(0);
    cpu.registers.set_ecx(0);
    cpu.set_eip(SOURCE);
    let transfers = cpu.perf_counters().jit_direct_linked_transfers;
    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, new_source)
            .unwrap()
    );
    assert_eq!(cpu.registers.eip, END);
    assert_eq!(cpu.registers.eax(), 3);
    assert_eq!(cpu.registers.ecx(), 5);
    assert_eq!(
        cpu.perf_counters().jit_direct_linked_transfers - transfers,
        1
    );
}

#[test]
fn word_renderer_slice_is_admitted_only_for_586() {
    const ENTRY: u32 = 0x101;
    let code = [
        0x89, 0xc0, // mov eax,eax
        0x89, 0xc9, // mov ecx,ecx
        0x89, 0xd2, // mov edx,edx
        0x66, 0x89, 0xc0, // mov ax,ax
        0x89, 0xdb, // mov ebx,ebx
        0x89, 0xf6, // mov esi,esi
    ];
    let starts = [
        ENTRY,
        ENTRY + 2,
        ENTRY + 4,
        ENTRY + 6,
        ENTRY + 9,
        ENTRY + 11,
    ];

    for (mode, expected_instructions) in [(GswMode::Gsw486, 3), (GswMode::Gsw586, 6)] {
        let mut memory = vec![0; 0x1000];
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
        let mut cpu = flat_stack_cpu(ENTRY);
        cpu.set_mode(mode);
        let mut bus = TestBus::with_memory(memory);
        decode_fixture(&mut cpu, &mut bus, &starts);

        let block = install_fixture_block(&mut cpu, ENTRY);
        assert_eq!(block.span().instructions, expected_instructions, "{mode:?}");
    }
}

#[test]
fn quake_word_renderer_families_match_interpreter_state_flags_memory_and_timing() {
    const ENTRY: u32 = 0x101;
    const DATA: u32 = 0x3000;
    let code = [
        0x66, 0x89, 0xd8, // mov ax,bx
        0x66, 0x8b, 0xf8, // mov di,ax
        0x66, 0x89, 0x0d, 0x00, 0x30, 0x00, 0x00, // mov word [DATA],cx
        0x66, 0x8b, 0x15, 0x00, 0x30, 0x00, 0x00, // mov dx,word [DATA]
        0x66, 0xff, 0x05, 0x02, 0x30, 0x00, 0x00, // inc word [DATA+2]
        0x66, 0xff, 0x0d, 0x02, 0x30, 0x00, 0x00, // dec word [DATA+2]
        0x66, 0x4b, // dec bx
        0x66, 0xff, 0xc1, // inc cx through FF /0
        0x66, 0x39, 0x1d, 0x00, 0x30, 0x00, 0x00, // cmp word [DATA],bx
        0x72, 0x0b, // jb final HLT, not taken when the preceding CMP is correct
        0x66, 0x3b, 0x1d, 0x00, 0x30, 0x00, 0x00, // cmp bx,word [DATA]
        0x89, 0xf6, // mov esi,esi keeps the comparison flags live
        0x89, 0xf6, // second filler keeps the comparison block independently compilable
        0xf4,
    ];
    let starts = [
        ENTRY,
        ENTRY + 3,
        ENTRY + 6,
        ENTRY + 13,
        ENTRY + 20,
        ENTRY + 27,
        ENTRY + 34,
        ENTRY + 36,
        ENTRY + 39,
        ENTRY + 46,
        ENTRY + 48,
        ENTRY + 55,
        ENTRY + 57,
    ];
    let mut memory = vec![0; 0x5000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA as usize + 2..DATA as usize + 4].copy_from_slice(&0xffffu16.to_le_bytes());

    let mut direct = flat_stack_cpu(ENTRY);
    let mut interpreter = flat_stack_cpu(ENTRY);
    let mut direct_bus = TestBus::with_memory(memory.clone());
    let mut interpreter_bus = TestBus::with_memory(memory);
    for bus in [&mut direct_bus, &mut interpreter_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    decode_fixture(&mut direct, &mut direct_bus, &starts);
    decode_fixture(&mut interpreter, &mut interpreter_bus, &starts);
    for (cpu, bus) in [
        (&mut direct, &mut direct_bus),
        (&mut interpreter, &mut interpreter_bus),
    ] {
        map_direct_page(
            cpu,
            bus,
            DATA,
            DATA,
            jit::fast_map::PagePermissions::UNPAGED,
            true,
            true,
        );
        cpu.registers.set_ecx(0xaaaa_1234);
        cpu.registers.set_edx(0xbbbb_0000);
        cpu.registers.set_ebx(0xcccc_1200);
        cpu.registers.set_eax(0xdddd_0000);
        cpu.registers.set_edi(0xeeee_0000);
        cpu.registers.eflags = 0x203;
        cpu.pending_flags = PendingFlags::default();
        cpu.set_eip(ENTRY);
    }
    let first = install_fixture_block(&mut direct, ENTRY);
    let first_compare = install_fixture_block(&mut direct, ENTRY + 39);
    let second_compare = install_fixture_block(&mut direct, ENTRY + 48);
    assert_eq!(first.span().instructions, 8);
    assert_eq!(first.word_reads(), 3);
    assert_eq!(first.word_stores(), 3);
    assert_eq!(first_compare.span().instructions, 2);
    assert_eq!(first_compare.word_reads(), 1);
    assert_eq!(first_compare.word_stores(), 0);
    assert_eq!(second_compare.span().instructions, 3);
    assert_eq!(second_compare.word_reads(), 1);
    assert_eq!(second_compare.word_stores(), 0);

    assert!(
        direct
            .try_run_direct_block_for_test(&mut direct_bus, first)
            .unwrap()
    );
    for _ in 0..13 {
        interpreter.cycle(&mut interpreter_bus).unwrap();
    }

    assert_eq!(direct.registers, interpreter.registers);
    assert_eq!(direct.pending_flags, interpreter.pending_flags);
    assert_eq!(direct.eflags(), interpreter.eflags());
    assert_eq!(direct_bus.memory, interpreter_bus.memory);
    assert_eq!(direct.elapsed_clocks, interpreter.elapsed_clocks);
    assert_eq!(
        direct_bus.trace.elapsed_clocks(),
        interpreter_bus.trace.elapsed_clocks()
    );
    assert_eq!(direct.registers.edx(), 0xbbbb_1234);
    assert_eq!(direct.registers.ebx(), 0xcccc_11ff);
    assert_eq!(direct.registers.ecx(), 0xaaaa_1235);
    assert_eq!(direct.registers.eax(), 0xdddd_1200);
    assert_eq!(direct.registers.edi(), 0xeeee_1200);
    assert_eq!(
        &direct_bus.memory[DATA as usize..DATA as usize + 4],
        &[0x34, 0x12, 0xff, 0xff]
    );
}
