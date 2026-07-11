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
        0xb9, 0x05, 0x00, 0x00, 0x00, // mov ecx,5
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
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;

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
    native.set_jit_auto_admit(true);
    let native_before = native.perf_counters().jit_direct_insns;
    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(
        native_outcomes, interp_outcomes,
        "run-boundary timing differs"
    );
    assert_eq!(native, interp, "architectural or clock state differs");
    assert_eq!(native_bus.trace.cycles(), interp_bus.trace.cycles());
    assert_eq!(native.registers.eax(), 16);
    assert_eq!(native.registers.edx(), 16);
    assert!(
        native.jit_direct.len() > 0,
        "the direct block was not cached"
    );
    assert!(
        native.perf_counters().jit_direct_insns - native_before >= 8,
        "taken and fallthrough executions must both be native: {:?}, cache={}",
        native.perf_counters(),
        native.jit_direct.len()
    );
    assert_eq!(native.perf_counters().jit_direct_side_exits, 0);
}

#[test]
fn resident_chain_crosses_three_blocks_with_one_root_entry() {
    const A: u32 = 0x100;
    const B: u32 = 0x10b;
    const C: u32 = 0x114;
    let mut memory = vec![0; 0x1000];
    memory[0xff] = 0x90;
    memory[A as usize..0x11e].copy_from_slice(&[
        0xb8, 1, 0, 0, 0, 0x89, 0xc3, 0x85, 0xc0, 0x74, 0x12, // A, fall through
        0x83, 0xc0, 1, 0x89, 0xc1, 0x85, 0xc0, 0x74, 0x0b, // B, fall through
        0x83, 0xc0, 2, 0x89, 0xc2, 0x85, 0xc0, 0x74, 0x02, // C, unresolved exit
        0xf4,
    ]);
    let mut native = fresh();
    let mut interp = fresh();
    for cpu in [&mut native, &mut interp] {
        for segment in [SegmentIndex::Cs, SegmentIndex::Ds, SegmentIndex::Ss] {
            let mut descriptor = cpu.registers.segment(segment);
            descriptor.base = 0;
            descriptor.limit = u32::MAX;
            cpu.registers.set_segment(segment, descriptor);
        }
        cpu.registers.eip = 0xff;
    }
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    native_bus.direct_pages_enabled = true;
    interp_bus.direct_pages_enabled = true;
    drive(&mut native, &mut native_bus);
    drive(&mut interp, &mut interp_bus);

    let mut root = None;
    for linear in [A, B, C] {
        let key = jit::direct::key_for(&native, linear, true).expect("decoded chain entry");
        assert!(matches!(
            native.jit_direct.probe(key),
            jit::direct::BlockProbe::Interpret
        ));
        let compilation = jit::direct::compile(&mut native, linear, true).expect("direct block");
        let id = native
            .jit_direct
            .install(&compilation)
            .expect("direct block install");
        if linear == A {
            root = native.jit_direct.block(id);
        }
    }
    native.jit_direct.set_defer_short_for_test(true);
    for cpu in [&mut native, &mut interp] {
        cpu.halted = false;
        cpu.registers.eip = A;
        cpu.registers.gpr.fill(0);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    native_bus.trace = BusTrace::default();
    interp_bus.trace = BusTrace::default();
    let root = root.expect("root block");
    let start_registers = native.registers.clone();
    let start_pending = native.pending_flags;
    let transfers_before_deadline = native.perf_counters().jit_direct_linked_transfers;
    assert!(
        native
            .try_run_direct_block_with_cap_for_test(
                &mut native_bus,
                root,
                u64::from(root.raw_clocks()) + 1,
            )
            .unwrap()
    );
    assert_eq!(native.registers.eip, B);
    assert_eq!(
        native.perf_counters().jit_direct_linked_transfers - transfers_before_deadline,
        0,
    );
    native.registers = start_registers;
    native.pending_flags = start_pending;
    native.elapsed_clocks = 0;
    native.timing_rem = 0;
    native.core_clocks_so_far = 0;
    native_bus.trace = BusTrace::default();
    let entries = native.perf_counters().jit_direct_entries;
    let transfers = native.perf_counters().jit_direct_linked_transfers;

    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, root)
            .unwrap()
    );
    for _ in 0..12 {
        interp.cycle(&mut interp_bus).unwrap();
    }

    assert_eq!(native.registers, interp.registers);
    assert_eq!(native.pending_flags, interp.pending_flags);
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert_eq!(native.perf_counters().jit_direct_entries - entries, 1);
    assert_eq!(
        native.perf_counters().jit_direct_linked_transfers - transfers,
        2
    );
}

#[test]
fn hot_loop_head_interprets_first_and_compiles_second() {
    let mut cpu = fresh();
    let mut memory = loop_program();
    memory[0x101] = 1;
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    drive(&mut cpu, &mut bus);
    cpu.set_jit_auto_admit(true);

    let run_once = |cpu: &mut CpuGsw, bus: &mut TestBus| {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.registers.set_eax(0);
        cpu.registers.set_ecx(1);
        cpu.registers.set_edx(0);
        drive(cpu, bus);
    };

    run_once(&mut cpu, &mut bus);
    assert!(cpu.jit_direct.tracked_len() > 0);
    assert_eq!(cpu.jit_direct.len(), 0);

    run_once(&mut cpu, &mut bus);
    assert_eq!(cpu.jit_direct.len(), 1);
    assert_eq!(cpu.perf_counters().jit_direct_entries, 1);
    assert_eq!(cpu.perf_counters().jit_direct_insns, 4);

    run_once(&mut cpu, &mut bus);
    assert_eq!(cpu.jit_direct.len(), 1);
    assert_eq!(cpu.perf_counters().jit_direct_entries, 2);
    assert_eq!(cpu.perf_counters().jit_direct_insns, 8);
    assert_eq!(cpu.registers.eax(), 3);
    assert_eq!(cpu.registers.edx(), 3);
}

#[test]
fn cr3_and_invlpg_keep_compiled_blocks_and_reuse_same_mapping() {
    for (name, system_instruction) in [
        ("mov cr3", &[0x0f, 0x22, 0xd8][..]),
        ("invlpg", &[0x0f, 0x01, 0x3d, 0x40, 0, 0, 0][..]),
    ] {
        let mut memory = loop_program();
        memory[0x80..0x80 + system_instruction.len()].copy_from_slice(system_instruction);
        memory[0x101..0x105].copy_from_slice(&1u32.to_le_bytes());
        let mut cpu = fresh();
        let mut bus = TestBus::with_memory(memory);
        bus.direct_pages_enabled = true;
        drive(&mut cpu, &mut bus);
        cpu.set_jit_auto_admit(true);
        for _ in 0..3 {
            cpu.halted = false;
            cpu.registers.eip = 0x100;
            drive(&mut cpu, &mut bus);
        }
        let cached_blocks = cpu.jit_direct.len();
        let installs = cpu.perf_counters().jit_direct_blocks_installed;
        let compile_attempts = cpu.perf_counters().jit_direct_compile_attempts;
        let entries = cpu.perf_counters().jit_direct_entries;
        assert!(cached_blocks > 0, "{name}");

        cpu.halted = false;
        cpu.registers.eip = 0x80;
        cpu.registers.set_eax(0);
        cpu.cycle_no_interrupt_check(&mut bus).unwrap();

        assert_eq!(cpu.jit_direct.len(), cached_blocks, "{name}");
        assert_eq!(
            cpu.perf_counters().jit_direct_blocks_installed,
            installs,
            "{name}"
        );
        for _ in 0..2 {
            cpu.halted = false;
            cpu.registers.eip = 0x100;
            drive(&mut cpu, &mut bus);
        }
        assert_eq!(cpu.jit_direct.len(), cached_blocks, "{name}");
        assert_eq!(
            cpu.perf_counters().jit_direct_blocks_installed,
            installs,
            "{name}"
        );
        assert_eq!(
            cpu.perf_counters().jit_direct_compile_attempts,
            compile_attempts,
            "{name}"
        );
        assert!(cpu.perf_counters().jit_direct_entries > entries, "{name}");
    }
}

#[test]
fn direct_compile_rejects_non_direct_and_partial_instruction_pages() {
    for (name, memory_len, direct_pages_enabled) in [
        ("non-direct", 0x1000, false),
        ("partial final page", 0x0800, true),
    ] {
        let mut memory = loop_program();
        memory[0x101..0x105].copy_from_slice(&1u32.to_le_bytes());
        memory.truncate(memory_len);
        let mut cpu = fresh();
        let mut bus = TestBus::with_memory(memory);
        bus.direct_pages_enabled = direct_pages_enabled;

        drive(&mut cpu, &mut bus);
        cpu.set_jit_auto_admit(true);
        for _ in 0..3 {
            cpu.halted = false;
            cpu.registers.eip = 0x100;
            drive(&mut cpu, &mut bus);
        }

        assert!(cpu.jit_direct.tracked_len() > 0, "{name}");
        assert_eq!(cpu.jit_direct.len(), 0, "{name}");
        assert_eq!(cpu.perf_counters().jit_direct_entries, 0, "{name}");
    }
}

#[test]
fn direct_block_replays_cold_fetch_after_internal_decode_line_collision() {
    const EVICTED_SLOT: u32 = 0x108;

    let mut interp = fresh();
    let mut native = fresh();
    let collision = EVICTED_SLOT + native.decode_cache.mask + 1;
    let mut memory = loop_program();
    memory[0x101..0x105].copy_from_slice(&1u32.to_le_bytes());
    memory.resize(collision as usize + 1, 0);
    memory[collision as usize] = 0xf4;
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;
    native_bus.direct_page_clocks = true;

    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..3 {
        native.halted = false;
        native.registers.eip = 0x100;
        drive(&mut native, &mut native_bus);
    }
    assert!(
        native.jit_direct.len() > 0,
        "tracked={} perf={:?}",
        native.jit_direct.tracked_len(),
        native.perf_counters()
    );

    for (cpu, bus) in [
        (&mut interp, &mut interp_bus),
        (&mut native, &mut native_bus),
    ] {
        cpu.halted = false;
        cpu.registers.eip = collision;
        assert!(cpu.cycle_no_interrupt_check(bus).unwrap().halted);
    }

    for cpu in [&mut interp, &mut native] {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.registers.gpr.fill(0);
        cpu.registers.eflags = 0x203;
        cpu.pending_flags = PendingFlags::default();
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    interp_bus.trace = BusTrace::default();
    native_bus.trace = BusTrace::default();
    let direct_entries = native.perf_counters().jit_direct_entries;
    let direct_insns = native.perf_counters().jit_direct_insns;

    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(native_outcomes, interp_outcomes);
    assert_eq!(native, interp);
    assert_eq!(native_bus.trace.cycles(), interp_bus.trace.cycles());
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    let replay_entries = native.perf_counters().jit_direct_entries;
    let replay_insns = native.perf_counters().jit_direct_insns;
    assert!(replay_entries > direct_entries);
    assert!(replay_insns - direct_insns < 5);
    assert!(
        native.jit_direct.len() > 0,
        "the compiled block stays reusable"
    );

    native.halted = false;
    native.registers.eip = 0x100;
    native.registers.gpr.fill(0);
    drive(&mut native, &mut native_bus);
    assert!(native.perf_counters().jit_direct_entries > replay_entries);
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
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;

    // Warm with admission disabled, then run one first encounter before the measured compile.
    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..2 {
        for cpu in [&mut interp, &mut native] {
            cpu.halted = false;
            cpu.registers.eip = 0x100;
            cpu.alu_add(0xffff_ffff, 1, 0, BusWidth::Dword);
        }
        drive(&mut interp, &mut interp_bus);
        drive(&mut native, &mut native_bus);
    }

    for cpu in [&mut interp, &mut native] {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.alu_add(0xffff_ffff, 1, 0, BusWidth::Dword);
    }
    let interp_elapsed = interp.elapsed_clocks;
    let native_elapsed = native.elapsed_clocks;
    let native_before = native.perf_counters().jit_direct_insns;
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
        native.perf_counters().jit_direct_insns - native_before >= 3,
        "direct shift did not run: {:?}, cache={}",
        native.perf_counters(),
        native.jit_direct.len()
    );
}

struct DirectRegisterCase {
    name: &'static str,
    code: &'static [u8],
    initial_gpr: [u32; 8],
    initial_eflags: u32,
    expected_gpr: [u32; 8],
}

fn arm_direct_register_case(cpu: &mut CpuGsw, case: &DirectRegisterCase) {
    cpu.halted = false;
    cpu.registers.eip = 0x100;
    cpu.registers.gpr = case.initial_gpr;
    cpu.registers.eflags = case.initial_eflags;
    cpu.pending_flags = PendingFlags::default();
    make_data_segments_flat(cpu);
}

fn assert_direct_register_case(case: &DirectRegisterCase) {
    let mut memory = vec![0; 0x1000];
    memory[0x100] = 0x90;
    memory[0x101..0x101 + case.code.len()].copy_from_slice(case.code);
    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;
    native_bus.direct_page_clocks = true;

    arm_direct_register_case(&mut interp, case);
    drive(&mut interp, &mut interp_bus);
    arm_direct_register_case(&mut native, case);
    drive(&mut native, &mut native_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..3 {
        arm_direct_register_case(&mut native, case);
        drive(&mut native, &mut native_bus);
    }
    assert!(
        native.jit_direct.len() > 0,
        "{} did not compile: tracked={} perf={:?}",
        case.name,
        native.jit_direct.tracked_len(),
        native.perf_counters()
    );

    for cpu in [&mut interp, &mut native] {
        arm_direct_register_case(cpu, case);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.trace = BusTrace::default();
    }
    let direct_insns = native.perf_counters().jit_direct_insns;
    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(native_outcomes, interp_outcomes, "{} timing", case.name);
    assert_eq!(native, interp, "{} CPU state", case.name);
    assert_eq!(
        native.pending_flags, interp.pending_flags,
        "{} pending flags",
        case.name
    );
    assert_eq!(native.eflags(), interp.eflags(), "{} EFLAGS", case.name);
    assert_eq!(
        native.registers.gpr, case.expected_gpr,
        "{} GPRs",
        case.name
    );
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks(),
        "{} bus timing",
        case.name
    );
    assert!(
        native.perf_counters().jit_direct_insns - direct_insns >= 3,
        "{} did not run natively: {:?}",
        case.name,
        native.perf_counters()
    );
}

