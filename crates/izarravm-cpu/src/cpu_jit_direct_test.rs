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

#[test]
fn linked_target_eviction_returns_before_target_and_replays_cold_fetch() {
    const ENTRY: u32 = 0x100;
    const SOURCE: u32 = 0x101;
    const TARGET: u32 = 0x1200;
    const HLT: u32 = 0x120a;
    // One decode-cache line above TARGET, so the two share a line whatever the cache is sized to.
    // A hardcoded collision would silently stop colliding the next time DECODE_CACHE_LINES moves,
    // and this test would then pass while proving nothing: no eviction, no hidden portal.
    let collision = TARGET + crate::decode_cache_lines() as u32;

    let mut memory = vec![0; collision as usize + 1];
    memory[ENTRY as usize] = 0x90;
    memory[SOURCE as usize..SOURCE as usize + 13].copy_from_slice(&[
        0xb8, 1, 0, 0, 0, // mov eax,1
        0x83, 0xc0, 2, // add eax,2
        0xe9, 0xf2, 0x10, 0, 0, // jmp TARGET
    ]);
    memory[TARGET as usize..TARGET as usize + 10].copy_from_slice(&[
        0xbb, 3, 0, 0, 0, // mov ebx,3
        0x83, 0xc3, 4, // add ebx,4
        0xeb, 0, // jmp HLT
    ]);
    memory[HLT as usize] = 0xf4;
    memory[collision as usize] = 0x90;

    let mut native = fresh();
    let mut interp = fresh();
    // Keep the default decode-cache size. Installing a smaller one here would leave jit_direct's
    // decode-slot count at the default, and the eviction path only reports a slot to the JIT when
    // the two counts agree (see fetch_decoded and the guard in try_direct_continuation). Mismatched
    // counts take the wholesale invalidate_translation path instead, which hides the portal for a
    // different reason and makes this test prove the wrong thing.
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    native_bus.direct_pages_enabled = true;
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_page_clocks = true;
    interp_bus.direct_page_clocks = true;

    native.set_jit_auto_admit(true);
    native.jit_direct.set_admission_heat_for_test(1);
    let decode_at = |cpu: &mut CpuGsw, bus: &mut TestBus, linear: u32| {
        cpu.registers.eip = linear;
        cpu.fetch_decoded(bus, linear).expect("fixture decode");
    };
    for linear in [
        ENTRY,
        SOURCE,
        SOURCE + 5,
        SOURCE + 8,
        TARGET,
        TARGET + 5,
        TARGET + 8,
        HLT,
    ] {
        decode_at(&mut native, &mut native_bus, linear);
        decode_at(&mut interp, &mut interp_bus, linear);
    }

    let install = |cpu: &mut CpuGsw, linear: u32| {
        let key = jit::direct::key_for(cpu, linear, true).expect("fixture block key");
        assert!(matches!(
            cpu.jit_direct.probe(key),
            jit::direct::BlockProbe::Interpret
        ));
        let compilation = jit::direct::compile(cpu, linear, true).expect("fixture block");
        cpu.jit_direct
            .install(&compilation)
            .expect("fixture install")
    };
    let source_id = install(&mut native, SOURCE);
    install(&mut native, TARGET);
    let source = native
        .jit_direct
        .block(source_id)
        .expect("source remains live");
    assert!(native.jit_direct.has_linked_successor(source));

    decode_at(&mut native, &mut native_bus, collision);
    decode_at(&mut interp, &mut interp_bus, collision);
    assert!(native.decode_cache.line_live(SOURCE, true));
    assert!(!native.decode_cache.line_live(TARGET, true));
    assert!(native.decode_cache.line_live(TARGET + 5, true));
    assert!(native.decode_cache.line_live(TARGET + 8, true));
    assert!(native.decode_cache.line_live(collision, true));
    assert!(
        !native.jit_direct.has_linked_successor(source),
        "evicting the target slot must hide its portal from the source edge"
    );

    for cpu in [&mut native, &mut interp] {
        cpu.halted = false;
        cpu.registers.eip = ENTRY;
        cpu.registers.gpr.fill(0);
        cpu.registers.eflags = 0x2;
        cpu.pending_flags = PendingFlags::default();
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.fp_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    native_bus.trace = BusTrace::default();
    interp_bus.trace = BusTrace::default();
    let before = native.perf_counters().clone();

    let native_outcomes = drive(&mut native, &mut native_bus);
    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    assert_eq!(native_outcomes, interp_outcomes);
    assert_eq!(
        native_outcomes
            .iter()
            .map(|(_, eip, halted)| (*eip, *halted))
            .collect::<Vec<_>>(),
        vec![(TARGET, false), (HLT, false), (HLT + 1, true)]
    );
    assert_eq!(native, interp);
    assert_eq!(native.registers.eax(), 3);
    assert_eq!(native.registers.ebx(), 7);
    assert_eq!(native_bus.trace.cycles(), interp_bus.trace.cycles());
    assert_eq!(native_bus.trace.cycles().len(), 26);
    assert_eq!(native_bus.trace.elapsed_clocks(), 52);
    assert_eq!(
        native_bus
            .trace
            .cycles()
            .iter()
            .skip(14)
            .take(6)
            .map(|cycle| cycle.address)
            .collect::<Vec<_>>(),
        vec![
            TARGET,
            TARGET,
            TARGET + 1,
            TARGET + 2,
            TARGET + 3,
            TARGET + 4
        ]
    );

    let after = native.perf_counters();
    assert_eq!(after.instructions - before.instructions, 8);
    assert_eq!(after.decode_misses - before.decode_misses, 1);
    assert_eq!(after.straight_line_runs - before.straight_line_runs, 3);
    assert_eq!(after.brk_cont_decode_miss - before.brk_cont_decode_miss, 1);
    assert_eq!(
        after.brk_cont_not_continuable - before.brk_cont_not_continuable,
        1
    );
    assert_eq!(after.brk_halt - before.brk_halt, 1);
    assert_eq!(after.jit_direct_entries - before.jit_direct_entries, 1);
    assert_eq!(after.jit_direct_insns - before.jit_direct_insns, 3);
    assert_eq!(
        after.jit_direct_linked_transfers - before.jit_direct_linked_transfers,
        0
    );
    assert_eq!(
        after.jit_direct_unresolved_exits - before.jit_direct_unresolved_exits,
        1
    );
    assert_eq!(
        after.jit_direct_unresolved_static_unbound - before.jit_direct_unresolved_static_unbound,
        0
    );
    assert_eq!(
        after.jit_direct_unresolved_static_hidden - before.jit_direct_unresolved_static_hidden,
        1
    );
    assert_eq!(
        after.jit_direct_unresolved_dynamic_miss_or_unbound
            - before.jit_direct_unresolved_dynamic_miss_or_unbound,
        0
    );
    assert_eq!(
        after.jit_direct_unresolved_dynamic_hidden - before.jit_direct_unresolved_dynamic_hidden,
        0
    );
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
        0xb9,
        0x05,
        0x00,
        0x00,
        0x00,           // mov ecx,5
        DIRECT_BARRIER, // unsupported direct entry
        DIRECT_BARRIER, // unsupported direct entry
        0x83,
        0xe9,
        0x01, // sub ecx,1
        0x75,
        0xf9, // jnz 0x105
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
        0xb8,
        0x01,
        0x00,
        0x00,
        0x00, // mov eax,1
        0x89,
        0xc3, // mov ebx,eax
        0x83,
        0xc3,
        0x02,           // add ebx,2
        DIRECT_BARRIER, // unsupported barrier
        0xb9,
        0x04,
        0x00,
        0x00,
        0x00, // mov ecx,4
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
        ("same value", 0x5a, true, false, 1, Some(true)),
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
        (0x4100u32, 0x4100u32, true, 1, 1),
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

#[test]
fn same_value_watched_dword_store_elides_invalidation_and_replays_warm() {
    const TARGET: u32 = 0x4200;
    const VALUE: u32 = 0x1234_5678;

    let memory = store_exit_program(TARGET);
    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    arm_store_fixture(&mut interp);
    drive(&mut interp, &mut interp_bus);
    prime_direct_store_block(&mut native, &mut native_bus);

    for (cpu, bus) in [
        (&mut interp, &mut interp_bus),
        (&mut native, &mut native_bus),
    ] {
        bus.memory[TARGET as usize..TARGET as usize + 4].copy_from_slice(&VALUE.to_le_bytes());
        cpu.registers.eip = TARGET;
        cpu.begin_instruction();
        cpu.fetch_decoded(bus, TARGET).expect("target decode");
        arm_store_fixture(cpu);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.fp_rem = 0;
        cpu.core_clocks_so_far = 0;
        bus.trace = BusTrace::default();
    }
    let interp_invalidations = interp.perf_counters().code_invalidations;
    let native_invalidations = native.perf_counters().code_invalidations;

    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(native, interp);
    assert_eq!(native_bus.memory, interp_bus.memory);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert!(native_outcomes.last().is_some_and(|outcome| outcome.2));
    assert!(interp_outcomes.last().is_some_and(|outcome| outcome.2));
    // G2: the dword store writes the same bytes already at TARGET, so the watched-code write is
    // elided and neither side invalidates (the native store side-exits through the interpreter).
    let interp_after = interp.perf_counters().code_invalidations;
    let native_after = native.perf_counters().code_invalidations;
    assert_eq!(interp_after, interp_invalidations);
    assert_eq!(native_after, native_invalidations);

    let interp_misses = interp.perf_counters().decode_misses;
    let native_misses = native.perf_counters().decode_misses;
    for (cpu, bus) in [
        (&mut interp, &mut interp_bus),
        (&mut native, &mut native_bus),
    ] {
        cpu.registers.eip = TARGET;
        cpu.begin_instruction();
        cpu.fetch_decoded(bus, TARGET).expect("warm target decode");
    }
    // The decode line at TARGET survived (no invalidation), so the replay is a warm cache hit.
    assert_eq!(interp.perf_counters().decode_misses, interp_misses);
    assert_eq!(native.perf_counters().decode_misses, native_misses);
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
        (0x4100u32, 0xd4u8, 0, 1),
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

#[path = "cpu_jit_direct_execution_test.rs"]
mod execution;

#[path = "cpu_jit_sixteen_bit_test.rs"]
mod sixteen_bit;

#[path = "cpu_jit_direct_timing_test.rs"]
mod timing;

fn word_jcc_loop_program() -> Vec<u8> {
    let mut memory = vec![0; 0x1000];
    memory[0x100..0x10f].copy_from_slice(&[
        0xb9, 0x05, 0x00, 0x00, 0x00, // mov ecx,5
        0x83, 0xc0, 0x03, // add eax,3
        0x83, 0xe9, 0x01, // sub ecx,1
        0x66, 0x75, 0xf7, // jnz 0x105, at Word operand size
        0xf4, // hlt
    ]);
    memory
}

/// A 66-prefixed Jcc in 32-bit code decodes at Word operand size and is now lowered. This is
/// the end-to-end proof that the emitted condition and taken target are right, which the
/// compile-shape fixtures cannot see.
///
/// It does NOT exercise the `control_target_limit` clamp: at this entry `cs.limit` is 0xFFFF, so
/// the clamp is the identity and the 16-bit mask is a no-op. The clamp's catchers are the
/// high-entry fixtures in `cpu_jit_compile_outcome_test.rs`.
#[test]
fn direct_block_matches_the_interpreter_across_a_word_operand_size_jcc() {
    let mut interp = fresh();
    let mut native = fresh();
    interp.registers.set_eax(1);
    native.registers.set_eax(1);
    let mut interp_bus = TestBus::with_memory(word_jcc_loop_program());
    let mut native_bus = TestBus::with_memory(word_jcc_loop_program());
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;

    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);
    for cpu in [&mut interp, &mut native] {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.registers.set_eax(1);
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
    assert_eq!(native.registers.eax(), 16, "the loop ran five times");
    assert!(
        native.perf_counters().jit_direct_insns > native_before,
        "the Word Jcc block was never entered natively: {:?}",
        native.perf_counters()
    );
}

/// A protected-mode CPU with flat segments except SS, which keeps a 16-bit stack pointer.
///
/// SP is seeded at 0 so that every push BORROWS across bit 16. That is what makes the
/// partial-register update observable at all: at any SP with headroom below it, a 16-bit and a
/// 32-bit subtract give identical results, and a test built there cannot tell them apart. A
/// mutation battery found exactly that gap in the first version of this fixture.
fn sixteen_bit_stack_cpu(entry: u32) -> CpuGsw {
    let mut cpu = flat_stack_cpu(entry);
    let mut ss = cpu.registers.segment(SegmentIndex::Ss);
    ss.default_size_32 = false;
    cpu.registers.set_segment(SegmentIndex::Ss, ss);
    cpu.registers.set_esp(0x1234_0000);
    cpu
}

fn reset_sixteen_bit_stack_run(cpu: &mut CpuGsw, bus: &mut TestBus, entry: u32) {
    cpu.halted = false;
    cpu.set_eip(entry);
    cpu.registers.set_eax(0xbeef);
    cpu.registers.set_esp(0x1234_0000);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
    bus.memory[0xfff0..0x10000].fill(0);
    bus.trace = BusTrace::default();
}

fn word_push_loop_program() -> Vec<u8> {
    // A full 64K, because the stack wraps to 0xFFFE on the first push.
    let mut memory = vec![0; 0x10000];
    memory[0x100..0x10d].copy_from_slice(&[
        0xb9, 0x08, 0x00, 0x00, 0x00, // mov ecx,8
        0x66, 0x50, // push ax, at Word operand size on a 16-bit stack
        0x83, 0xe9, 0x01, // sub ecx,1
        0x75, 0xf9, // jnz 0x105
        0xf4, // hlt
    ]);
    memory
}

/// The end-to-end proof for the 16-bit stack push. The compile-shape fixtures can only see that
/// the slot was admitted; this sees whether it was admitted CORRECTLY.
///
/// Two mechanisms are load-bearing:
///
/// - **The effective address wraps at 64K.** `stack_addr` uses the full ESP home as its base, so
///   without the mask the address would be 0x1234_FFFE rather than 0x FFFE, on a page this
///   fixture never maps. The interpreter forms `u32::from(sp - 2)` and never leaves 16 bits.
/// - **The pointer update preserves ESP[31:16].** A 32-bit subtract would borrow into the high
///   half and give 0x1233_FFFE, where `write_gpr16` keeps the 0x1234.
///
/// The fast map is enabled and the stack page mapped UP FRONT. Without that the block still
/// compiles but every push takes a side exit into the interpreter, which produces correct guest
/// state and therefore hides an incorrect emitter entirely. The `jit_direct_side_exits` and
/// native-instruction assertions below are what keep this test from going vacuous the same way.
#[test]
fn direct_block_matches_the_interpreter_across_a_sixteen_bit_stack_push() {
    const ENTRY: u32 = 0x100;
    const STACK_PAGE: u32 = 0xf000;

    let mut interp = sixteen_bit_stack_cpu(ENTRY);
    let mut native = sixteen_bit_stack_cpu(ENTRY);
    for cpu in [&mut interp, &mut native] {
        cpu.registers.set_eax(0xbeef);
    }
    let mut interp_bus = TestBus::with_memory(word_push_loop_program());
    let mut native_bus = TestBus::with_memory(word_push_loop_program());
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.direct_pages_enabled = true;
    }
    native.jit_direct.set_fast_map_enabled_for_test(true);
    map_direct_page(
        &mut native,
        &mut native_bus,
        STACK_PAGE,
        STACK_PAGE,
        jit::fast_map::PagePermissions::UNPAGED,
        true,
        true,
    );

    // Three passes, and the third is the measured one. The first warms every decode line; the
    // second lets the first-seen/second-entry admission policy install the block. Only then is
    // the state reset, so that the FIRST push of the measured run is native and starts at
    // SP = 0.
    //
    // That ordering is the whole point. Only the first push of a run borrows across bit 16;
    // every later one starts from 0xFFFE and does not. Measure a run whose first push was
    // interpreted and a 32-bit subtract is indistinguishable from a 16-bit one, which is
    // precisely how the first version of this test passed under a mutation that broke it.
    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);
    native.set_jit_auto_admit(true);
    reset_sixteen_bit_stack_run(&mut native, &mut native_bus, ENTRY);
    drive(&mut native, &mut native_bus);
    reset_sixteen_bit_stack_run(&mut native, &mut native_bus, ENTRY);
    reset_sixteen_bit_stack_run(&mut interp, &mut interp_bus, ENTRY);
    let native_before = native.perf_counters().jit_direct_insns;
    let side_before = native.perf_counters().jit_direct_side_exits;
    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(
        native_outcomes, interp_outcomes,
        "run-boundary timing differs"
    );
    assert_eq!(native, interp, "architectural or clock state differs");
    assert_eq!(
        native_bus.memory, interp_bus.memory,
        "stack contents differ"
    );

    // Eight pushes of two bytes each, with only SP moving.
    assert_eq!(
        native.registers.esp(),
        0x1234_fff0,
        "SP must wrap in 16 bits and ESP[31:16] must survive"
    );
    assert_eq!(
        &native_bus.memory[0xfff0..0x10000],
        &[0xefu8, 0xbe].repeat(8)[..],
        "each push writes two bytes at a masked address"
    );
    // At least the three pushes plus their block-mates ran natively, and none of them fell back
    // through a side exit. Both halves matter: without the second, an emitter that computed the
    // wrong address would side-exit, the interpreter would produce the right answer, and every
    // assertion above would still pass.
    assert!(
        native.perf_counters().jit_direct_insns >= native_before + 6,
        "the 16-bit stack block was not entered natively: {:?}",
        native.perf_counters()
    );
    assert_eq!(
        native.perf_counters().jit_direct_side_exits,
        side_before,
        "a push that side-exits proves nothing about the emitted form"
    );
}

fn word_pop_program() -> Vec<u8> {
    let mut memory = vec![0; 0x10000];
    memory[0x100..0x108].copy_from_slice(&[
        0x66, 0x58, // pop ax
        0x66, 0x5b, // pop bx
        0x66, 0x59, // pop cx
        0x40, // inc eax, so the block is not all pops
        0xf4, // hlt
    ]);
    // The words the pops will read, starting at SP = 0xFFFA. The THIRD pop is the one whose
    // advance carries out of bit 16, and it is chosen that way deliberately: block admission
    // takes two passes and the block ends up keyed at the SECOND pop, so a carry placed on the
    // first one is executed by the interpreter and discriminates nothing. A mutation battery
    // found exactly that.
    memory[0xfffa] = 0x11;
    memory[0xfffb] = 0x22;
    memory[0xfffc] = 0x33;
    memory[0xfffd] = 0x44;
    memory[0xfffe] = 0x55;
    memory[0xffff] = 0x66;
    memory
}

/// The end-to-end proof for the 16-bit stack pop.
///
/// SP starts at 0xFFFE, which is doing three jobs at once and each one was named by a review:
///
/// - **0xFFFE is 2 mod 4.** A read emitted at Dword width fails the alignment guard there and
///   side-exits, where a Word read passes. That is the ONLY way the read width is observable:
///   a Dword read that still narrows its destination write discards the extra two bytes and
///   leaves register state, memory and clocks identical to the correct form.
/// - **It carries out of bit 16 on the first advance.** `0xFFFE + 2` wraps SP to 0x0000 and must
///   leave ESP[31:16] alone; a 32-bit add would carry into it.
/// - **The reads themselves wrap.** The second pop reads at 0x0000, so an unmasked effective
///   address would land at 0x1234_0000 rather than 0x0000. The read path only gained a wrap
///   parameter with this slice; before it, `emit_ram_read_pointer` hard-coded no mask.
///
/// The destination registers are seeded with non-zero high halves, so a full 32-bit destination
/// write is caught rather than merged.
#[test]
fn direct_block_matches_the_interpreter_across_a_sixteen_bit_stack_pop() {
    const ENTRY: u32 = 0x100;

    let mut interp = sixteen_bit_stack_cpu(ENTRY);
    let mut native = sixteen_bit_stack_cpu(ENTRY);
    let mut interp_bus = TestBus::with_memory(word_pop_program());
    let mut native_bus = TestBus::with_memory(word_pop_program());
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.direct_pages_enabled = true;
    }
    native.jit_direct.set_fast_map_enabled_for_test(true);
    for page in [0x0000u32, 0xf000] {
        map_direct_page(
            &mut native,
            &mut native_bus,
            page,
            page,
            jit::fast_map::PagePermissions::UNPAGED,
            true,
            true,
        );
    }

    let reset = |cpu: &mut CpuGsw| {
        cpu.halted = false;
        cpu.set_eip(ENTRY);
        // `gpr` is in x86 ENCODING order: EAX, ECX, EDX, EBX, ESP, EBP, ESI, EDI. Seeding it
        // as if it were alphabetical is how the first version of this test named the wrong
        // register in its expectations.
        cpu.registers.gpr = [0xaaaa_0000, 0xcccc_0000, 0, 0xbbbb_0000, 0, 0, 0, 0];
        cpu.registers.set_esp(0x1234_fffa);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    };

    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);
    native.set_jit_auto_admit(true);
    reset(&mut native);
    drive(&mut native, &mut native_bus);
    reset(&mut native);
    reset(&mut interp);
    let native_before = native.perf_counters().jit_direct_insns;
    let side_before = native.perf_counters().jit_direct_side_exits;
    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(
        native_outcomes, interp_outcomes,
        "run-boundary timing differs"
    );
    assert_eq!(native, interp, "architectural or clock state differs");

    // Each destination keeps its seeded high half and takes only the popped word.
    assert_eq!(
        native.registers.eax(),
        0xaaaa_2211 + 1,
        "pop ax, then inc eax"
    );
    assert_eq!(native.registers.ebx(), 0xbbbb_4433);
    assert_eq!(native.registers.ecx(), 0xcccc_6655);
    assert_eq!(
        native.registers.edx(),
        0,
        "an untouched register stays untouched"
    );
    // Three pops of two bytes from 0xFFFE, wrapping in 16 bits only.
    assert_eq!(
        native.registers.esp(),
        0x1234_0000,
        "SP must wrap in 16 bits and ESP[31:16] must survive"
    );
    // Admission keys the block at the SECOND pop, so the first one is interpreted and three
    // instructions retire natively: pops two and three plus the trailing `inc`. The exact count
    // matters, because the carry that discriminates a 16-bit pointer advance from a 32-bit one
    // happens on the THIRD pop, and this is what proves that pop was native.
    assert_eq!(
        native.perf_counters().jit_direct_insns,
        native_before + 3,
        "pops two and three must retire natively: {:?}",
        native.perf_counters()
    );
    assert_eq!(
        native.perf_counters().jit_direct_side_exits,
        side_before,
        "a pop that side-exits proves nothing about the emitted form, and a Dword-width read \
         at this SP would side-exit on the alignment guard"
    );
}

fn pop_sp_program() -> Vec<u8> {
    let mut memory = vec![0; 0x10000];
    // Shaped like the pop fixture above and for the same reason: admission keys the block at
    // the SECOND instruction, so leading with a pop is what puts POP SP inside the compiled
    // block. Leading with an `inc` leaves the block uncompilable in this harness and the whole
    // test passes on interpreted execution.
    memory[0x100..0x108].copy_from_slice(&[
        0x66, 0x58, // pop ax
        0x66, 0x5b, // pop bx
        0x66, 0x5c, // pop sp
        0x40, // inc eax
        0xf4, // hlt
    ]);
    memory[0xfffa] = 0x11;
    memory[0xfffb] = 0x22;
    memory[0xfffc] = 0x33;
    memory[0xfffd] = 0x44;
    memory[0xfffe] = 0x55;
    memory[0xffff] = 0x66; // the word POP SP loads is 0x6655
    memory
}

/// POP SP is the aliasing case: the destination IS the stack pointer.
///
/// The interpreter advances the pointer first and assigns second, so the final SP is the LOADED
/// word rather than the advanced pointer. The Dword form of this is already pinned on both
/// backends; the Word form was not covered anywhere, and it is the one where the two writes are
/// different widths as well as the same register.
#[test]
fn a_word_pop_into_sp_takes_the_loaded_word_not_the_advanced_pointer() {
    const ENTRY: u32 = 0x100;

    let mut interp = sixteen_bit_stack_cpu(ENTRY);
    let mut native = sixteen_bit_stack_cpu(ENTRY);
    let mut interp_bus = TestBus::with_memory(pop_sp_program());
    let mut native_bus = TestBus::with_memory(pop_sp_program());
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.direct_pages_enabled = true;
    }
    native.jit_direct.set_fast_map_enabled_for_test(true);
    // Both the code page and the stack page. Mapping only the stack page leaves the block
    // uncompilable, and the test would then pass on interpreted execution.
    for page in [0x0000u32, 0xf000] {
        map_direct_page(
            &mut native,
            &mut native_bus,
            page,
            page,
            jit::fast_map::PagePermissions::UNPAGED,
            true,
            true,
        );
    }

    // The GPRs are reset too, not just SP. The native side takes one more pass than the
    // interpreter (it needs the admission pass), so anything the block accumulates has to be
    // cleared or the two diverge for a reason that has nothing to do with the pop.
    let reset = |cpu: &mut CpuGsw| {
        cpu.halted = false;
        cpu.set_eip(ENTRY);
        cpu.registers.gpr = [0; 8];
        cpu.registers.set_esp(0x1234_fffa);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    };

    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);
    // Two admission passes, not one. This block is short enough that a single pass only takes
    // the key from first-seen to compiled, so the measured run would find nothing installed and
    // the whole test would pass on interpreted execution.
    native.set_jit_auto_admit(true);
    for _ in 0..2 {
        reset(&mut native);
        drive(&mut native, &mut native_bus);
    }
    reset(&mut native);
    reset(&mut interp);
    let side_before = native.perf_counters().jit_direct_side_exits;
    let insns_before = native.perf_counters().jit_direct_insns;
    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);

    assert_eq!(native, interp, "architectural or clock state differs");
    // The loaded word wins. Reversing the two writes would leave 0x1234_0000 here.
    // The loaded word wins over the advance. Reversing the two writes would leave 0x1234_6657
    // here, and a discarded advance is exactly why this case cannot also pin the pointer width.
    assert_eq!(
        native.registers.esp(),
        0x1234_6655,
        "POP SP takes the loaded word, and the high half survives"
    );
    assert_eq!(native.perf_counters().jit_direct_side_exits, side_before);
    // Without this the test is vacuous: if the block never compiled, the interpreter would
    // produce the same correct SP and a reversed emitted order would go unnoticed.
    assert!(
        native.perf_counters().jit_direct_insns > insns_before,
        "the POP SP block was not entered natively: entries={} blocks={} insns={} attempts={}",
        native.perf_counters().jit_direct_entries,
        native.jit_direct.len(),
        native.perf_counters().jit_direct_insns,
        native.perf_counters().jit_direct_compile_attempts
    );
}