#[test]
fn direct_integer_continuity_matches_flags_bytes_and_effective_addresses() {
    let cases = [
        DirectRegisterCase {
            name: "INC/DEC preserves set CF",
            code: &[0x40, 0x4b, 0x89, 0xc1, 0xf4],
            initial_gpr: [0x7fff_ffff, 0, 0, 0x8000_0000, 0, 0, 0, 0],
            initial_eflags: 0x203,
            expected_gpr: [0x8000_0000, 0x8000_0000, 0, 0x7fff_ffff, 0, 0, 0, 0],
        },
        DirectRegisterCase {
            name: "INC/DEC preserves clear CF",
            code: &[0x40, 0x4b, 0x89, 0xc1, 0xf4],
            initial_gpr: [u32::MAX, 0, 0, 0, 0, 0, 0, 0],
            initial_eflags: 0x202,
            expected_gpr: [0, 0, 0, u32::MAX, 0, 0, 0, 0],
        },
        DirectRegisterCase {
            name: "high byte MOV forms",
            code: &[0xb4, 0xab, 0x88, 0xe5, 0x8a, 0xdd, 0xc6, 0xc6, 0x7e, 0xf4],
            initial_gpr: [0x1122_3344, 0x5566_7788, 0, 0xaabb_ccdd, 0, 0, 0, 0],
            initial_eflags: 0x202,
            expected_gpr: [
                0x1122_ab44,
                0x5566_ab88,
                0x0000_7e00,
                0xaabb_ccab,
                0,
                0,
                0,
                0,
            ],
        },
        DirectRegisterCase {
            name: "dword LEA",
            code: &[
                0x8d, 0x54, 0xb3, 0xe0, 0x89, 0xd1, 0xb8, 0x78, 0x56, 0x34, 0x12, 0xf4,
            ],
            initial_gpr: [0, 0, 0, 0x1000, 0, 0, 0x10, 0],
            initial_eflags: 0x202,
            expected_gpr: [0x1234_5678, 0x1020, 0x1020, 0x1000, 0, 0, 0x10, 0],
        },
        DirectRegisterCase {
            name: "ADC carry clear lazy flags",
            code: &[0x11, 0xc8, 0x89, 0xc2, 0x89, 0xd3, 0xf4],
            initial_gpr: [u32::MAX, 1, 0, 0, 0, 0, 0, 0],
            initial_eflags: 0x202,
            expected_gpr: [0, 1, 0, 0, 0, 0, 0, 0],
        },
        DirectRegisterCase {
            name: "ADC carry set eager flags",
            code: &[0x13, 0xc1, 0x89, 0xc2, 0x89, 0xd3, 0xf4],
            initial_gpr: [0x7fff_ffff, 0, 0, 0, 0, 0, 0, 0],
            initial_eflags: 0x203,
            expected_gpr: [0x8000_0000, 0, 0x8000_0000, 0x8000_0000, 0, 0, 0, 0],
        },
        DirectRegisterCase {
            name: "SBB borrow clear lazy flags",
            code: &[0x1b, 0xc1, 0x89, 0xc2, 0x89, 0xd3, 0xf4],
            initial_gpr: [0, 1, 0, 0, 0, 0, 0, 0],
            initial_eflags: 0x202,
            expected_gpr: [u32::MAX, 1, u32::MAX, u32::MAX, 0, 0, 0, 0],
        },
        DirectRegisterCase {
            name: "SBB borrow set eager flags",
            code: &[0x19, 0xc8, 0x89, 0xc2, 0x89, 0xd3, 0xf4],
            initial_gpr: [0x8000_0000, 0, 0, 0, 0, 0, 0, 0],
            initial_eflags: 0x203,
            expected_gpr: [0x7fff_ffff, 0, 0x7fff_ffff, 0x7fff_ffff, 0, 0, 0, 0],
        },
        DirectRegisterCase {
            name: "ADC/SBB register immediates",
            code: &[
                0x83, 0xd0, 0xff, 0x81, 0xdb, 0x00, 0x00, 0x00, 0x00, 0x89, 0xc1, 0xf4,
            ],
            initial_gpr: [0, 0, 0, 5, 0, 0, 0, 0],
            initial_eflags: 0x203,
            expected_gpr: [0, 0, 0, 4, 0, 0, 0, 0],
        },
        DirectRegisterCase {
            name: "ADC/SBB accumulator immediates",
            code: &[
                0x15, 0x00, 0x00, 0x00, 0x00, 0x1d, 0x01, 0x00, 0x00, 0x00, 0x89, 0xc2, 0xf4,
            ],
            initial_gpr: [0, 0, 0, 0, 0, 0, 0, 0],
            initial_eflags: 0x203,
            expected_gpr: [0, 0, 0, 0, 0, 0, 0, 0],
        },
        DirectRegisterCase {
            name: "group 80 high byte carry and borrow",
            code: &[
                0x80, 0xc4, 0x01, 0x80, 0xdc, 0x00, 0x80, 0xd4, 0x00, 0x80, 0xfc, 0x00, 0xf4,
            ],
            initial_gpr: [0x0000_ff00, 0, 0, 0, 0, 0, 0, 0],
            initial_eflags: 0x202,
            expected_gpr: [0, 0, 0, 0, 0, 0, 0, 0],
        },
        DirectRegisterCase {
            name: "group 80 high byte logic and compare",
            code: &[
                0x80, 0xcc, 0x0f, 0x80, 0xe4, 0xf3, 0x80, 0xf4, 0x33, 0x80, 0xfc, 0xc0, 0xf4,
            ],
            initial_gpr: [0x0000_f000, 0, 0, 0, 0, 0, 0, 0],
            initial_eflags: 0x212,
            expected_gpr: [0x0000_c000, 0, 0, 0, 0, 0, 0, 0],
        },
    ];

    for case in &cases {
        assert_direct_register_case(case);
    }
}

#[test]
fn cold_straight_line_code_is_seen_but_not_compiled() {
    let memory = shift_program();
    let mut cpu = fresh();
    let mut bus = TestBus::with_memory(memory);

    drive(&mut cpu, &mut bus);
    cpu.halted = false;
    cpu.registers.eip = 0x100;
    cpu.set_jit_auto_admit(true);

    drive(&mut cpu, &mut bus);

    assert!(cpu.jit_direct.tracked_len() > 0);
    assert_eq!(cpu.jit_direct.len(), 0);
    assert_eq!(cpu.perf_counters().jit_direct_entries, 0);
}

#[test]
fn hot_unsupported_entries_stay_interpreted_without_legacy_region_fallback() {
    let mut memory = vec![0; 0x1000];
    memory[0x100..0x10d].copy_from_slice(&[
        0xb9, 0x05, 0x00, 0x00, 0x00, // mov ecx,5
        0x90, // nop: unsupported direct entry
        0x90, // nop: unsupported direct entry
        0x83, 0xe9, 0x01, // sub ecx,1
        0x75, 0xf9, // jnz 0x105
        0xf4, // hlt
    ]);
    let mut cpu = fresh();
    cpu.set_jit_auto_admit(true);
    let mut bus = TestBus::with_memory(memory);

    drive(&mut cpu, &mut bus);

    assert!(cpu.jit_direct.tracked_len() > 0);
    assert_eq!(cpu.jit_direct.len(), 0);
    assert_eq!(cpu.perf_counters().jit_direct_entries, 0);
    assert_eq!(cpu.perf_counters().jit_region_entries, 0);
    assert_eq!(cpu.registers.ecx(), 0);
}

#[test]
fn supported_prefix_compiles_before_an_unsupported_barrier() {
    let mut memory = vec![0; 0x1000];
    memory[0x100..0x112].copy_from_slice(&[
        0x90, // starter
        0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax,1
        0x89, 0xc3, // mov ebx,eax
        0x83, 0xc3, 0x02, // add ebx,2
        0x90, // unsupported barrier
        0xb9, 0x04, 0x00, 0x00, 0x00, // mov ecx,4
        0xf4, // hlt
    ]);
    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;

    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..3 {
        native.halted = false;
        native.registers.eip = 0x100;
        drive(&mut native, &mut native_bus);
    }
    assert_eq!(native.jit_direct.len(), 1);

    for cpu in [&mut interp, &mut native] {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.registers.gpr.fill(0);
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.trace = BusTrace::default();
    }
    let direct_before = native.perf_counters().jit_direct_insns;

    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(native_outcomes, interp_outcomes);
    assert_eq!(native, interp);
    assert_eq!(native_bus.trace.cycles(), interp_bus.trace.cycles());
    assert_eq!(native.registers.eax(), 1);
    assert_eq!(native.registers.ebx(), 3);
    assert_eq!(native.registers.ecx(), 4);
    assert_eq!(native.perf_counters().jit_direct_insns - direct_before, 3);
    assert_eq!(native.perf_counters().jit_region_entries, 0);
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
        assert_eq!(cpu.perf_counters().jit_direct_entries, 0);
        assert_eq!(cpu.perf_counters().jit_direct_insns, 0);
        assert_eq!(cpu.jit_direct.len(), 0);
    }
}

const READ_ENTRY: u32 = 0x101;

fn arm_read_fixture(cpu: &mut CpuGsw) {
    cpu.halted = false;
    cpu.registers.eip = READ_ENTRY - 1;
    cpu.registers.set_eax(0);
    cpu.registers.set_ecx(0);
    cpu.registers.set_edx(0);
    cpu.registers.set_ebx(0x400);
    cpu.registers.set_esi(0x20);
    cpu.registers.set_edi(0);
    let mut ds = cpu.registers.segment(SegmentIndex::Ds);
    ds.base = 0;
    ds.limit = u32::MAX;
    cpu.registers.set_segment(SegmentIndex::Ds, ds);
    cpu.registers.eflags = 0x202;
    cpu.pending_flags = PendingFlags::default();
    cpu.alu_add(0xffff_ffff, 1, 0, BusWidth::Dword);
}

fn prime_direct_memory_block(cpu: &mut CpuGsw, bus: &mut TestBus) {
    cpu.set_jit_auto_admit(true);
    arm_read_fixture(cpu);
    drive(cpu, bus);
    for _ in 0..3 {
        arm_read_fixture(cpu);
        drive(cpu, bus);
    }
    assert!(
        cpu.jit_direct.len() > 0,
        "direct read block did not compile"
    );
}

fn reset_read_measurement(cpu: &mut CpuGsw, bus: &mut TestBus) {
    arm_read_fixture(cpu);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
    bus.trace = BusTrace::default();
}

fn assert_read_parity(
    interp: &CpuGsw,
    interp_bus: &TestBus,
    native: &CpuGsw,
    native_bus: &TestBus,
) {
    assert_eq!(
        native, interp,
        "register, EFLAGS, pending flags, or clocks differ"
    );
    assert_eq!(native.registers, interp.registers);
    assert_eq!(native.eflags(), interp.eflags());
    assert_eq!(native.pending_flags, interp.pending_flags);
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks(),
        "bulk direct-RAM bus clocks differ"
    );
}

fn successful_read_program() -> Vec<u8> {
    let mut memory = vec![0; 0x2000];
    memory[(READ_ENTRY - 1) as usize] = 0x90;
    let code = [
        0xa1, 0x00, 0x03, 0x00, 0x00, // mov eax,[0x300]
        0x8b, 0x0b, // mov ecx,[ebx]
        0x8b, 0x14, 0x33, // mov edx,[ebx+esi]
        0x8b, 0xbc, 0x33, 0x00, 0x10, 0x00, 0x00, // mov edi,[ebx+esi+0x1000]
        0x8a, 0x04, 0x33, // mov al,[ebx+esi]
        0x89, 0xc6, // mov esi,eax
        0xf4, // hlt
    ];
    memory[READ_ENTRY as usize..READ_ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[0x300..0x304].copy_from_slice(&0x1122_3344u32.to_le_bytes());
    memory[0x400..0x404].copy_from_slice(&0x5566_7788u32.to_le_bytes());
    memory[0x420..0x424].copy_from_slice(&0xa1b2_c3d4u32.to_le_bytes());
    memory[0x1420..0x1424].copy_from_slice(&0xcafe_babeu32.to_le_bytes());
    memory
}

#[test]
fn direct_ram_reads_cover_moffs_reg_sib_and_disp32_with_exact_state() {
    let memory = successful_read_program();
    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;
    native_bus.direct_page_clocks = true;

    arm_read_fixture(&mut interp);
    drive(&mut interp, &mut interp_bus);
    prime_direct_memory_block(&mut native, &mut native_bus);
    reset_read_measurement(&mut interp, &mut interp_bus);
    reset_read_measurement(&mut native, &mut native_bus);
    let native_before = native.perf_counters().jit_direct_insns;

    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(native_outcomes, interp_outcomes);
    assert_read_parity(&interp, &interp_bus, &native, &native_bus);
    assert_eq!(native.registers.eax(), 0x1122_33d4);
    assert_eq!(native.registers.ecx(), 0x5566_7788);
    assert_eq!(native.registers.edx(), 0xa1b2_c3d4);
    assert_eq!(native.registers.edi(), 0xcafe_babe);
    assert_eq!(native.registers.esi(), 0x1122_33d4);
    assert_eq!(native.perf_counters().jit_direct_insns - native_before, 6);
}

fn carry_memory_source_program() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[0x100] = 0x90;
    memory[0x101..0x110].copy_from_slice(&[
        0x13, 0x05, 0x00, 0x03, 0x00, 0x00, // adc eax,[0x300]
        0x1b, 0x1d, 0x04, 0x03, 0x00, 0x00, // sbb ebx,[0x304]
        0x89, 0xc2, // mov edx,eax
        0xf4,
    ]);
    memory[0x300..0x304].copy_from_slice(&0u32.to_le_bytes());
    memory[0x304..0x308].copy_from_slice(&1u32.to_le_bytes());
    memory
}

fn arm_carry_memory_source(cpu: &mut CpuGsw) {
    cpu.halted = false;
    cpu.registers.eip = 0x100;
    cpu.registers.gpr.fill(0);
    cpu.registers.set_eax(0x7fff_ffff);
    cpu.registers.set_ebx(0x8000_0000);
    cpu.registers.eflags = 0x203;
    cpu.pending_flags = PendingFlags::default();
    make_data_segments_flat(cpu);
}

#[test]
fn direct_adc_sbb_memory_sources_match_eager_and_lazy_flag_paths() {
    let memory = carry_memory_source_program();
    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;
    native_bus.direct_page_clocks = true;

    arm_carry_memory_source(&mut interp);
    drive(&mut interp, &mut interp_bus);
    prime_direct_memory_block(&mut native, &mut native_bus);
    for cpu in [&mut interp, &mut native] {
        arm_carry_memory_source(cpu);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.trace = BusTrace::default();
    }
    let direct_insns = native.perf_counters().jit_direct_insns;
    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(native_outcomes, interp_outcomes);
    assert_read_parity(&interp, &interp_bus, &native, &native_bus);
    assert_eq!(native.registers.eax(), 0x8000_0000);
    assert_eq!(native.registers.ebx(), 0x7fff_ffff);
    assert_eq!(native.registers.edx(), 0x8000_0000);
    assert_eq!(native.perf_counters().jit_direct_insns - direct_insns, 3);
    assert_eq!(native.perf_counters().jit_direct_side_exits, 0);
}