fn word_call_program() -> Vec<u8> {
    let mut memory = vec![0; 0x10000];
    memory[0x100..0x10d].copy_from_slice(&[
        0x40, // inc eax
        0x41, // inc ecx
        0x66, 0xe8, 0x06, 0x00, // call +6 at Word operand size, target 0x10c
        0x90, 0x90, 0x90, 0x90, 0x90, 0x90, // skipped
        0xf4, // hlt at 0x10c
    ]);
    memory
}

/// The end-to-end proof for the 16-bit CALL: the pushed return address is the two-byte IP, only
/// SP moves, and the target is right.
///
/// SP is kept EVEN, because a Word stack access at an odd SP takes the alignment side exit
/// unconditionally and would hand the whole instruction back to the interpreter, which produces
/// the correct answer and hides any emitter defect.
#[test]
fn direct_block_matches_the_interpreter_across_a_sixteen_bit_call() {
    const ENTRY: u32 = 0x100;

    let mut interp = sixteen_bit_stack_cpu(ENTRY);
    let mut native = sixteen_bit_stack_cpu(ENTRY);
    let mut interp_bus = TestBus::with_memory(word_call_program());
    let mut native_bus = TestBus::with_memory(word_call_program());
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.direct_pages_enabled = true;
    }
    native.jit_direct.set_fast_map_enabled_for_test(true);
    for page in [0x0000u32, 0xf000] {
        map_direct_page(
            &mut native,
            &mut native_bus,
            page,
            page,
            jit::fast_map::PagePermissions::UNPAGED,
            true,
            true,
        );
    }

    let reset = |cpu: &mut CpuGsw, bus: &mut TestBus| {
        cpu.halted = false;
        cpu.set_eip(ENTRY);
        cpu.registers.gpr = [0; 8];
        // SP at 0 so the push BORROWS across bit 16. Anywhere with headroom below it, a 16-bit
        // and a 32-bit decrement give the same answer and the fixture discriminates nothing;
        // that gap has now been found by the battery in three consecutive slices.
        cpu.registers.set_esp(0x1234_0000);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
        bus.memory[0xfff0..0x10000].fill(0);
        bus.trace = BusTrace::default();
    };

    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..2 {
        reset(&mut native, &mut native_bus);
        drive(&mut native, &mut native_bus);
    }
    reset(&mut native, &mut native_bus);
    reset(&mut interp, &mut interp_bus);
    let insns_before = native.perf_counters().jit_direct_insns;
    let side_before = native.perf_counters().jit_direct_side_exits;
    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);

    assert_eq!(native, interp, "architectural or clock state differs");
    assert_eq!(
        native_bus.memory, interp_bus.memory,
        "stack contents differ"
    );
    // Two bytes pushed, and only SP moved.
    assert_eq!(
        native.registers.esp(),
        0x1234_fffe,
        "SP wraps in 16 bits and ESP[31:16] survives"
    );
    assert_eq!(
        &native_bus.memory[0xfffe..0x10000],
        &[0x06, 0x01],
        "the pushed return address is the two-byte IP, 0x0106"
    );
    assert!(
        native.perf_counters().jit_direct_insns > insns_before,
        "the 16-bit call block was not entered natively: {:?}",
        native.perf_counters()
    );
    assert_eq!(
        native.perf_counters().jit_direct_side_exits,
        side_before,
        "a call that side-exits proves nothing about the emitted form"
    );
}