fn prefix_exit_program(cross_page: bool) -> Vec<u8> {
    let mut memory = vec![0; 0x2000];
    memory[(READ_ENTRY - 1) as usize] = 0x90;
    let address = if cross_page { 0x0fff_u32 } else { 0x0300 };
    let mut code = vec![
        0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax,1
        0x89, 0xc1, // mov ecx,eax
        0x83, 0xc1, 0x02, // add ecx,2
        0xa1,
    ];
    code.extend_from_slice(&address.to_le_bytes());
    code.extend_from_slice(&[0x89, 0xc2, 0xf4]); // mov edx,eax; hlt
    memory[READ_ENTRY as usize..READ_ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[0x300..0x304].copy_from_slice(&0xdead_beefu32.to_le_bytes());
    memory[0x0fff..0x1003].copy_from_slice(&0x1234_5678u32.to_le_bytes());
    memory
}

#[test]
fn fast_map_miss_returns_prefix_then_interprets_exact_faulting_read() {
    let memory = prefix_exit_program(false);
    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;
    native_bus.direct_page_clocks = true;
    arm_read_fixture(&mut interp);
    drive(&mut interp, &mut interp_bus);
    prime_direct_memory_block(&mut native, &mut native_bus);
    native.jit_fast_map.invalidate_page(0x300);
    reset_read_measurement(&mut interp, &mut interp_bus);
    reset_read_measurement(&mut native, &mut native_bus);
    let native_before = native.perf_counters().jit_direct_insns;
    let exits_before = native.perf_counters().jit_direct_side_exits;
    let unavailable_before = native.perf_counters().jit_direct_exit_unavailable_or_kind;

    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(native_outcomes, interp_outcomes);
    assert_read_parity(&interp, &interp_bus, &native, &native_bus);
    assert_eq!(native.registers.eax(), 0xdead_beef);
    assert_eq!(native.registers.edx(), 0xdead_beef);
    assert_eq!(native.perf_counters().jit_direct_insns - native_before, 3);
    assert_eq!(
        native.perf_counters().jit_direct_side_exits - exits_before,
        1
    );
    assert_eq!(
        native.perf_counters().jit_direct_exit_unavailable_or_kind - unavailable_before,
        1
    );
}

#[test]
fn paging_alias_dpc_hit_refills_fast_map_for_native_retry() {
    const ALIAS_A: u32 = 0x3000;
    const ALIAS_B: u32 = 0x4000;
    const FRAME: u32 = 0x5000;

    let mut memory = vec![0; 0x7000];
    memory[0x1000..0x1004].copy_from_slice(&0x0000_2007u32.to_le_bytes());
    memory[0x2000..0x2004].copy_from_slice(&0x0000_0007u32.to_le_bytes());
    memory[0x200c..0x2010].copy_from_slice(&0x0000_5007u32.to_le_bytes());
    memory[0x2010..0x2014].copy_from_slice(&0x0000_5007u32.to_le_bytes());
    memory[0x100] = 0x90;
    memory[0x101..0x10b].copy_from_slice(&[
        0xa1, 0x00, 0x40, 0x00, 0x00, // mov eax,[0x4000]
        0x89, 0xc2, // mov edx,eax
        0x89, 0xc1, // mov ecx,eax
        0xf4,
    ]);
    memory[FRAME as usize..FRAME as usize + 4].copy_from_slice(&0x7654_3210u32.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    let mut cpu = fresh();
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.control.cr3 = 0x1000;
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0,
            base: 0,
            limit: u32::MAX,
            access: 0x9b,
            default_size_32: true,
        },
    );
    for segment in [SegmentIndex::Ds, SegmentIndex::Ss, SegmentIndex::Es] {
        cpu.registers.set_segment(
            segment,
            SegmentRegister {
                selector: 0,
                base: 0,
                limit: u32::MAX,
                access: 0x93,
                default_size_32: true,
            },
        );
    }
    cpu.set_jit_auto_admit(true);
    assert_eq!(
        cpu.read_memory_sized(
            &mut bus,
            SegmentIndex::Ds,
            ALIAS_A,
            OperandSize::Dword,
            BusAccessKind::DataRead,
        )
        .unwrap(),
        0x7654_3210
    );

    let arm = |cpu: &mut CpuGsw| {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.registers.set_eax(0);
        cpu.registers.set_ecx(0);
        cpu.registers.set_edx(0);
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
    };
    for _ in 0..4 {
        arm(&mut cpu);
        drive(&mut cpu, &mut bus);
    }
    assert!(cpu.jit_direct.len() > 0);
    assert!(cpu.jit_fast_map.has_read_mapping(ALIAS_A, FRAME));
    assert!(cpu.jit_fast_map.has_read_mapping(ALIAS_B, FRAME));

    assert_eq!(
        cpu.read_memory_sized(
            &mut bus,
            SegmentIndex::Ds,
            ALIAS_A,
            OperandSize::Dword,
            BusAccessKind::DataRead,
        )
        .unwrap(),
        0x7654_3210
    );
    cpu.jit_fast_map.invalidate_page(ALIAS_B);
    assert!(!cpu.jit_fast_map.has_read_mapping(ALIAS_B, FRAME));
    let unavailable = cpu.perf_counters().jit_direct_exit_unavailable_or_kind;
    arm(&mut cpu);
    drive(&mut cpu, &mut bus);
    assert_eq!(
        cpu.perf_counters().jit_direct_exit_unavailable_or_kind - unavailable,
        1
    );
    assert_eq!(cpu.registers.eax(), 0x7654_3210);
    assert!(
        cpu.jit_fast_map.has_read_mapping(ALIAS_B, FRAME),
        "the interpreted DPC hit must fill the missing linear alias"
    );

    let unavailable = cpu.perf_counters().jit_direct_exit_unavailable_or_kind;
    let native_insns = cpu.perf_counters().jit_direct_insns;
    arm(&mut cpu);
    drive(&mut cpu, &mut bus);
    assert_eq!(
        cpu.perf_counters().jit_direct_exit_unavailable_or_kind,
        unavailable
    );
    assert!(cpu.perf_counters().jit_direct_insns - native_insns >= 3);
    assert_eq!(cpu.registers.eax(), 0x7654_3210);
    assert_eq!(cpu.registers.edx(), 0x7654_3210);
    assert_eq!(cpu.registers.ecx(), 0x7654_3210);
}

#[test]
fn cross_page_dword_read_side_exits_after_only_the_completed_prefix() {
    let memory = prefix_exit_program(true);
    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;
    native_bus.direct_page_clocks = true;

    // Allocate/populate the map through an unrelated same-page read before compiling the block.
    native.set_jit_auto_admit(true);
    native
        .read_memory_sized(
            &mut native_bus,
            SegmentIndex::Ds,
            0x300,
            OperandSize::Dword,
            BusAccessKind::DataRead,
        )
        .unwrap();
    arm_read_fixture(&mut interp);
    drive(&mut interp, &mut interp_bus);
    prime_direct_memory_block(&mut native, &mut native_bus);
    reset_read_measurement(&mut interp, &mut interp_bus);
    reset_read_measurement(&mut native, &mut native_bus);
    let native_before = native.perf_counters().jit_direct_insns;
    let exits_before = native.perf_counters().jit_direct_side_exits;
    let cross_page_before = native
        .perf_counters()
        .jit_direct_exit_cross_page_or_alignment;

    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(native_outcomes, interp_outcomes);
    assert_read_parity(&interp, &interp_bus, &native, &native_bus);
    assert_eq!(native.registers.eax(), 0x1234_5678);
    assert_eq!(native.registers.edx(), 0x1234_5678);
    assert_eq!(native.perf_counters().jit_direct_insns - native_before, 3);
    assert_eq!(
        native.perf_counters().jit_direct_side_exits - exits_before,
        1
    );
    assert_eq!(
        native
            .perf_counters()
            .jit_direct_exit_cross_page_or_alignment
            - cross_page_before,
        1
    );
}

#[test]
fn cpl3_fast_map_permission_side_exit_is_counted() {
    let mut cpu = fresh();
    let mut bus = TestBus::with_memory(successful_read_program());
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;

    arm_read_fixture(&mut cpu);
    drive(&mut cpu, &mut bus);
    cpu.control.cr0 |= CR0_PE;
    cpu.cpl = 3;
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 3,
            base: 0,
            limit: u32::MAX,
            access: 0xfb,
            default_size_32: true,
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
                default_size_32: true,
            },
        );
    }
    arm_read_fixture(&mut cpu);
    cpu.registers.eip = READ_ENTRY;
    let page = bus
        .direct_page(0x300, BusAccessKind::DataRead)
        .unwrap()
        .unwrap();
    assert!(cpu.jit_fast_map.populate_read(
        0x300,
        0x300,
        page,
        jit::fast_map::PagePermissions {
            writable: true,
            user: false,
        },
    ));

    let key = jit::direct::key_for(&cpu, READ_ENTRY, true).unwrap();
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = jit::direct::compile(&mut cpu, READ_ENTRY, true).unwrap();
    let id = cpu.jit_direct.install(&compilation).unwrap();
    let block = cpu
        .jit_direct
        .block(id)
        .expect("installed block must be live");

    let exits_before = cpu.perf_counters().jit_direct_side_exits;
    let permissions_before = cpu.perf_counters().jit_direct_exit_permission;
    assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    assert_eq!(cpu.registers.eip, READ_ENTRY);
    assert_eq!(cpu.perf_counters().jit_direct_side_exits - exits_before, 1);
    assert_eq!(
        cpu.perf_counters().jit_direct_exit_permission - permissions_before,
        1
    );
}

const STORE_ENTRY: u32 = 0x1101;

fn arm_store_fixture(cpu: &mut CpuGsw) {
    cpu.halted = false;
    cpu.registers.eip = STORE_ENTRY - 1;
    cpu.registers.set_eax(0);
    cpu.registers.set_ecx(0x10);
    cpu.registers.set_edx(0x5566_7788);
    cpu.registers.set_ebx(0xa1b2_c3d4);
    cpu.registers.set_esp(0x3000);
    cpu.registers.set_esi(0x20);
    for segment in [SegmentIndex::Ds, SegmentIndex::Ss] {
        let mut descriptor = cpu.registers.segment(segment);
        descriptor.base = 0;
        descriptor.limit = u32::MAX;
        cpu.registers.set_segment(segment, descriptor);
    }
    cpu.registers.eflags = 0x202;
    cpu.pending_flags = PendingFlags::default();
}

fn prime_direct_store_block(cpu: &mut CpuGsw, bus: &mut TestBus) {
    cpu.set_jit_auto_admit(true);
    arm_store_fixture(cpu);
    drive(cpu, bus);
    for _ in 0..3 {
        arm_store_fixture(cpu);
        drive(cpu, bus);
    }
    assert!(
        cpu.jit_direct.len() > 0,
        "direct store block did not compile: tracked={} perf={:?}",
        cpu.jit_direct.tracked_len(),
        cpu.perf_counters()
    );
}

fn store_address_program() -> Vec<u8> {
    let mut memory = vec![0; 0x6000];
    memory[(STORE_ENTRY - 1) as usize] = 0x90;
    let code = [
        0xb8, 0x44, 0x33, 0x22, 0x11, // mov eax,0x11223344
        0x89, 0x04, 0x24, // mov [esp],eax
        0x88, 0x5c, 0x4c, 0x7f, // mov [esp+ecx*2+0x7f],bl
        0x89, 0x94, 0xb4, 0x00, 0x01, 0x00, 0x00, // mov [esp+esi*4+0x100],edx
        0xa3, 0x00, 0x32, 0x00, 0x00, // mov [0x3200],eax
        0x89, 0x1c, 0xf5, 0x00, 0x33, 0x00, 0x00, // mov [esi*8+0x3300],ebx
        0xf4,
    ];
    memory[STORE_ENTRY as usize..STORE_ENTRY as usize + code.len()].copy_from_slice(&code);
    memory
}

#[test]
fn direct_ram_stores_cover_esp_sib_scales_displacements_and_disp_only() {
    let memory = store_address_program();
    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;
    native_bus.direct_page_clocks = true;

    arm_store_fixture(&mut interp);
    drive(&mut interp, &mut interp_bus);
    prime_direct_store_block(&mut native, &mut native_bus);
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.memory[0x3000..0x3500].fill(0);
        bus.trace = BusTrace::default();
    }
    for cpu in [&mut interp, &mut native] {
        arm_store_fixture(cpu);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    let native_before = native.perf_counters().jit_native_store_hits;

    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(native_outcomes, interp_outcomes);
    assert_eq!(native, interp);
    assert_eq!(native_bus.memory, interp_bus.memory);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert_eq!(
        &native_bus.memory[0x3000..0x3004],
        &0x1122_3344u32.to_le_bytes()
    );
    assert_eq!(native_bus.memory[0x309f], 0xd4);
    assert_eq!(
        &native_bus.memory[0x3180..0x3184],
        &0x5566_7788u32.to_le_bytes()
    );
    assert_eq!(
        &native_bus.memory[0x3200..0x3204],
        &0x1122_3344u32.to_le_bytes()
    );
    assert_eq!(
        &native_bus.memory[0x3400..0x3404],
        &0xa1b2_c3d4u32.to_le_bytes()
    );
    assert_eq!(
        native.perf_counters().jit_native_store_hits - native_before,
        4,
        "{:?}",
        native.perf_counters()
    );
}

fn immediate_store_program() -> Vec<u8> {
    let mut memory = vec![0; 0x6000];
    memory[(STORE_ENTRY - 1) as usize] = 0x90;
    let code = [
        0xc6, 0x05, 0x00, 0x30, 0x00, 0x00, 0x5a, // mov byte [0x3000],0x5a
        0xc7, 0x05, 0x04, 0x30, 0x00, 0x00, 0x78, 0x56, 0x34,
        0x12, // mov dword [0x3004],0x12345678
        0xc6, 0xc4, 0xa5, // mov ah,0xa5
        0xc7, 0xc1, 0x44, 0x33, 0x22, 0x11, // mov ecx,0x11223344
        0x89, 0xca, // mov edx,ecx
        0xf4,
    ];
    memory[STORE_ENTRY as usize..STORE_ENTRY as usize + code.len()].copy_from_slice(&code);
    memory
}

#[test]
fn direct_c6_c7_register_and_ram_immediates_keep_state_and_timing() {
    let memory = immediate_store_program();
    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;
    native_bus.direct_page_clocks = true;

    arm_store_fixture(&mut interp);
    drive(&mut interp, &mut interp_bus);
    prime_direct_store_block(&mut native, &mut native_bus);
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.memory[0x3000..0x3008].fill(0);
        bus.trace = BusTrace::default();
    }
    for cpu in [&mut interp, &mut native] {
        arm_store_fixture(cpu);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    let stores = native.perf_counters().jit_native_store_hits;
    let exits = native.perf_counters().jit_direct_side_exits;
    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(native_outcomes, interp_outcomes);
    assert_eq!(native, interp);
    assert_eq!(native_bus.memory, interp_bus.memory);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert_eq!(native_bus.memory[0x3000], 0x5a);
    assert_eq!(
        &native_bus.memory[0x3004..0x3008],
        &0x1234_5678u32.to_le_bytes()
    );
    assert_eq!(native.registers.eax(), 0x0000_a500);
    assert_eq!(native.registers.ecx(), 0x1122_3344);
    assert_eq!(native.registers.edx(), 0x1122_3344);
    assert_eq!(native.perf_counters().jit_native_store_hits - stores, 2);
    assert_eq!(native.perf_counters().jit_direct_side_exits - exits, 0);
}

fn immediate_store_exit_program(target: u32) -> Vec<u8> {
    let mut memory = vec![0; 0x7000];
    memory[(STORE_ENTRY - 1) as usize] = 0x90;
    let mut code = vec![
        0xc7, 0x05, 0x00, 0x30, 0x00, 0x00, 0x04, 0x03, 0x02, 0x01, // safe prefix store
        0xc6, 0x05,
    ];
    code.extend_from_slice(&target.to_le_bytes());
    code.extend_from_slice(&[0x5a, 0x89, 0xc2, 0xf4]);
    memory[STORE_ENTRY as usize..STORE_ENTRY as usize + code.len()].copy_from_slice(&code);
    memory
}

#[test]
fn immediate_ram_store_watch_and_map_miss_side_exits_are_precise() {
    const TARGET: u32 = 0x4100;
    for (name, initial, watched, map_miss, expected_writes, expected_reason) in [
        ("same value", 0x5a, true, false, 2, None),
        ("changed code", 0x00, true, false, 1, Some(true)),
        ("map miss", 0x00, false, true, 1, Some(false)),
    ] {
        let memory = immediate_store_exit_program(TARGET);
        let mut interp = fresh();
        let mut native = fresh();
        let mut interp_bus = TestBus::with_memory(memory.clone());
        let mut native_bus = TestBus::with_memory(memory);
        interp_bus.direct_pages_enabled = true;
        native_bus.direct_pages_enabled = true;
        interp_bus.direct_page_clocks = true;
        native_bus.direct_page_clocks = true;
        arm_store_fixture(&mut interp);
        drive(&mut interp, &mut interp_bus);
        prime_direct_store_block(&mut native, &mut native_bus);

        if watched {
            native.decode_cache.mark_code_range(TARGET, 1);
            if expected_reason == Some(true) {
                interp.decode_cache.mark_code_range(TARGET, 1);
            }
        }
        if map_miss {
            native.jit_fast_map.invalidate_page(TARGET);
            native.data_write_pages.invalidate();
            interp.data_write_pages.invalidate();
            native_bus.direct_write_denied_page = Some(TARGET & !0xfff);
            interp_bus.direct_write_denied_page = Some(TARGET & !0xfff);
        }
        for bus in [&mut interp_bus, &mut native_bus] {
            bus.memory[0x3000..0x3004].fill(0);
            bus.memory[TARGET as usize] = initial;
            bus.trace = BusTrace::default();
        }
        for cpu in [&mut interp, &mut native] {
            arm_store_fixture(cpu);
            cpu.elapsed_clocks = 0;
            cpu.timing_rem = 0;
            cpu.core_clocks_so_far = 0;
        }
        let writes = native.perf_counters().jit_native_store_hits;
        let exits = native.perf_counters().jit_direct_side_exits;
        let code_watch = native.perf_counters().jit_direct_exit_code_watch;
        let unavailable = native.perf_counters().jit_direct_exit_unavailable_or_kind;
        let interp_outcomes = drive(&mut interp, &mut interp_bus);
        let native_outcomes = drive(&mut native, &mut native_bus);

        if expected_reason.is_none() {
            assert_eq!(native_outcomes, interp_outcomes, "{name}");
        } else {
            assert_eq!(
                native_outcomes
                    .iter()
                    .map(|(clocks, _, _)| u64::from(*clocks))
                    .sum::<u64>(),
                interp_outcomes
                    .iter()
                    .map(|(clocks, _, _)| u64::from(*clocks))
                    .sum::<u64>(),
                "{name} aggregate raw clocks"
            );
            assert!(
                native_outcomes.last().is_some_and(|entry| entry.2),
                "{name}"
            );
        }
        assert_eq!(native, interp, "{name}");
        assert_eq!(native_bus.memory, interp_bus.memory, "{name}");
        assert_eq!(
            native_bus.trace.elapsed_clocks(),
            interp_bus.trace.elapsed_clocks(),
            "{name}"
        );
        assert_eq!(native_bus.memory[TARGET as usize], 0x5a, "{name}");
        assert_eq!(
            native.perf_counters().jit_native_store_hits - writes,
            expected_writes,
            "{name}"
        );
        assert_eq!(
            native.perf_counters().jit_direct_side_exits - exits,
            u64::from(expected_reason.is_some()),
            "{name}"
        );
        assert_eq!(
            native.perf_counters().jit_direct_exit_code_watch - code_watch,
            u64::from(expected_reason == Some(true)),
            "{name}"
        );
        assert_eq!(
            native.perf_counters().jit_direct_exit_unavailable_or_kind - unavailable,
            u64::from(expected_reason == Some(false)),
            "{name}"
        );
    }
}