fn word_ret_program(release: u16) -> Vec<u8> {
    let mut memory = vec![0; 0x10000];
    if release == 0 {
        memory[0x100..0x104].copy_from_slice(&[
            0x40, // inc eax
            0x41, // inc ecx
            0x66, 0xc3, // ret at Word operand size
        ]);
    } else {
        memory[0x100..0x106].copy_from_slice(&[
            0x40, // inc eax
            0x41, // inc ecx
            0x66,
            0xc2, // ret imm16 at Word operand size
            release as u8,
            (release >> 8) as u8,
        ]);
    }
    memory[0x200] = 0xf4; // hlt at the return target
    // The return address at SP = 0xFFFE. The two bytes ABOVE it are poisoned: a Dword-width read
    // that somehow avoided the alignment guard would fold them into the target.
    memory[0xfffe] = 0x00;
    memory[0xffff] = 0x02; // 0x0200
    memory[0x0000] = 0xee;
    memory[0x0001] = 0xee;
    memory
}

/// The end-to-end proof for the 16-bit RET.
///
/// SP starts at 0xFFFE, which does three jobs. The advance CARRIES out of bit 16, so a 32-bit
/// add is distinguishable from a 16-bit one. The read address is 2 mod 4, so a Dword-width read
/// fails the alignment guard. And the read wraps, so an unmasked effective address would land
/// outside the fixture entirely.
///
/// The bytes above the return address are poisoned as well, because the alignment guard turns a
/// wrong read width into a SIDE EXIT rather than a wrong value: the interpreter then produces
/// the right answer and every state assertion passes. The side-exit assertion is what catches
/// that, and the poison is the second line of defence.
#[test]
fn direct_block_matches_the_interpreter_across_a_sixteen_bit_ret() {
    // Both release values. Without the non-zero one, an emitter that dropped the release
    // entirely is invisible, because a plain RET releases nothing anyway.
    for release in [0u16, 8] {
        sixteen_bit_ret_case(release);
    }
}