const WATCHED_TARGET: usize = 0x1180;

fn watched_store_program() -> Vec<u8> {
    let mut memory = vec![0; 0x3000];
    memory[(STORE_ENTRY - 1) as usize] = 0x90;
    let code = [
        0xb8, 0x78, 0x56, 0x34, 0x12, // mov eax,0x12345678
        0xa3, 0x80, 0x11, 0x00, 0x00, // mov [0x1180],eax
        0x89, 0xc2, // mov edx,eax
        0xf4,
    ];
    memory[STORE_ENTRY as usize..STORE_ENTRY as usize + code.len()].copy_from_slice(&code);
    memory
}

fn ready_block(cpu: &mut CpuGsw, entry: u32) -> jit::direct::CompiledBlock {
    let key = jit::direct::key_for(cpu, entry, true).expect("direct block key");
    let jit::direct::BlockProbe::Ready(id) = cpu.jit_direct.probe(key) else {
        panic!("direct block at {entry:#x} is not ready");
    };
    cpu.jit_direct.block(id).expect("direct block is live")
}

fn arm_prefetch_for_page(cpu: &mut CpuGsw, physical: u32) {
    cpu.begin_instruction();
    cpu.prefetch.bytes.fill(0x90);
    cpu.prefetch.cs = cpu.registers.cs();
    cpu.prefetch.linear_base = physical;
    cpu.prefetch.physical_base = physical;
    cpu.prefetch.len = PREFETCH_WINDOW_BYTES as u8;
    cpu.fetch_page.invalidate();
}

#[test]
fn ranged_device_write_preserves_unrelated_decode_and_prefetch_state() {
    let mut cpu = fresh();
    let mut memory = vec![0; 0x5000];
    memory[0x1000] = 0x90;
    memory[0x1020] = 0x90;
    let mut bus = TestBus::with_memory(memory);
    for linear in [0x1000, 0x1020] {
        cpu.registers.eip = linear;
        cpu.begin_instruction();
        cpu.fetch_decoded(&mut bus, linear).expect("decode NOP");
    }
    let generation = cpu.decode_cache_generation();
    arm_prefetch_for_page(&mut cpu, 0x3000);

    cpu.note_device_memory_write_range(0x3020, 16);
    assert_eq!(cpu.prefetch.len, PREFETCH_WINDOW_BYTES as u8);
    assert!(cpu.decode_cache.get(0x1000, true).is_some());
    assert!(cpu.decode_cache.get(0x1020, true).is_some());

    cpu.note_device_memory_write_range(0x1000, 1);
    assert_eq!(cpu.decode_cache_generation(), generation);
    assert!(cpu.decode_cache.get(0x1000, true).is_none());
    assert!(cpu.decode_cache.get(0x1020, true).is_some());
    assert_eq!(cpu.prefetch.len, PREFETCH_WINDOW_BYTES as u8);

    cpu.note_device_memory_write_range(0x3004, 1);
    assert_eq!(cpu.prefetch.len, 0);
    assert!(cpu.decode_cache.get(0x1020, true).is_some());
    assert_eq!(cpu.perf_counters().device_write_ranges, 3);
    assert_eq!(cpu.perf_counters().device_write_code_hits, 2);
    assert_eq!(cpu.perf_counters().device_write_coarse_resets, 0);
}

#[test]
fn native_store_records_a_prefetched_but_undecoded_page() {
    let mut cpu = fresh();
    let mut bus = TestBus::with_memory(watched_store_program());
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    prime_direct_store_block(&mut cpu, &mut bus);
    let block = ready_block(&mut cpu, STORE_ENTRY);
    let target = WATCHED_TARGET as u32;
    assert!(!cpu.decode_cache.range_hits_code(target, 4));
    assert!(!cpu.jit_direct.range_hits_compiled_code(target, 4));

    bus.memory[WATCHED_TARGET..WATCHED_TARGET + 4].fill(0);
    arm_store_fixture(&mut cpu);
    cpu.registers.eip = STORE_ENTRY;
    arm_prefetch_for_page(&mut cpu, target);
    let native_stores = cpu.perf_counters().jit_native_store_hits;

    assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    assert_eq!(cpu.perf_counters().jit_native_store_hits - native_stores, 1);
    assert_eq!(
        &bus.memory[WATCHED_TARGET..WATCHED_TARGET + 4],
        &0x1234_5678u32.to_le_bytes()
    );
    assert!(cpu.written_pages.contains(&Some(target >> 12)));
    assert_ne!(
        cpu.prefetch.len, 0,
        "the snapshot lives until the next instruction"
    );

    cpu.begin_instruction();
    assert_eq!(cpu.prefetch.len, 0);
}

fn watched_rmw_program() -> Vec<u8> {
    let mut memory = vec![0; 0x3000];
    memory[(STORE_ENTRY - 1) as usize] = 0x90;
    let code = [
        0xb9, 0x01, 0x00, 0x00, 0x00, // mov ecx,1
        0xff, 0x05, 0x80, 0x11, 0x00, 0x00, // inc dword [0x1180]
        0x85, 0xc9, // test ecx,ecx
        0x75, 0x00, // jnz to the fallthrough HLT
        0xf4,
    ];
    memory[STORE_ENTRY as usize..STORE_ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[WATCHED_TARGET..WATCHED_TARGET + 4].copy_from_slice(&0xf3u32.to_le_bytes());
    memory
}

#[test]
fn native_rmw_records_a_prefetched_but_undecoded_page() {
    let mut cpu = fresh();
    let mut bus = TestBus::with_memory(watched_rmw_program());
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    prime_direct_store_block(&mut cpu, &mut bus);
    let block = ready_block(&mut cpu, STORE_ENTRY);
    let target = WATCHED_TARGET as u32;
    assert!(!cpu.decode_cache.range_hits_code(target, 4));
    assert!(!cpu.jit_direct.range_hits_compiled_code(target, 4));

    bus.memory[WATCHED_TARGET..WATCHED_TARGET + 4].copy_from_slice(&0xf3u32.to_le_bytes());
    arm_store_fixture(&mut cpu);
    cpu.registers.eip = STORE_ENTRY;
    arm_prefetch_for_page(&mut cpu, target);
    let native_stores = cpu.perf_counters().jit_native_store_hits;

    assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    assert_eq!(cpu.perf_counters().jit_native_store_hits - native_stores, 1);
    assert_eq!(
        &bus.memory[WATCHED_TARGET..WATCHED_TARGET + 4],
        &0xf4u32.to_le_bytes()
    );
    assert!(cpu.written_pages.contains(&Some(target >> 12)));

    cpu.begin_instruction();
    assert_eq!(cpu.prefetch.len, 0);
}

#[test]
fn invalidated_compiled_code_reused_as_data_stays_on_the_native_store_path() {
    let mut cpu = fresh();
    let mut bus = TestBus::with_memory(watched_store_program());
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    prime_direct_store_block(&mut cpu, &mut bus);

    let target = WATCHED_TARGET as u32;
    let old_code = [
        0xb8, 1, 0, 0, 0, // mov eax,1
        0x89, 0xc2, // mov edx,eax
        0x89, 0xc1, // mov ecx,eax
    ];
    bus.memory[WATCHED_TARGET..WATCHED_TARGET + old_code.len()].copy_from_slice(&old_code);
    let mut linear = target;
    for _ in 0..3 {
        cpu.registers.eip = linear;
        cpu.begin_instruction();
        let insn = cpu.fetch_decoded(&mut bus, linear).unwrap();
        linear = linear.wrapping_add(u32::from(insn.len));
    }
    let key = jit::direct::key_for(&cpu, target, true).expect("old code key");
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Compile
    ));
    let compilation = jit::direct::compile(&mut cpu, target, true).expect("old code block");
    cpu.jit_direct
        .install(&compilation)
        .expect("old code installs");
    assert!(cpu.jit_direct.range_hits_compiled_code(target, 1));

    assert_eq!(cpu.jit_direct.invalidate_physical_range(target, 1), 1);
    assert!(!cpu.jit_direct.range_hits_compiled_code(target, 1));
    assert!(
        cpu.decode_cache.range_hits_code(target, 1),
        "the interpreter watch has independent lifetime"
    );
    cpu.decode_cache.invalidate_and_clear_code_marks();

    bus.memory[WATCHED_TARGET..WATCHED_TARGET + old_code.len()].fill(0);
    prime_direct_store_block(&mut cpu, &mut bus);
    assert!(!cpu.decode_cache.range_hits_code(target, 1));
    assert!(!cpu.jit_direct.range_hits_compiled_code(target, 1));
    bus.memory[WATCHED_TARGET..WATCHED_TARGET + 4].fill(0);
    arm_store_fixture(&mut cpu);
    let native_stores = cpu.perf_counters().jit_native_store_hits;
    let watch_exits = cpu.perf_counters().jit_direct_exit_code_watch;

    drive(&mut cpu, &mut bus);

    assert_eq!(cpu.perf_counters().jit_native_store_hits - native_stores, 1);
    assert_eq!(
        cpu.perf_counters().jit_direct_exit_code_watch - watch_exits,
        0
    );
    assert_eq!(
        &bus.memory[WATCHED_TARGET..WATCHED_TARGET + 4],
        &0x1234_5678u32.to_le_bytes()
    );
}

#[test]
fn adjacent_data_store_outside_watched_chunks_stays_native() {
    for same_value in [false, true] {
        let memory = watched_store_program();
        let mut interp = fresh();
        let mut native = fresh();
        let mut interp_bus = TestBus::with_memory(memory.clone());
        let mut native_bus = TestBus::with_memory(memory);
        interp_bus.direct_pages_enabled = true;
        native_bus.direct_pages_enabled = true;
        interp_bus.direct_page_clocks = true;
        native_bus.direct_page_clocks = true;
        arm_store_fixture(&mut interp);
        drive(&mut interp, &mut interp_bus);
        prime_direct_store_block(&mut native, &mut native_bus);

        let initial: u32 = if same_value { 0x1234_5678 } else { 0 };
        for bus in [&mut interp_bus, &mut native_bus] {
            bus.memory[WATCHED_TARGET..WATCHED_TARGET + 4].copy_from_slice(&initial.to_le_bytes());
            bus.trace = BusTrace::default();
        }
        for cpu in [&mut interp, &mut native] {
            arm_store_fixture(cpu);
            cpu.elapsed_clocks = 0;
            cpu.timing_rem = 0;
            cpu.core_clocks_so_far = 0;
        }
        let native_stores = native.perf_counters().jit_native_store_hits;
        let helper_exits = native.perf_counters().jit_direct_side_exits;
        let cached_blocks = native.jit_direct.len();

        let interp_outcomes = drive(&mut interp, &mut interp_bus);
        let native_outcomes = drive(&mut native, &mut native_bus);

        assert_eq!(native_outcomes, interp_outcomes, "same_value={same_value}");
        assert_eq!(native, interp, "same_value={same_value}");
        assert_eq!(
            native_bus.memory, interp_bus.memory,
            "same_value={same_value}"
        );
        assert_eq!(
            native_bus.trace.elapsed_clocks(),
            interp_bus.trace.elapsed_clocks(),
            "same_value={same_value}"
        );
        assert_eq!(
            native.perf_counters().jit_native_store_hits - native_stores,
            1,
            "same_value={same_value}"
        );
        assert_eq!(
            native.perf_counters().jit_direct_side_exits - helper_exits,
            0,
            "same_value={same_value}"
        );
        assert_eq!(native.jit_direct.len(), cached_blocks);
    }
}

fn store_exit_program(target: u32) -> Vec<u8> {
    let mut memory = vec![0; 0x7000];
    memory[(STORE_ENTRY - 1) as usize] = 0x90;
    let mut code = vec![
        0xb8, 0x78, 0x56, 0x34, 0x12, // mov eax,0x12345678
        0xa3, 0x00, 0x30, 0x00, 0x00, // safe prefix store
        0xa3,
    ];
    code.extend_from_slice(&target.to_le_bytes());
    code.extend_from_slice(&[0x89, 0xc2, 0xf4]);
    memory[STORE_ENTRY as usize..STORE_ENTRY as usize + code.len()].copy_from_slice(&code);
    memory
}

#[test]
fn native_store_watch_covers_overlap_cross_chunk_and_same_value_cases() {
    for (target, marked, same_value, expected_stores, expected_exits) in [
        (0x4100u32, 0x4100u32, false, 1, 1),
        (0x410fu32, 0x4110u32, false, 1, 1),
        (0x4100u32, 0x4100u32, true, 2, 0),
    ] {
        let mut cpu = fresh();
        let mut bus = TestBus::with_memory(store_exit_program(target));
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        prime_direct_store_block(&mut cpu, &mut bus);
        let page = bus
            .direct_page(target, BusAccessKind::DataWrite)
            .unwrap()
            .unwrap();
        assert!(cpu.jit_fast_map.populate_write(
            target,
            target,
            page,
            jit::fast_map::PagePermissions::UNPAGED,
        ));
        cpu.decode_cache.mark_code_range(marked, 1);
        let initial = if same_value { 0x1234_5678u32 } else { 0 };
        bus.memory[target as usize..target as usize + 4].copy_from_slice(&initial.to_le_bytes());
        bus.trace = BusTrace::default();
        arm_store_fixture(&mut cpu);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
        let stores = cpu.perf_counters().jit_native_store_hits;
        let exits = cpu.perf_counters().jit_direct_side_exits;
        let code_watch_exits = cpu.perf_counters().jit_direct_exit_code_watch;
        let alignment_exits = cpu.perf_counters().jit_direct_exit_cross_page_or_alignment;
        let cached_blocks = cpu.jit_direct.len();

        drive(&mut cpu, &mut bus);

        assert_eq!(
            &bus.memory[target as usize..target as usize + 4],
            &0x1234_5678u32.to_le_bytes(),
            "target={target:#x} marked={marked:#x} same={same_value}"
        );
        assert_eq!(
            cpu.perf_counters().jit_native_store_hits - stores,
            expected_stores,
            "target={target:#x} marked={marked:#x} same={same_value}"
        );
        assert_eq!(
            cpu.perf_counters().jit_direct_side_exits - exits,
            expected_exits,
            "target={target:#x} marked={marked:#x} same={same_value}"
        );
        assert_eq!(
            cpu.perf_counters().jit_direct_exit_code_watch - code_watch_exits,
            if target & 3 == 0 { expected_exits } else { 0 },
            "target={target:#x} marked={marked:#x} same={same_value}: {:?}",
            cpu.perf_counters()
        );
        assert_eq!(
            cpu.perf_counters().jit_direct_exit_cross_page_or_alignment - alignment_exits,
            if target & 3 == 0 { 0 } else { expected_exits },
        );
        assert_eq!(cpu.jit_direct.len(), cached_blocks);
    }
}