fn sixteen_bit_ret_case(release: u16) {
    const ENTRY: u32 = 0x100;

    let mut interp = sixteen_bit_stack_cpu(ENTRY);
    let mut native = sixteen_bit_stack_cpu(ENTRY);
    let mut interp_bus = TestBus::with_memory(word_ret_program(release));
    let mut native_bus = TestBus::with_memory(word_ret_program(release));
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.direct_pages_enabled = true;
    }
    native.jit_direct.set_fast_map_enabled_for_test(true);
    for page in [0x0000u32, 0xf000] {
        map_direct_page(
            &mut native,
            &mut native_bus,
            page,
            page,
            jit::fast_map::PagePermissions::UNPAGED,
            true,
            true,
        );
    }

    let reset = |cpu: &mut CpuGsw| {
        cpu.halted = false;
        cpu.set_eip(ENTRY);
        cpu.registers.gpr = [0; 8];
        cpu.registers.set_esp(0x1234_fffe);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    };

    // Reset BEFORE the warming drives too. The helper seeds a stack pointer of its own, and a
    // RET taken from there pops whatever happens to be at that address and never reaches the
    // halt, so the drive runs out its iteration budget instead.
    reset(&mut interp);
    reset(&mut native);
    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..2 {
        reset(&mut native);
        drive(&mut native, &mut native_bus);
    }
    reset(&mut native);
    reset(&mut interp);
    let insns_before = native.perf_counters().jit_direct_insns;
    let side_before = native.perf_counters().jit_direct_side_exits;
    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);

    assert_eq!(native, interp, "architectural or clock state differs");
    // The popped word is the return target, and SP advanced by two with the carry staying in
    // the low half.
    assert_eq!(
        native.registers.esp(),
        0x1234_0000u32.wrapping_add(u32::from(release)),
        "SP advances by two plus the release and wraps in 16 bits, keeping ESP[31:16]"
    );
    assert!(
        native.perf_counters().jit_direct_insns > insns_before,
        "the 16-bit ret block was not entered natively: {:?}",
        native.perf_counters()
    );
    assert_eq!(
        native.perf_counters().jit_direct_side_exits,
        side_before,
        "a ret that side-exits proves nothing about the emitted form, and a Dword-width read at \
         this SP would side-exit on the alignment guard"
    );
}