fn watched_byte_store_program(target: u32) -> Vec<u8> {
    let mut memory = vec![0; 0x7000];
    memory[(STORE_ENTRY - 1) as usize] = 0x90;
    let mut code = vec![
        0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax,1
        0x88, 0x1d, // mov [target],bl
    ];
    code.extend_from_slice(&target.to_le_bytes());
    code.extend_from_slice(&[0x89, 0xc2, 0xf4]); // mov edx,eax; hlt
    memory[STORE_ENTRY as usize..STORE_ENTRY as usize + code.len()].copy_from_slice(&code);
    memory
}

#[test]
fn native_byte_store_watch_checks_chunk_and_same_value() {
    for (marked, initial, expected_writes, expected_exits) in [
        (0x4100u32, 0u8, 0, 1),
        (0x4100u32, 0xd4u8, 1, 0),
        (0x4110u32, 0u8, 1, 0),
    ] {
        let target = 0x4100u32;
        let mut cpu = fresh();
        let mut bus = TestBus::with_memory(watched_byte_store_program(target));
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        prime_direct_store_block(&mut cpu, &mut bus);
        cpu.decode_cache.mark_code_range(marked, 1);
        bus.memory[target as usize] = initial;
        bus.trace = BusTrace::default();
        arm_store_fixture(&mut cpu);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
        let writes = cpu.perf_counters().jit_native_store_hits;
        let exits = cpu.perf_counters().jit_direct_side_exits;

        drive(&mut cpu, &mut bus);

        assert_eq!(bus.memory[target as usize], 0xd4, "marked={marked:#x}");
        assert_eq!(
            cpu.perf_counters().jit_native_store_hits - writes,
            expected_writes,
            "marked={marked:#x} initial={initial:#x}"
        );
        assert_eq!(
            cpu.perf_counters().jit_direct_side_exits - exits,
            expected_exits,
            "marked={marked:#x} initial={initial:#x}"
        );
    }
}

#[test]
fn cross_page_and_unavailable_write_bias_exit_before_the_faulting_store() {
    for (target, disable_write_page) in [(0x3fffu32, false), (0x4000, true)] {
        let memory = store_exit_program(target);
        let mut interp = fresh();
        let mut native = fresh();
        let mut interp_bus = TestBus::with_memory(memory.clone());
        let mut native_bus = TestBus::with_memory(memory);
        interp_bus.direct_pages_enabled = true;
        native_bus.direct_pages_enabled = true;
        interp_bus.direct_page_clocks = true;
        native_bus.direct_page_clocks = true;
        arm_store_fixture(&mut interp);
        drive(&mut interp, &mut interp_bus);
        prime_direct_store_block(&mut native, &mut native_bus);

        if disable_write_page {
            native.jit_fast_map.invalidate_page(target);
            interp.jit_fast_map.invalidate_page(target);
            native.data_write_pages.invalidate();
            interp.data_write_pages.invalidate();
            native_bus.direct_write_denied_page = Some(target & !0xfff);
            interp_bus.direct_write_denied_page = Some(target & !0xfff);
        }
        for bus in [&mut interp_bus, &mut native_bus] {
            bus.memory[0x3000..0x5000].fill(0);
            bus.trace = BusTrace::default();
        }
        for cpu in [&mut interp, &mut native] {
            arm_store_fixture(cpu);
            cpu.elapsed_clocks = 0;
            cpu.timing_rem = 0;
            cpu.core_clocks_so_far = 0;
        }
        let native_stores = native.perf_counters().jit_native_store_hits;
        let helper_exits = native.perf_counters().jit_direct_side_exits;

        let interp_outcomes = drive(&mut interp, &mut interp_bus);
        let native_outcomes = drive(&mut native, &mut native_bus);

        assert_eq!(native_outcomes, interp_outcomes, "target={target:#x}");
        assert_eq!(native, interp, "target={target:#x}");
        assert_eq!(native_bus.memory, interp_bus.memory, "target={target:#x}");
        assert_eq!(
            native_bus.trace.elapsed_clocks(),
            interp_bus.trace.elapsed_clocks(),
            "target={target:#x}"
        );
        assert_eq!(
            native.perf_counters().jit_native_store_hits - native_stores,
            1,
            "target={target:#x}"
        );
        assert_eq!(
            native.perf_counters().jit_direct_side_exits - helper_exits,
            1,
            "target={target:#x}"
        );
    }
}

fn mode13_store_program() -> Vec<u8> {
    let mut memory = vec![0; 0x000b_1000];
    memory[(STORE_ENTRY - 1) as usize] = 0x90;
    let code = [
        0xb8, 0x44, 0x33, 0x22, 0x11, // mov eax,0x11223344
        0xa3, 0x00, 0x00, 0x0a, 0x00, // mov [0xa0000],eax
        0x88, 0x1d, 0x23, 0xf1, 0x0a, 0x00, // mov [0xaf123],bl
        0x89, 0xc2, // mov edx,eax
        0xf4,
    ];
    memory[STORE_ENTRY as usize..STORE_ENTRY as usize + code.len()].copy_from_slice(&code);
    memory
}

#[test]
fn direct_mode13_stores_return_exact_physical_dirty_mask_and_video_timing() {
    let memory = mode13_store_program();
    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;
    native_bus.direct_page_clocks = true;
    arm_store_fixture(&mut interp);
    drive(&mut interp, &mut interp_bus);
    prime_direct_store_block(&mut native, &mut native_bus);

    for bus in [&mut interp_bus, &mut native_bus] {
        bus.memory[0x000a_0000..0x000b_0000].fill(0);
        bus.trace = BusTrace::default();
        bus.mode13_dirty_pages = 0;
        bus.mode13_byte_writes = 0;
        bus.mode13_dword_writes = 0;
    }
    for cpu in [&mut interp, &mut native] {
        arm_store_fixture(cpu);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    let native_stores = native.perf_counters().jit_native_store_hits;

    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(native_outcomes, interp_outcomes);
    assert_eq!(native, interp);
    assert_eq!(native_bus.memory, interp_bus.memory);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert_eq!(native_bus.mode13_dirty_pages, 0x8001);
    assert_eq!(native_bus.mode13_dirty_pages, interp_bus.mode13_dirty_pages);
    assert_eq!(native_bus.mode13_byte_writes, 1);
    assert_eq!(native_bus.mode13_dword_writes, 1);
    assert_eq!(native_bus.mode13_byte_writes, interp_bus.mode13_byte_writes);
    assert_eq!(
        native_bus.mode13_dword_writes,
        interp_bus.mode13_dword_writes
    );
    assert_eq!(
        native.perf_counters().jit_native_store_hits - native_stores,
        2
    );
}

#[test]
fn direct_mode13_reads_match_values_and_video_bus_timing() {
    let mut memory = vec![0; 0x000b_0000];
    memory[0x100] = 0x90;
    memory[0x101..0x111].copy_from_slice(&[
        0xa0, 0x01, 0x00, 0x0a, 0x00, // mov al,[0xa0001]
        0x8b, 0x1d, 0x04, 0x00, 0x0a, 0x00, // mov ebx,[0xa0004]
        0x89, 0xc1, // mov ecx,eax
        0x89, 0xda, // mov edx,ebx
        0xf4,
    ]);
    memory[0x000a_0001] = 0x5a;
    memory[0x000a_0004..0x000a_0008].copy_from_slice(&0x1234_5678u32.to_le_bytes());
    let mut interp = fresh();
    let mut native = fresh();
    make_data_segments_flat(&mut interp);
    make_data_segments_flat(&mut native);
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;
    native_bus.direct_page_clocks = true;
    drive(&mut interp, &mut interp_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..3 {
        native.halted = false;
        native.registers.eip = 0x100;
        drive(&mut native, &mut native_bus);
    }
    for cpu in [&mut interp, &mut native] {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.registers.gpr.fill(0);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    interp_bus.trace = BusTrace::default();
    native_bus.trace = BusTrace::default();
    let loads = native.perf_counters().jit_native_load_hits;

    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(
        native_outcomes
            .iter()
            .map(|outcome| u64::from(outcome.0))
            .sum::<u64>(),
        interp_outcomes
            .iter()
            .map(|outcome| u64::from(outcome.0))
            .sum::<u64>(),
    );
    assert_eq!(native, interp);
    assert_eq!(native.registers.eax(), 0x5a);
    assert_eq!(native.registers.ebx(), 0x1234_5678);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert_eq!(native.perf_counters().jit_native_load_hits - loads, 2);
}

fn deadline_program() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[0x100..0x113].copy_from_slice(&[
        0x90, // nop starter
        0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax,1
        0x89, 0xc3, // mov ebx,eax
        0x89, 0xc1, // mov ecx,eax
        0x89, 0xc2, // mov edx,eax
        0x89, 0xc6, // mov esi,eax: reaches cap=1
        0x89, 0xc7, // mov edi,eax: zero-scaled suffix
        0x89, 0xc5, // mov ebp,eax: zero-scaled suffix
        0xf4,
    ]);
    memory
}

#[test]
fn direct_block_equal_to_deadline_falls_back_before_zero_scaled_suffix() {
    let memory = deadline_program();
    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..3 {
        native.halted = false;
        native.registers.eip = 0x100;
        drive(&mut native, &mut native_bus);
    }

    for cpu in [&mut interp, &mut native] {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.registers.gpr.fill(0);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.trace = BusTrace::default();
    }
    let direct_entries = native.perf_counters().jit_direct_entries;

    let interp_outcome = interp.run_straight_line(&mut interp_bus, 1).unwrap();
    let native_outcome = native.run_straight_line(&mut native_bus, 1).unwrap();

    assert_eq!(native_outcome, interp_outcome);
    assert_eq!(native, interp);
    assert_eq!(native.registers.eip, 0x10e);
    assert_eq!(native.registers.edi(), 0);
    assert_eq!(native.registers.ebp(), 0);
    assert_eq!(native.perf_counters().jit_direct_entries, direct_entries);
}

const GAME_LOOP_ENTRY: u32 = 0x101;

fn make_data_segments_flat(cpu: &mut CpuGsw) {
    for segment in [
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
        SegmentIndex::Fs,
        SegmentIndex::Gs,
    ] {
        let mut descriptor = cpu.registers.segment(segment);
        descriptor.base = 0;
        descriptor.limit = u32::MAX;
        cpu.registers.set_segment(segment, descriptor);
    }
}

fn quake_loop_program() -> Vec<u8> {
    let mut memory = vec![0; 0x000e_0000];
    memory[0x100] = 0x90;
    let code = [
        0xA1, 0xA0, 0xA7, 0x0D, 0x00, // mov eax,[0xda7a0]
        0x03, 0x04, 0x32, // add eax,[edx+esi]
        0xA3, 0xA0, 0xA7, 0x0D, 0x00, // mov [0xda7a0],eax
        0x03, 0x84, 0x16, 0x00, 0x00, 0x01, 0x00, // add eax,[esi+edx+0x10000]
        0xA3, 0xA0, 0xA7, 0x0D, 0x00, // mov [0xda7a0],eax
        0x83, 0xC2, 0x04, // add edx,4
        0x39, 0xFA, // cmp edx,edi
        0x7C, 0xE0, // jl 0x101
    ];
    assert_eq!(code.len(), 32);
    memory[GAME_LOOP_ENTRY as usize..GAME_LOOP_ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[GAME_LOOP_ENTRY as usize + code.len()] = 0xF4;
    memory[0x000d_a7a0..0x000d_a7a4].copy_from_slice(&5u32.to_le_bytes());
    for i in 0..4usize {
        memory[0x0002_0000 + i * 4..0x0002_0004 + i * 4]
            .copy_from_slice(&(i as u32 + 1).to_le_bytes());
        memory[0x0003_0000 + i * 4..0x0003_0004 + i * 4]
            .copy_from_slice(&(i as u32 + 10).to_le_bytes());
    }
    memory
}

fn arm_quake_loop(cpu: &mut CpuGsw) {
    cpu.halted = false;
    cpu.registers.eip = 0x100;
    cpu.registers.gpr.fill(0);
    cpu.registers.set_edx(0);
    cpu.registers.set_esi(0x0002_0000);
    cpu.registers.set_edi(16);
    cpu.registers.eflags = 0x203;
    cpu.pending_flags = PendingFlags::default();
    make_data_segments_flat(cpu);
}

#[test]
fn direct_quake_loop_runs_four_iterations_with_memory_source_alu() {
    let initial = quake_loop_program();
    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(initial.clone());
    let mut native_bus = TestBus::with_memory(initial.clone());
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;
    native_bus.direct_page_clocks = true;

    arm_quake_loop(&mut interp);
    drive(&mut interp, &mut interp_bus);
    native.set_jit_auto_admit(true);
    arm_quake_loop(&mut native);
    drive(&mut native, &mut native_bus);
    for _ in 0..2 {
        arm_quake_loop(&mut native);
        drive(&mut native, &mut native_bus);
    }
    assert!(native.jit_direct.len() > 0, "Quake block did not compile");
    let root = jit::direct::compile(&mut native, GAME_LOOP_ENTRY, true)
        .expect("full Quake loop must remain directly compilable");
    assert!(
        root.code.len() <= 3_500,
        "full Quake loop emitted {} bytes",
        root.code.len()
    );

    for bus in [&mut interp_bus, &mut native_bus] {
        bus.memory.copy_from_slice(&initial);
        bus.trace = BusTrace::default();
    }
    for cpu in [&mut interp, &mut native] {
        arm_quake_loop(cpu);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    let direct_insns = native.perf_counters().jit_direct_insns;
    let direct_entries = native.perf_counters().jit_direct_entries;
    let direct_exits = native.perf_counters().jit_direct_side_exits;

    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(native_outcomes, interp_outcomes);
    assert_eq!(native, interp);
    assert_eq!(native.pending_flags, interp.pending_flags);
    assert_eq!(native.eflags(), interp.eflags());
    assert_eq!(native_bus.memory, interp_bus.memory);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert_eq!(
        u32::from_le_bytes(
            native_bus.memory[0x000d_a7a0..0x000d_a7a4]
                .try_into()
                .unwrap()
        ),
        61
    );
    assert_eq!(native.perf_counters().jit_direct_insns - direct_insns, 32);
    assert_eq!(
        native.perf_counters().jit_direct_entries - direct_entries,
        1
    );
    assert_eq!(
        native.perf_counters().jit_direct_side_exits - direct_exits,
        0
    );
}

const DOOM_COUNTER: usize = 0x4000;

fn doom_drawcolumn_program() -> Vec<u8> {
    let mut memory = vec![0; 0x0002_0000];
    memory[0x100] = 0x90;
    let code = [
        0x8B, 0xCD, // mov ecx,ebp
        0x81, 0xC5, 0x00, 0x7C, 0x33, 0x01, // add ebp,0x01337c00
        0x88, 0x07, // mov [edi],al
        0xC1, 0xE9, 0x19, // shr ecx,25
        0x8B, 0xD5, // mov edx,ebp
        0x81, 0xC5, 0x00, 0x7C, 0x33, 0x01, // add ebp,0x01337c00
        0x88, 0x5F, 0x50, // mov [edi+0x50],bl
        0xC1, 0xEA, 0x19, // shr edx,25
        0x8A, 0x04, 0x0E, // mov al,[esi+ecx]
        0x81, 0xC7, 0xA0, 0x00, 0x00, 0x00, // add edi,0xa0
        0x8A, 0x1C, 0x16, // mov bl,[esi+edx]
        0xFF, 0x0D, 0x00, 0x40, 0x00, 0x00, // dec dword [0x4000]
        0x8A, 0x00, // mov al,[eax]
        0x8A, 0x1B, // mov bl,[ebx]
        0x75, 0xCD, // jnz 0x101
    ];
    assert_eq!(code.len(), 51);
    memory[GAME_LOOP_ENTRY as usize..GAME_LOOP_ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[GAME_LOOP_ENTRY as usize + code.len()] = 0xF4;
    memory[DOOM_COUNTER..DOOM_COUNTER + 4].copy_from_slice(&4u32.to_le_bytes());
    for i in 0..256usize {
        memory[0x8000 + i] = i.wrapping_mul(37).wrapping_add(11) as u8;
        memory[0x10000 + i] = (i as u8) ^ 0x5A;
        memory[0x11000 + i] = (i as u8).wrapping_add(0x31);
    }
    memory
}

fn doom_drawcolumn_program_with_counter(target: u32) -> Vec<u8> {
    let mut memory = doom_drawcolumn_program();
    let immediate = GAME_LOOP_ENTRY as usize + 41;
    assert_eq!(&memory[immediate - 2..immediate], &[0xff, 0x0d]);
    memory[immediate..immediate + 4].copy_from_slice(&target.to_le_bytes());
    memory[target as usize..target as usize + 4].copy_from_slice(&4u32.to_le_bytes());
    memory
}

fn arm_doom_loop(cpu: &mut CpuGsw) {
    cpu.halted = false;
    cpu.registers.eip = 0x100;
    cpu.registers.gpr.fill(0);
    cpu.registers.set_eax(0x0001_0001);
    cpu.registers.set_ebx(0x0001_1002);
    cpu.registers.set_esi(0x8000);
    cpu.registers.set_edi(0x6000);
    cpu.registers.set_ebp(0);
    cpu.registers.eflags = 0x203;
    cpu.pending_flags = PendingFlags::default();
    make_data_segments_flat(cpu);
}

#[test]
fn native_rmw_watch_checks_overlap_and_both_touched_chunks() {
    for (target, marked) in [(0x4100u32, 0x4100u32), (0x410fu32, 0x4110u32)] {
        let mut cpu = fresh();
        let mut bus = TestBus::with_memory(doom_drawcolumn_program_with_counter(target));
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        cpu.set_jit_auto_admit(true);
        for _ in 0..3 {
            bus.memory[target as usize..target as usize + 4].copy_from_slice(&4u32.to_le_bytes());
            arm_doom_loop(&mut cpu);
            drive(&mut cpu, &mut bus);
        }
        assert!(cpu.jit_direct.len() > 0);

        cpu.decode_cache.mark_code_range(marked, 1);
        bus.memory[target as usize..target as usize + 4].copy_from_slice(&1u32.to_le_bytes());
        bus.trace = BusTrace::default();
        arm_doom_loop(&mut cpu);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
        let exits = cpu.perf_counters().jit_direct_side_exits;
        let cached_blocks = cpu.jit_direct.len();

        drive(&mut cpu, &mut bus);

        assert_eq!(
            &bus.memory[target as usize..target as usize + 4],
            &0u32.to_le_bytes(),
            "target={target:#x} marked={marked:#x}"
        );
        assert_eq!(
            cpu.perf_counters().jit_direct_side_exits - exits,
            1,
            "target={target:#x} marked={marked:#x}"
        );
        assert_eq!(cpu.jit_direct.len(), cached_blocks);
    }
}

#[test]
fn direct_doom_drawcolumn_runs_four_iterations_with_dec_rmw() {
    let initial = doom_drawcolumn_program();
    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(initial.clone());
    let mut native_bus = TestBus::with_memory(initial.clone());
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;
    native_bus.direct_page_clocks = true;

    arm_doom_loop(&mut interp);
    drive(&mut interp, &mut interp_bus);
    native.set_jit_auto_admit(true);
    arm_doom_loop(&mut native);
    drive(&mut native, &mut native_bus);
    for _ in 0..2 {
        native_bus.memory[DOOM_COUNTER..DOOM_COUNTER + 4].copy_from_slice(&4u32.to_le_bytes());
        arm_doom_loop(&mut native);
        drive(&mut native, &mut native_bus);
    }
    assert!(native.jit_direct.len() > 0, "Doom block did not compile");
    let root = jit::direct::compile(&mut native, GAME_LOOP_ENTRY, true)
        .expect("full Doom loop must remain directly compilable");
    assert!(
        root.code.len() <= 4_000,
        "full Doom loop emitted {} bytes",
        root.code.len()
    );

    for bus in [&mut interp_bus, &mut native_bus] {
        bus.memory.copy_from_slice(&initial);
        bus.trace = BusTrace::default();
    }
    for cpu in [&mut interp, &mut native] {
        arm_doom_loop(cpu);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    let direct_insns = native.perf_counters().jit_direct_insns;
    let direct_entries = native.perf_counters().jit_direct_entries;
    let direct_exits = native.perf_counters().jit_direct_side_exits;
    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(native_outcomes, interp_outcomes);
    assert_eq!(native, interp);
    assert_eq!(native.pending_flags, interp.pending_flags);
    assert_eq!(native.eflags(), interp.eflags());
    assert_eq!(native_bus.memory, interp_bus.memory);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert_eq!(
        u32::from_le_bytes(
            native_bus.memory[DOOM_COUNTER..DOOM_COUNTER + 4]
                .try_into()
                .unwrap()
        ),
        0
    );
    assert_eq!(native.perf_counters().jit_direct_insns - direct_insns, 60);
    assert_eq!(
        native.perf_counters().jit_direct_entries - direct_entries,
        1
    );
    assert_eq!(
        native.perf_counters().jit_direct_side_exits - direct_exits,
        0
    );
}

#[test]
fn direct_self_loop_quota_stops_only_after_complete_iterations() {
    let mut memory = loop_program();
    memory[0x101..0x105].copy_from_slice(&1_000u32.to_le_bytes());
    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;

    drive(&mut interp, &mut interp_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..3 {
        native.halted = false;
        native.registers.eip = 0x100;
        native.registers.set_eax(0);
        native.registers.set_edx(0);
        drive(&mut native, &mut native_bus);
    }
    assert!(native.jit_direct.len() > 0);

    for cpu in [&mut interp, &mut native] {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.registers.gpr.fill(0);
        cpu.registers.eflags = 0x203;
        cpu.pending_flags = PendingFlags::default();
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    interp_bus.trace = BusTrace::default();
    native_bus.trace = BusTrace::default();
    let direct_insns = native.perf_counters().jit_direct_insns;
    let direct_entries = native.perf_counters().jit_direct_entries;

    let interp_outcome = interp.run_straight_line(&mut interp_bus, 25).unwrap();
    let native_outcome = native.run_straight_line(&mut native_bus, 25).unwrap();
    let chained_insns = native.perf_counters().jit_direct_insns - direct_insns;

    assert_eq!(native_outcome, interp_outcome);
    assert_eq!(native, interp);
    assert_eq!(native_bus.trace.cycles(), interp_bus.trace.cycles());
    assert!(chained_insns >= 4);
    assert_eq!(chained_insns % 4, 0, "a quota exit split a loop iteration");
    assert!(native.perf_counters().jit_direct_entries > direct_entries);
}

#[test]
fn direct_large_self_loop_keeps_the_generic_fetch_fallback_exact() {
    const ITERATIONS: u32 = 1_000;
    let mut memory = loop_program();
    memory[0x101..0x105].copy_from_slice(&ITERATIONS.to_le_bytes());
    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;

    drive(&mut interp, &mut interp_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..3 {
        native.halted = false;
        native.registers.eip = 0x100;
        native.registers.set_eax(0);
        native.registers.set_edx(0);
        drive(&mut native, &mut native_bus);
    }
    assert!(native.jit_direct.len() > 0);

    for cpu in [&mut interp, &mut native] {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.registers.gpr.fill(0);
        cpu.registers.eflags = 0x203;
        cpu.pending_flags = PendingFlags::default();
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    interp_bus.trace = BusTrace::default();
    native_bus.trace = BusTrace::default();
    assert!(!native_bus.charge_native_cached_fetches(0x100, 0x100, &[1], 4_000));
    assert_eq!(native_bus.trace.elapsed_clocks(), 0);
    let direct_insns = native.perf_counters().jit_direct_insns;
    let direct_entries = native.perf_counters().jit_direct_entries;

    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(native_outcomes, interp_outcomes);
    assert_eq!(native, interp);
    assert_eq!(native_bus.trace.cycles(), interp_bus.trace.cycles());
    assert_eq!(native.registers.eax(), ITERATIONS * 3);
    assert_eq!(
        native.perf_counters().jit_direct_insns - direct_insns,
        u64::from(ITERATIONS) * 4
    );
    assert_eq!(
        native.perf_counters().jit_direct_entries - direct_entries,
        1
    );
}

fn later_store_exit_program() -> Vec<u8> {
    let mut memory = vec![0; 0x3000];
    memory[(STORE_ENTRY - 1) as usize] = 0x90;
    memory[STORE_ENTRY as usize..STORE_ENTRY as usize + 10].copy_from_slice(&[
        0x88, 0x03, // mov [ebx],al
        0x83, 0xc3, 0x01, // add ebx,1
        0x39, 0xfb, // cmp ebx,edi
        0x7c, 0xf7, // jl 0x1101
        0xf4,
    ]);
    memory[WATCHED_TARGET] = 0x88;
    memory
}

const LATER_STORE_TARGET: usize = STORE_ENTRY as usize - 2;

fn arm_later_store_exit(cpu: &mut CpuGsw, start: u32, end: u32) {
    cpu.halted = false;
    cpu.registers.eip = STORE_ENTRY - 1;
    cpu.registers.gpr.fill(0);
    cpu.registers.set_eax(0x88);
    cpu.registers.set_ebx(start);
    cpu.registers.set_edi(end);
    cpu.registers.eflags = 0x203;
    cpu.pending_flags = PendingFlags::default();
    make_data_segments_flat(cpu);
}

#[test]
fn direct_self_loop_reports_a_later_iteration_memory_side_exit() {
    let memory = later_store_exit_program();
    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;
    native_bus.direct_page_clocks = true;

    arm_later_store_exit(
        &mut interp,
        WATCHED_TARGET as u32,
        WATCHED_TARGET as u32 + 1,
    );
    drive(&mut interp, &mut interp_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..5 {
        native_bus.memory[WATCHED_TARGET] = 0x88;
        arm_later_store_exit(
            &mut native,
            WATCHED_TARGET as u32,
            WATCHED_TARGET as u32 + 1,
        );
        drive(&mut native, &mut native_bus);
    }
    assert!(
        jit::direct::compile(&mut native, STORE_ENTRY, true).is_some(),
        "later-store block was not directly compilable"
    );
    assert!(
        native.jit_direct.len() > 0,
        "cache={} tracked={} perf={:?}",
        native.jit_direct.len(),
        native.jit_direct.tracked_len(),
        native.perf_counters()
    );

    for bus in [&mut interp_bus, &mut native_bus] {
        bus.memory[LATER_STORE_TARGET] = 0;
        bus.memory[LATER_STORE_TARGET + 1] = 0x90;
        bus.trace = BusTrace::default();
    }
    for cpu in [&mut interp, &mut native] {
        arm_later_store_exit(
            cpu,
            LATER_STORE_TARGET as u32,
            LATER_STORE_TARGET as u32 + 2,
        );
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    let direct_insns = native.perf_counters().jit_direct_insns;
    let direct_entries = native.perf_counters().jit_direct_entries;
    let direct_exits = native.perf_counters().jit_direct_side_exits;
    let direct_stores = native.perf_counters().jit_native_store_hits;

    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(
        native_outcomes
            .iter()
            .map(|(clocks, _, _)| u64::from(*clocks))
            .sum::<u64>(),
        interp_outcomes
            .iter()
            .map(|(clocks, _, _)| u64::from(*clocks))
            .sum::<u64>()
    );
    assert!(native_outcomes.last().is_some_and(|outcome| outcome.2));
    assert_eq!(native, interp);
    assert_eq!(native_bus.memory, interp_bus.memory);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert_eq!(native.perf_counters().jit_direct_insns - direct_insns, 7);
    assert_eq!(
        native.perf_counters().jit_direct_entries - direct_entries,
        2
    );
    assert_eq!(
        native.perf_counters().jit_direct_side_exits - direct_exits,
        1
    );
    assert_eq!(
        native.perf_counters().jit_native_store_hits - direct_stores,
        1
    );
}

fn mode13_loop_program() -> Vec<u8> {
    let mut memory = vec![0; 0x000b_0000];
    memory[0x100] = 0x90;
    memory[0x101..0x10f].copy_from_slice(&[
        0xa2, 0x00, 0x00, 0x0a, 0x00, // mov [0xa0000],al
        0x83, 0xc0, 0x01, // add eax,1
        0x83, 0xe9, 0x01, // sub ecx,1
        0x75, 0xf3, // jnz 0x101
        0xf4,
    ]);
    memory
}

fn arm_mode13_loop(cpu: &mut CpuGsw, iterations: u32) {
    cpu.halted = false;
    cpu.registers.eip = 0x100;
    cpu.registers.gpr.fill(0);
    cpu.registers.set_ecx(iterations);
    cpu.registers.eflags = 0x203;
    cpu.pending_flags = PendingFlags::default();
    make_data_segments_flat(cpu);
}

#[test]
fn direct_self_loop_aggregates_mode13_dirty_timing_past_packed_counter_width() {
    const ITERATIONS: u32 = 300;
    let memory = mode13_loop_program();
    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;
    native_bus.direct_page_clocks = true;

    arm_mode13_loop(&mut interp, 3);
    drive(&mut interp, &mut interp_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..3 {
        arm_mode13_loop(&mut native, 3);
        drive(&mut native, &mut native_bus);
    }
    assert!(native.jit_direct.len() > 0);
    interp_bus.uniform_native_fetches = true;
    native_bus.uniform_native_fetches = true;

    for bus in [&mut interp_bus, &mut native_bus] {
        bus.memory[0x000a_0000] = 0;
        bus.trace = BusTrace::default();
        bus.mode13_dirty_pages = 0;
        bus.mode13_byte_writes = 0;
        bus.mode13_dword_writes = 0;
    }
    for cpu in [&mut interp, &mut native] {
        arm_mode13_loop(cpu, ITERATIONS);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    let direct_insns = native.perf_counters().jit_direct_insns;
    let direct_entries = native.perf_counters().jit_direct_entries;

    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(native_outcomes, interp_outcomes);
    assert_eq!(native, interp);
    assert_eq!(native_bus.memory, interp_bus.memory);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert_eq!(native_bus.mode13_dirty_pages, 1);
    assert_eq!(native_bus.mode13_byte_writes, u64::from(ITERATIONS));
    assert_eq!(native_bus.mode13_byte_writes, interp_bus.mode13_byte_writes);
    assert_eq!(
        native.perf_counters().jit_direct_insns - direct_insns,
        u64::from(ITERATIONS) * 4
    );
    assert_eq!(
        native.perf_counters().jit_direct_entries - direct_entries,
        1
    );
}

#[test]
fn direct_self_loop_entry_rejects_interrupt_shadow_and_segment_preconditions() {
    let memory = quake_loop_program();
    let mut cpu = flat_stack_cpu(GAME_LOOP_ENTRY);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    cpu.set_jit_auto_admit(true);
    for _ in 0..3 {
        arm_quake_loop(&mut cpu);
        drive(&mut cpu, &mut bus);
    }
    let key = jit::direct::key_for(&cpu, GAME_LOOP_ENTRY, true).unwrap();
    let jit::direct::BlockProbe::Ready(id) = cpu.jit_direct.probe(key) else {
        panic!("Quake block was not ready");
    };
    let block = cpu.jit_direct.block(id).expect("ready block must be live");

    arm_quake_loop(&mut cpu);
    let registers = cpu.registers.clone();
    let pending = cpu.pending_flags;

    let observer_rejects = cpu.perf_counters().jit_direct_reject_observer;
    cpu.profile.enabled = true;
    assert!(!cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    cpu.profile.enabled = false;
    assert_eq!(
        cpu.perf_counters().jit_direct_reject_observer - observer_rejects,
        1
    );

    let aggregate_rejects = cpu.perf_counters().jit_direct_reject_aggregate_accounting;
    bus.native_aggregate_accounting_disabled = true;
    assert!(!cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    bus.native_aggregate_accounting_disabled = false;
    assert_eq!(
        cpu.perf_counters().jit_direct_reject_aggregate_accounting - aggregate_rejects,
        1
    );

    let shadow_rejects = cpu.perf_counters().jit_direct_reject_interrupt_shadow;
    cpu.interrupt_shadow = true;
    assert!(!cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    assert_eq!(
        cpu.perf_counters().jit_direct_reject_interrupt_shadow - shadow_rejects,
        1
    );
    assert_eq!(cpu.registers, registers);
    assert_eq!(cpu.pending_flags, pending);

    cpu.interrupt_shadow = false;
    let flat_ss = cpu.registers.segment(SegmentIndex::Ss);
    let mut changed_ss = flat_ss;
    changed_ss.default_size_32 = !changed_ss.default_size_32;
    cpu.registers.set_segment(SegmentIndex::Ss, changed_ss);
    let mode_rejects = cpu.perf_counters().jit_direct_reject_mode_key;
    assert!(!cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    assert_eq!(
        cpu.perf_counters().jit_direct_reject_mode_key - mode_rejects,
        1
    );
    cpu.registers.set_segment(SegmentIndex::Ss, flat_ss);

    let flat_cs = cpu.registers.cs();
    cpu.cpl = 3;
    let cpl_rejects = cpu.perf_counters().jit_direct_reject_cpl;
    assert!(!cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    assert_eq!(cpu.perf_counters().jit_direct_reject_cpl - cpl_rejects, 1);
    cpu.cpl = 0;

    let flat_ds = cpu.registers.segment(SegmentIndex::Ds);
    let mut ds = flat_ds;
    ds.base = 1;
    cpu.registers.set_segment(SegmentIndex::Ds, ds);
    let data_rejects = cpu.perf_counters().jit_direct_reject_data_segment;
    assert!(!cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    assert_eq!(
        cpu.perf_counters().jit_direct_reject_data_segment - data_rejects,
        1
    );

    cpu.registers.set_segment(SegmentIndex::Ds, flat_ds);
    let mut cs = flat_cs;
    cs.limit = 0x0000_ffff;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    let cs_rejects = cpu.perf_counters().jit_direct_reject_cs_layout;
    assert!(!cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    assert_eq!(
        cpu.perf_counters().jit_direct_reject_cs_layout - cs_rejects,
        1
    );
}

fn flat_stack_cpu(entry: u32) -> CpuGsw {
    let mut cpu = fresh();
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
    read: bool,
    write: bool,
) {
    if read {
        let page = bus
            .direct_page(physical, BusAccessKind::DataRead)
            .unwrap()
            .unwrap();
        assert!(
            cpu.jit_fast_map
                .populate_read(linear, physical, page, permissions)
        );
    }
    if write {
        let page = bus
            .direct_page(physical, BusAccessKind::DataWrite)
            .unwrap()
            .unwrap();
        assert!(
            cpu.jit_fast_map
                .populate_write(linear, physical, page, permissions)
        );
    }
}

fn install_fixture_block(cpu: &mut CpuGsw, linear: u32) -> jit::direct::CompiledBlock {
    let key = jit::direct::key_for(cpu, linear, true).unwrap();
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = jit::direct::compile(cpu, linear, true)
        .unwrap_or_else(|| panic!("direct compilation failed at {linear:#x}"));
    let id = cpu.jit_direct.install(&compilation).unwrap_or_else(|| {
        panic!(
            "direct install failed at {linear:#x}, code_len={}",
            compilation.code.len()
        )
    });
    cpu.jit_direct.block(id).unwrap()
}

fn arm_stack_fixture(cpu: &mut CpuGsw, entry: u32, esp: u32) {
    cpu.halted = false;
    cpu.interrupt_shadow = false;
    cpu.registers.gpr.fill(0);
    cpu.registers.set_ebx(0x89ab_cdef);
    cpu.registers.set_esp(esp);
    cpu.registers.eflags = 0x246;
    cpu.pending_flags = PendingFlags::default();
    cpu.set_eip(entry);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
}

#[test]
fn direct_stack_call_jump_and_return_chain_matches_interpreter() {
    const ENTRY: u32 = 0x101;
    const CALL: u32 = 0x10a;
    const RETURN: u32 = 0x10f;
    const TARGET: u32 = 0x120;
    const FINAL: u32 = 0x130;
    const HALT: u32 = 0x140;
    const INITIAL_ESP: u32 = 0x3000;

    let mut pristine = vec![0; 0x5000];
    pristine[ENTRY as usize..RETURN as usize].copy_from_slice(&[
        0x54, // push esp
        0x58, // pop eax
        0x68, 0x78, 0x56, 0x34, 0x12, // push 0x12345678
        0x6a, 0xfe, // push -2
        0xe8, 0x11, 0x00, 0x00, 0x00, // call 0x120
    ]);
    pristine[RETURN as usize..0x116].copy_from_slice(&[
        0x89, 0xc1, // mov ecx,eax
        0xe9, 0x1a, 0x00, 0x00, 0x00, // jmp 0x130
    ]);
    pristine[TARGET as usize..0x125].copy_from_slice(&[
        0x53, // push ebx
        0x5a, // pop edx
        0xc2, 0x08, 0x00, // ret 8
    ]);
    pristine[FINAL as usize..0x138].copy_from_slice(&[
        0x68, 0x34, 0x12, 0x00, 0x00, // push 0x1234
        0x5c, // pop esp
        0xeb, 0x08, // jmp 0x140
    ]);
    pristine[HALT as usize] = 0xf4;

    let mut native = flat_stack_cpu(ENTRY);
    let mut interp = flat_stack_cpu(ENTRY);
    let mut native_bus = TestBus::with_memory(pristine.clone());
    let mut interp_bus = TestBus::with_memory(pristine.clone());
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [
        ENTRY, 0x102, 0x103, 0x108, 0x10a, RETURN, 0x111, TARGET, 0x121, 0x122, FINAL, 0x135, 0x136,
    ];
    decode_fixture(&mut native, &mut native_bus, &starts);
    decode_fixture(&mut interp, &mut interp_bus, &starts);
    map_direct_page(
        &mut native,
        &mut native_bus,
        0x2000,
        0x2000,
        jit::fast_map::PagePermissions::UNPAGED,
        true,
        true,
    );

    let entry_block = install_fixture_block(&mut native, ENTRY);
    install_fixture_block(&mut native, CALL);
    let return_block = install_fixture_block(&mut native, RETURN);
    install_fixture_block(&mut native, TARGET);
    install_fixture_block(&mut native, FINAL);
    assert_eq!(entry_block.span().instructions, 4);

    arm_stack_fixture(&mut native, ENTRY, INITIAL_ESP);
    arm_stack_fixture(&mut interp, ENTRY, INITIAL_ESP);
    native_bus.memory.copy_from_slice(&pristine);
    interp_bus.memory.copy_from_slice(&pristine);
    native_bus.trace = BusTrace::default();
    interp_bus.trace = BusTrace::default();

    let registers = native.registers.clone();
    let memory = native_bus.memory.clone();
    assert!(
        !native
            .try_run_direct_block_with_cap_for_test(&mut native_bus, entry_block, 1)
            .unwrap()
    );
    assert_eq!(native.registers, registers);
    assert_eq!(native_bus.memory, memory);

    let entries = native.perf_counters().jit_direct_entries;
    let transfers = native.perf_counters().jit_direct_linked_transfers;
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, entry_block)
            .unwrap()
    );
    assert_eq!(native.registers.eip, RETURN);
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, return_block)
            .unwrap()
    );
    assert_eq!(native.registers.eip, HALT);
    for _ in 0..13 {
        interp.cycle(&mut interp_bus).unwrap();
    }

    assert_eq!(native.registers, interp.registers);
    assert_eq!(native.pending_flags, interp.pending_flags);
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
    assert_eq!(native_bus.memory, interp_bus.memory);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert_eq!(native.registers.eax(), INITIAL_ESP);
    assert_eq!(native.registers.ecx(), INITIAL_ESP);
    assert_eq!(native.registers.edx(), 0x89ab_cdef);
    assert_eq!(native.registers.esp(), 0x1234);
    assert_eq!(native.perf_counters().jit_direct_entries - entries, 2);
    assert_eq!(
        native.perf_counters().jit_direct_linked_transfers - transfers,
        3
    );
}

#[test]
fn direct_cross_page_push_exits_before_esp_commit() {
    const ENTRY: u32 = 0x201;
    const PUSH: u32 = 0x208;
    const INITIAL_ESP: u32 = 0x1002;
    let mut pristine = vec![0; 0x3000];
    pristine[ENTRY as usize..0x20a].copy_from_slice(&[
        0xb8, 0x44, 0x33, 0x22, 0x11, // mov eax,0x11223344
        0x89, 0xc1, // mov ecx,eax
        0x50, // push eax
        0xf4,
    ]);
    let mut native = flat_stack_cpu(ENTRY);
    let mut interp = flat_stack_cpu(ENTRY);
    let mut native_bus = TestBus::with_memory(pristine.clone());
    let mut interp_bus = TestBus::with_memory(pristine.clone());
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    decode_fixture(&mut native, &mut native_bus, &[ENTRY, 0x206, PUSH]);
    decode_fixture(&mut interp, &mut interp_bus, &[ENTRY, 0x206, PUSH]);
    map_direct_page(
        &mut native,
        &mut native_bus,
        0,
        0,
        jit::fast_map::PagePermissions::UNPAGED,
        false,
        true,
    );
    let block = install_fixture_block(&mut native, ENTRY);
    arm_stack_fixture(&mut native, ENTRY, INITIAL_ESP);
    arm_stack_fixture(&mut interp, ENTRY, INITIAL_ESP);
    native_bus.memory.copy_from_slice(&pristine);
    interp_bus.memory.copy_from_slice(&pristine);
    native_bus.trace = BusTrace::default();
    interp_bus.trace = BusTrace::default();
    let exits = native
        .perf_counters()
        .jit_direct_exit_cross_page_or_alignment;

    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    interp.cycle(&mut interp_bus).unwrap();
    interp.cycle(&mut interp_bus).unwrap();
    assert_eq!(native.registers, interp.registers);
    assert_eq!(native.registers.eip, PUSH);
    assert_eq!(native.registers.esp(), INITIAL_ESP);
    assert_eq!(native_bus.memory, pristine);
    assert_eq!(
        native
            .perf_counters()
            .jit_direct_exit_cross_page_or_alignment
            - exits,
        1
    );

    native.cycle(&mut native_bus).unwrap();
    interp.cycle(&mut interp_bus).unwrap();
    assert_eq!(native.registers, interp.registers);
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
    assert_eq!(native_bus.memory, interp_bus.memory);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
}

#[test]
fn direct_user_stack_permission_exit_preserves_fault_restart_state() {
    const ENTRY: u32 = 0x201;
    const PUSH: u32 = 0x208;
    const INITIAL_ESP: u32 = 0x8804;
    let mut pristine = vec![0; 0xa000];
    pristine[ENTRY as usize..0x20a].copy_from_slice(&[
        0xb8, 0x44, 0x33, 0x22, 0x11, // mov eax,0x11223344
        0x89, 0xc1, // mov ecx,eax
        0x50, // push eax
        0xf4,
    ]);
    pristine[0x3000..0x3004].copy_from_slice(&0x4007u32.to_le_bytes());
    pristine[0x4000..0x4004].copy_from_slice(&0x0007u32.to_le_bytes());
    pristine[0x4020..0x4024].copy_from_slice(&0x8003u32.to_le_bytes());

    let user_cpu = || {
        let mut cpu = flat_stack_cpu(ENTRY);
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
    };
    let mut native = user_cpu();
    let mut interp = user_cpu();
    let mut native_bus = TestBus::with_memory(pristine.clone());
    let mut interp_bus = TestBus::with_memory(pristine.clone());
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    decode_fixture(&mut native, &mut native_bus, &[ENTRY, 0x206, PUSH]);
    decode_fixture(&mut interp, &mut interp_bus, &[ENTRY, 0x206, PUSH]);
    map_direct_page(
        &mut native,
        &mut native_bus,
        0x8000,
        0x8000,
        jit::fast_map::PagePermissions {
            writable: true,
            user: false,
        },
        false,
        true,
    );
    let block = install_fixture_block(&mut native, ENTRY);
    arm_stack_fixture(&mut native, ENTRY, INITIAL_ESP);
    arm_stack_fixture(&mut interp, ENTRY, INITIAL_ESP);
    native_bus.memory.copy_from_slice(&pristine);
    interp_bus.memory.copy_from_slice(&pristine);
    native_bus.trace = BusTrace::default();
    interp_bus.trace = BusTrace::default();
    let exits = native.perf_counters().jit_direct_exit_permission;

    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    interp.cycle(&mut interp_bus).unwrap();
    interp.cycle(&mut interp_bus).unwrap();
    assert_eq!(native.registers, interp.registers);
    assert_eq!(native.registers.eip, PUSH);
    assert_eq!(native.registers.esp(), INITIAL_ESP);
    assert_eq!(native.perf_counters().jit_direct_exit_permission - exits, 1);

    let native_fault = native.push(&mut native_bus, native.registers.eax(), OperandSize::Dword);
    let interp_fault = interp.push(&mut interp_bus, interp.registers.eax(), OperandSize::Dword);
    let fault_code = |fault| match fault {
        Err(InternalFault::Exception {
            vector: 14,
            error_code: Some(code),
        }) => code,
        other => panic!("expected page fault, got {other:?}"),
    };
    assert_eq!(fault_code(native_fault), fault_code(interp_fault));
    assert_eq!(native.registers.esp(), INITIAL_ESP);
    assert_eq!(interp.registers.esp(), INITIAL_ESP);
    assert_eq!(
        &native_bus.memory[0x8800..0x8804],
        &pristine[0x8800..0x8804]
    );
    assert_eq!(
        &interp_bus.memory[0x8800..0x8804],
        &pristine[0x8800..0x8804]
    );
}

const ALU_MEM_ENTRY: u32 = 0x501;

fn memory_alu_instruction(op: u8, form: u8, target: u32, source: u32) -> Vec<u8> {
    let mut instruction = match form {
        0 => vec![(op << 3) | 1, (1 << 3) | 5],
        1 => vec![0x81, (op << 3) | 5],
        2 => vec![0x83, (op << 3) | 5],
        3 => vec![0x80, (op << 3) | 5],
        _ => unreachable!("memory ALU form"),
    };
    instruction.extend_from_slice(&target.to_le_bytes());
    match form {
        1 => instruction.extend_from_slice(&source.to_le_bytes()),
        2 | 3 => instruction.push(source as u8),
        _ => {}
    }
    instruction
}

fn run_memory_alu_differential(op: u8, form: u8, target: u32, old: u32, source: u32, eflags: u32) {
    let instruction = memory_alu_instruction(op, form, target, source);
    let width = if form == 3 {
        BusWidth::Byte
    } else {
        BusWidth::Dword
    };
    let memory_len = usize::try_from(target).unwrap().saturating_add(0x2000);
    let mut pristine = vec![0; memory_len.max(0x4000)];
    pristine[(ALU_MEM_ENTRY - 1) as usize] = 0x90;
    let mut code = instruction.clone();
    code.extend_from_slice(&[0x89, 0xc2, 0x89, 0xdb, 0xf4]);
    pristine[ALU_MEM_ENTRY as usize..ALU_MEM_ENTRY as usize + code.len()].copy_from_slice(&code);
    let target = target as usize;
    match width {
        BusWidth::Byte => pristine[target] = old as u8,
        BusWidth::Dword => pristine[target..target + 4].copy_from_slice(&old.to_le_bytes()),
        BusWidth::Word => unreachable!(),
    }

    let mut native = flat_stack_cpu(ALU_MEM_ENTRY);
    let mut interp = flat_stack_cpu(ALU_MEM_ENTRY);
    let mut native_bus = TestBus::with_memory(pristine.clone());
    let mut interp_bus = TestBus::with_memory(pristine);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [
        ALU_MEM_ENTRY,
        ALU_MEM_ENTRY + instruction.len() as u32,
        ALU_MEM_ENTRY + instruction.len() as u32 + 2,
    ];
    decode_fixture(&mut native, &mut native_bus, &starts);
    decode_fixture(&mut interp, &mut interp_bus, &starts);
    map_direct_page(
        &mut native,
        &mut native_bus,
        target as u32,
        target as u32,
        jit::fast_map::PagePermissions::UNPAGED,
        true,
        op != 7,
    );
    let block = install_fixture_block(&mut native, ALU_MEM_ENTRY);

    for cpu in [&mut native, &mut interp] {
        cpu.halted = false;
        cpu.registers.gpr.fill(0);
        cpu.registers.set_ecx(source);
        cpu.registers.set_ebx(0x55aa_33cc);
        cpu.registers.eflags = eflags;
        cpu.pending_flags = PendingFlags::default();
        cpu.set_eip(ALU_MEM_ENTRY);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    native_bus.trace = BusTrace::default();
    interp_bus.trace = BusTrace::default();

    let registers_before_cap = native.registers.clone();
    let pending_before_cap = native.pending_flags;
    let memory_before_cap = native_bus.memory.clone();
    let budget_rejects = native.perf_counters().jit_direct_reject_zero_budget;
    assert!(
        !native
            .try_run_direct_block_with_cap_for_test(&mut native_bus, block, 1)
            .unwrap(),
        "tight cap admitted op={op} form={form} target={target:#x}"
    );
    assert_eq!(native.registers, registers_before_cap);
    assert_eq!(native.pending_flags, pending_before_cap);
    assert_eq!(native_bus.memory, memory_before_cap);
    assert_eq!(
        native.perf_counters().jit_direct_reject_zero_budget - budget_rejects,
        1
    );

    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap(),
        "op={op} form={form} target={target:#x}"
    );
    for _ in 0..3 {
        interp.cycle(&mut interp_bus).unwrap();
    }

    assert_eq!(native.registers, interp.registers, "op={op} form={form}");
    assert_eq!(
        native.pending_flags, interp.pending_flags,
        "op={op} form={form}"
    );
    assert_eq!(native.eflags(), interp.eflags(), "op={op} form={form}");
    assert_eq!(
        native.elapsed_clocks, interp.elapsed_clocks,
        "op={op} form={form}"
    );
    assert_eq!(native_bus.memory, interp_bus.memory, "op={op} form={form}");
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks(),
        "op={op} form={form}"
    );
    assert_eq!(native_bus.mode13_dirty_pages, interp_bus.mode13_dirty_pages);
    assert_eq!(native_bus.mode13_byte_writes, interp_bus.mode13_byte_writes);
    assert_eq!(
        native_bus.mode13_dword_writes,
        interp_bus.mode13_dword_writes
    );
}

#[test]
fn direct_memory_destination_alu_matrix_matches_interpreter_in_ram_and_mode13() {
    for target in [0x3000, 0x000a_1000] {
        for op in 0..8 {
            run_memory_alu_differential(op, 0, target, 0x7fff_fffe, 0x8000_0001, 0x247);
            run_memory_alu_differential(op, 1, target, 0x7fff_fffe, 0x8000_0001, 0x247);
            run_memory_alu_differential(op, 2, target, 0x7fff_fffe, 0xffff_ffff, 0x247);
            run_memory_alu_differential(op, 3, target, 0x7e, 0x81, 0x247);
            if matches!(op, 2 | 3) {
                run_memory_alu_differential(op, 0, target, 0x7fff_fffe, 0x8000_0001, 0x246);
                run_memory_alu_differential(op, 1, target, 0x7fff_fffe, 0x8000_0001, 0x246);
            }
        }
    }
}

fn run_watched_memory_alu(form: u8, same_value: bool) {
    const TARGET: u32 = 0x3000;
    let op = if same_value { 1 } else { 0 };
    let source = u32::from(!same_value);
    let instruction = memory_alu_instruction(op, form, TARGET, source);
    let width = if form == 3 { 1 } else { 4 };
    let old = 0x1122_3344u32;
    let mut pristine = vec![0; 0x5000];
    pristine[(ALU_MEM_ENTRY - 1) as usize] = 0x90;
    let mut code = instruction.clone();
    code.extend_from_slice(&[0x89, 0xc2, 0x89, 0xdb, 0xf4]);
    pristine[ALU_MEM_ENTRY as usize..ALU_MEM_ENTRY as usize + code.len()].copy_from_slice(&code);
    pristine[TARGET as usize..TARGET as usize + 4].copy_from_slice(&old.to_le_bytes());

    let mut native = flat_stack_cpu(ALU_MEM_ENTRY);
    let mut interp = flat_stack_cpu(ALU_MEM_ENTRY);
    let mut native_bus = TestBus::with_memory(pristine.clone());
    let mut interp_bus = TestBus::with_memory(pristine);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [
        ALU_MEM_ENTRY,
        ALU_MEM_ENTRY + instruction.len() as u32,
        ALU_MEM_ENTRY + instruction.len() as u32 + 2,
    ];
    decode_fixture(&mut native, &mut native_bus, &starts);
    decode_fixture(&mut interp, &mut interp_bus, &starts);
    map_direct_page(
        &mut native,
        &mut native_bus,
        TARGET,
        TARGET,
        jit::fast_map::PagePermissions::UNPAGED,
        true,
        true,
    );
    let block = install_fixture_block(&mut native, ALU_MEM_ENTRY);
    native.decode_cache.mark_code_range(TARGET, width);
    for cpu in [&mut native, &mut interp] {
        cpu.registers.gpr.fill(0);
        cpu.registers.set_ecx(source);
        cpu.registers.eflags = 0x247;
        cpu.pending_flags = PendingFlags::default();
        cpu.set_eip(ALU_MEM_ENTRY);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    let registers = native.registers.clone();
    let pending = native.pending_flags;
    let memory = native_bus.memory.clone();
    let exits = native.perf_counters().jit_direct_exit_code_watch;

    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    if same_value {
        assert_eq!(native.perf_counters().jit_direct_exit_code_watch, exits);
    } else {
        assert_eq!(native.registers, registers);
        assert_eq!(native.pending_flags, pending);
        assert_eq!(native_bus.memory, memory);
        assert_eq!(native.perf_counters().jit_direct_exit_code_watch - exits, 1);
        for _ in 0..3 {
            native.cycle(&mut native_bus).unwrap();
        }
    }
    for _ in 0..3 {
        interp.cycle(&mut interp_bus).unwrap();
    }

    assert_eq!(
        native.registers, interp.registers,
        "form={form} same={same_value}"
    );
    assert_eq!(native.pending_flags, interp.pending_flags);
    assert_eq!(native.eflags(), interp.eflags());
    assert_eq!(native_bus.memory, interp_bus.memory);
}

#[test]
fn direct_memory_alu_watched_same_value_commits_and_changed_value_exits_transactionally() {
    for form in [0, 1, 3] {
        run_watched_memory_alu(form, true);
        run_watched_memory_alu(form, false);
    }
}

#[test]
fn repeated_memory_alu_root_splits_below_one_host_page_and_retires_natively() {
    const TARGET: u32 = 0x3000;
    const COUNT: usize = 32;
    let instruction = memory_alu_instruction(2, 1, TARGET, 1);
    let mut pristine = vec![0; 0x5000];
    pristine[(ALU_MEM_ENTRY - 1) as usize] = 0x90;
    let mut starts = Vec::with_capacity(COUNT);
    let mut cursor = ALU_MEM_ENTRY as usize;
    for _ in 0..COUNT {
        starts.push(cursor as u32);
        pristine[cursor..cursor + instruction.len()].copy_from_slice(&instruction);
        cursor += instruction.len();
    }
    pristine[cursor] = 0xf4;

    let mut native = flat_stack_cpu(ALU_MEM_ENTRY);
    let mut bus = TestBus::with_memory(pristine);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    decode_fixture(&mut native, &mut bus, &starts);
    map_direct_page(
        &mut native,
        &mut bus,
        TARGET,
        TARGET,
        jit::fast_map::PagePermissions::UNPAGED,
        true,
        true,
    );
    let compilation = jit::direct::compile(&mut native, ALU_MEM_ENTRY, true)
        .expect("repeated memory ALU root must split to a compilable prefix");
    assert_eq!(compilation.span.instructions, 3);
    assert!(
        compilation.code.len() <= 3_400,
        "three memory ALU slots emitted {} bytes",
        compilation.code.len()
    );
    let key = jit::direct::key_for(&native, ALU_MEM_ENTRY, true).unwrap();
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let id = native.jit_direct.install(&compilation).unwrap();
    let block = native.jit_direct.block(id).unwrap();
    native.registers.eflags = 0x202;
    native.pending_flags = PendingFlags::default();
    native.set_eip(ALU_MEM_ENTRY);
    bus.memory[TARGET as usize..TARGET as usize + 4].fill(0);
    let retired = native.perf_counters().jit_direct_insns;

    assert!(
        native
            .try_run_direct_block_for_test(&mut bus, block)
            .unwrap()
    );
    assert_eq!(
        native.registers.eip,
        ALU_MEM_ENTRY + 3 * instruction.len() as u32
    );
    assert_eq!(
        u32::from_le_bytes(
            bus.memory[TARGET as usize..TARGET as usize + 4]
                .try_into()
                .unwrap()
        ),
        3
    );
    assert_eq!(native.perf_counters().jit_direct_insns - retired, 3);
}

fn paged_memory_alu_cpu(entry: u32) -> CpuGsw {
    let mut cpu = flat_stack_cpu(entry);
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
fn direct_memory_alu_paging_and_cross_page_exits_precede_flags_and_memory_mutation() {
    for (target, user_page, expected_cross_page, expected_unavailable) in [
        (0x8000u32, false, false, false),
        (0x8fffu32, true, true, false),
        (0x9000u32, true, false, true),
    ] {
        let instruction = memory_alu_instruction(0, 1, target, 1);
        let mut pristine = vec![0; 0xa000];
        pristine[(ALU_MEM_ENTRY - 1) as usize] = 0x90;
        let mut code = instruction.clone();
        code.extend_from_slice(&[0x89, 0xc2, 0x89, 0xdb, 0xf4]);
        pristine[ALU_MEM_ENTRY as usize..ALU_MEM_ENTRY as usize + code.len()]
            .copy_from_slice(&code);
        pristine[0x3000..0x3004].copy_from_slice(&0x4007u32.to_le_bytes());
        pristine[0x4000..0x4004].copy_from_slice(&0x0007u32.to_le_bytes());
        pristine[0x4020..0x4024]
            .copy_from_slice(&(if user_page { 0x8007u32 } else { 0x8003u32 }).to_le_bytes());
        pristine[0x8ffc..0x9000].copy_from_slice(&0x1122_3344u32.to_le_bytes());

        let mut native = paged_memory_alu_cpu(ALU_MEM_ENTRY);
        let mut interp = paged_memory_alu_cpu(ALU_MEM_ENTRY);
        let mut native_bus = TestBus::with_memory(pristine.clone());
        let mut interp_bus = TestBus::with_memory(pristine);
        for bus in [&mut native_bus, &mut interp_bus] {
            bus.direct_pages_enabled = true;
            bus.direct_page_clocks = true;
        }
        let starts = [
            ALU_MEM_ENTRY,
            ALU_MEM_ENTRY + instruction.len() as u32,
            ALU_MEM_ENTRY + instruction.len() as u32 + 2,
        ];
        decode_fixture(&mut native, &mut native_bus, &starts);
        decode_fixture(&mut interp, &mut interp_bus, &starts);
        let mapped_page = if expected_unavailable {
            0x8000
        } else {
            target & !0xfff
        };
        map_direct_page(
            &mut native,
            &mut native_bus,
            mapped_page,
            mapped_page,
            jit::fast_map::PagePermissions {
                writable: true,
                user: user_page,
            },
            true,
            true,
        );
        let block = install_fixture_block(&mut native, ALU_MEM_ENTRY);
        for cpu in [&mut native, &mut interp] {
            cpu.registers.gpr.fill(0);
            cpu.registers.eflags = 0x247;
            cpu.pending_flags = PendingFlags::default();
            cpu.set_eip(ALU_MEM_ENTRY);
            cpu.elapsed_clocks = 0;
            cpu.timing_rem = 0;
            cpu.core_clocks_so_far = 0;
        }
        let registers = native.registers.clone();
        let pending = native.pending_flags;
        let target_range = target as usize..target as usize + 4;
        let target_bytes = native_bus.memory[target_range.clone()].to_vec();
        let cross_page_exits = native
            .perf_counters()
            .jit_direct_exit_cross_page_or_alignment;
        let permission_exits = native.perf_counters().jit_direct_exit_permission;
        let unavailable_exits = native.perf_counters().jit_direct_exit_unavailable_or_kind;

        assert!(
            native
                .try_run_direct_block_for_test(&mut native_bus, block)
                .unwrap()
        );
        assert_eq!(native.registers, registers);
        assert_eq!(native.pending_flags, pending);
        assert_eq!(&native_bus.memory[target_range.clone()], target_bytes);
        assert_eq!(
            native
                .perf_counters()
                .jit_direct_exit_cross_page_or_alignment
                - cross_page_exits,
            u64::from(expected_cross_page)
        );
        assert_eq!(
            native.perf_counters().jit_direct_exit_permission - permission_exits,
            u64::from(!expected_cross_page && !expected_unavailable)
        );
        assert_eq!(
            native.perf_counters().jit_direct_exit_unavailable_or_kind - unavailable_exits,
            u64::from(expected_unavailable)
        );

        let decoded = interp.decode_cache.get(ALU_MEM_ENTRY, true).unwrap();
        let interp_fault = interp.execute_decoded(&decoded, &mut interp_bus);
        // `map_direct_page` is a native-emission fixture and seeds a mapping without performing
        // the architectural page walk that normally precedes a FastMap fill. Remove that synthetic
        // entry before comparing the precise interpreter fallback and its A/D writes.
        native.jit_fast_map.invalidate_page(target);
        let native_decoded = native.decode_cache.get(ALU_MEM_ENTRY, true).unwrap();
        let native_fault = native.execute_decoded(&native_decoded, &mut native_bus);
        let page_fault = |fault| match fault {
            Err(InternalFault::Exception {
                vector: 14,
                error_code,
            }) => error_code,
            other => panic!("target={target:#x} expected #PF, got {other:?}"),
        };
        assert_eq!(page_fault(native_fault), page_fault(interp_fault));
        assert_eq!(
            native.control.cr2,
            if expected_cross_page { 0x9000 } else { target }
        );
        assert_eq!(native.control.cr2, interp.control.cr2);
        assert_eq!(native.registers, interp.registers);
        assert_eq!(native.pending_flags, pending);
        assert_eq!(native.pending_flags, interp.pending_flags);
        assert_eq!(native_bus.memory, interp_bus.memory);
        assert_eq!(&native_bus.memory[target_range], target_bytes);
        if expected_cross_page {
            let first_pte =
                u32::from_le_bytes(native_bus.memory[0x4020..0x4024].try_into().unwrap());
            assert_eq!(
                first_pte & 0x60,
                0x20,
                "first page is accessed but not dirty"
            );
        }
    }
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
    code.extend_from_slice(&[0x66, 0x89, 0xc0]);
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
