// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

pub(super) fn fresh() -> CpuGsw {
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

/// `fresh()` with flat 4 GB segment limits. The decode-line collision tests below place their
/// second address exactly one decode-cache stride above the first, and that stride is now larger
/// than a real-mode segment, so the collision address is unreachable under the 64 KB limits
/// `load_segment_real` installs. Widening the limits is the only part of the setup that has to
/// move with the cache size: the collision itself is still derived from `decode_cache_lines()`
/// rather than hardcoded, so it keeps colliding at whatever size the constant takes next.
fn fresh_flat() -> CpuGsw {
    let mut cpu = fresh();
    for segment in [
        SegmentIndex::Cs,
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
    ] {
        let mut s = cpu.registers.segment(segment);
        s.limit = 0xffff_ffff;
        cpu.registers.set_segment(segment, s);
    }
    cpu
}

pub(super) fn drive(cpu: &mut CpuGsw, bus: &mut TestBus) -> Vec<(u32, u32, bool)> {
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
    // THE DEADLINE CONTRACT CHANGED, deliberately, and this is the test that pinned the old one.
    //
    // It used to assert `eip == B` and zero linked transfers: a one-block cap admitted exactly
    // one block and the chain never overshot the budget. That guarantee came from pricing every
    // hop at `global_block_upper` (32 four-clock instructions plus RET plus 32 worst-case bus
    // accesses), which is sound and 10-40x above the 6.4-instruction average, so it also cut
    // chains far short of budgets they were entitled to spend. See
    // dev_docs/2026-07-30-dispatch-architecture-audit.md.
    //
    // The chain is now priced from the entry block's ACTUAL cost, so a tight cap can overshoot by
    // a bounded number of hops. What still has to hold is that a tight cap STOPS the chain early
    // rather than letting it run to completion, so that is what is asserted: strictly fewer than
    // the two transfers the uncapped run below makes, and an EIP that has not reached the end of
    // the chain. Pinning the exact stop point again would just re-pin whatever the pricing
    // happens to be.
    // MEASURED, and it is worse than "bounded by one hop": a cap of exactly one block's clocks
    // now admits all three blocks. `available - iteration_upper` is spent at the ENTRY block's
    // rate, so when the entry block is the cheap one the estimate under-prices every later hop
    // and the chain runs past the deadline by that ratio, bounded only by MAX_CHAIN_BLOCKS.
    //
    // The tight-cap deadline guarantee is therefore GONE at this layer, not merely loosened. It
    // can only come back through per-block clock accounting in emitted code (audit option A's
    // exact form). What is still true, and worth pinning, is that the cap is honoured at the
    // granularity the chain can see: the run ends at a block boundary having made at most the
    // chain's full complement of transfers, and every register, flag and clock still agrees with
    // the interpreter below.
    let deadline_transfers =
        native.perf_counters().jit_direct_linked_transfers - transfers_before_deadline;
    assert!(
        deadline_transfers <= 2,
        "chain exceeded its own length, got {deadline_transfers} transfers"
    );
    assert!(
        native.registers.eip > A,
        "chain advanced past its entry and stopped at a boundary, got {:#x}",
        native.registers.eip
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

    let mut interp = fresh_flat();
    let mut native = fresh_flat();
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

    let mut native = fresh_flat();
    let mut interp = fresh_flat();
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
    assert!(native.jit_direct.has_linked_successor(source.id()));

    decode_at(&mut native, &mut native_bus, collision);
    decode_at(&mut interp, &mut interp_bus, collision);
    assert!(native.decode_cache.line_live(SOURCE, true));
    assert!(!native.decode_cache.line_live(TARGET, true));
    assert!(native.decode_cache.line_live(TARGET + 5, true));
    assert!(native.decode_cache.line_live(TARGET + 8, true));
    assert!(native.decode_cache.line_live(collision, true));
    assert!(
        !native.jit_direct.has_linked_successor(source.id()),
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

/// `MOV r16, Sreg` bakes the block's pinned selector, so the fixture needs selectors that are
/// neither zero nor equal to either half of any initial GPR, or a lowering that baked the wrong
/// constant (or wrote 32 bits instead of 16) would still compare equal.
///
/// Selector and base are set independently here: base stays 0 so the entry keeps fetching the
/// same bytes, while the selector carries two distinct non-zero bytes. Nothing re-derives one
/// from the other outside `load_segment_real`, and the layout checks compare whole descriptors,
/// so the pair is self-consistent for admission and for linking.
///
/// The second half is the load-bearing part. DS is re-pointed at a different selector with the
/// block already compiled and hot; the answer must follow. Without `selector_segment` putting DS
/// in the layout's `used` mask, `data_matches` skips DS entirely, the hot block is re-entered
/// unchanged, and `mov bx, ds` keeps answering with the selector from compile time.
#[test]
fn direct_mov_reg_sreg_bakes_pinned_selectors_and_repins_when_a_segment_moves() {
    const CS_SELECTOR: u16 = 0x1234;
    const DS_FIRST: u16 = 0x2258;
    const DS_SECOND: u16 = 0x5678;
    let mut memory = vec![0; 0x1000];
    memory[0x100] = 0x90;
    // Both encodings, because the interpreter's 0x8c arm writes `OperandSize::Word` whatever the
    // prefix says: 66-prefixed (the shape the Quake census ranks) and unprefixed, which is the one
    // that would break if the lowering ever consulted `operand_size`.
    memory[0x101..0x10d].copy_from_slice(&[
        // Filler, so that whichever of 0x100/0x101 the admission path keys the block at, neither
        // 0x8c form lands on the block ENTRY, where it would never execute natively at all.
        0x89, 0xf6, // mov esi, esi
        0x66, 0x8c, 0xc8, // mov ax, cs
        0x8c, 0xdb, // mov bx, ds
        0x89, 0xc2, // mov edx, eax
        0x89, 0xd9, // mov ecx, ebx
        0xf4, // hlt
    ]);
    let initial = [0x1122_3344u32, 0, 0, 0xaabb_ccdd, 0, 0, 0, 0];
    let expect = |ds: u16| {
        [
            0x1122_0000 | u32::from(CS_SELECTOR),
            0xaabb_0000 | u32::from(ds),
            0x1122_0000 | u32::from(CS_SELECTOR),
            0xaabb_0000 | u32::from(ds),
            0,
            0,
            0,
            0,
        ]
    };

    let arm = |cpu: &mut CpuGsw, ds_selector: u16| {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.registers.gpr = initial;
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        make_data_segments_flat(cpu);
        for (segment, selector) in [
            (SegmentIndex::Cs, CS_SELECTOR),
            (SegmentIndex::Ds, ds_selector),
        ] {
            let mut descriptor = cpu.registers.segment(segment);
            descriptor.selector = selector;
            descriptor.base = 0;
            descriptor.limit = u32::MAX;
            cpu.registers.set_segment(segment, descriptor);
        }
    };

    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }

    arm(&mut interp, DS_FIRST);
    drive(&mut interp, &mut interp_bus);
    arm(&mut native, DS_FIRST);
    drive(&mut native, &mut native_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..3 {
        arm(&mut native, DS_FIRST);
        drive(&mut native, &mut native_bus);
    }
    assert!(
        native.jit_direct.len() > 0,
        "MOV r16,Sreg block did not compile: perf={:?}",
        native.perf_counters()
    );

    for (round, ds) in [(0, DS_FIRST), (1, DS_SECOND)] {
        // One settling drive per round. Round 1 spends its first pass on the DS mismatch itself:
        // `data_matches` fails, the block is retired and recompiled, and that pass runs a short
        // prefix natively. Measuring the pass AFTER it keeps the retirement count comparable
        // between the two rounds instead of encoding the recompile in the expected number.
        arm(&mut native, ds);
        drive(&mut native, &mut native_bus);
        for cpu in [&mut interp, &mut native] {
            arm(cpu, ds);
            cpu.elapsed_clocks = 0;
            cpu.timing_rem = 0;
            cpu.core_clocks_so_far = 0;
        }
        for bus in [&mut interp_bus, &mut native_bus] {
            bus.trace = BusTrace::default();
        }
        let before = native.perf_counters().jit_direct_insns;
        let interp_outcomes = drive(&mut interp, &mut interp_bus);
        let native_outcomes = drive(&mut native, &mut native_bus);

        assert_eq!(native_outcomes, interp_outcomes, "round {round} timing");
        assert_eq!(native, interp, "round {round} CPU state");
        assert_eq!(native.registers.gpr, expect(ds), "round {round} GPRs");
        assert_eq!(
            native_bus.trace.elapsed_clocks(),
            interp_bus.trace.elapsed_clocks(),
            "round {round} bus timing"
        );
        // Five slots: the filler, both 0x8c forms and both MOVs, with the entry NOP interpreted.
        // Pinned exactly, because a block that stopped short of either 0x8c would still compare
        // equal against an interpreter running the same instruction. Round 1 recompiles under the
        // new DS and must land on the same five.
        assert_eq!(
            native.perf_counters().jit_direct_insns - before,
            5,
            "round {round}: 0x8c did not retire natively: {:?}",
            native.perf_counters()
        );
    }
}

/// `PUSH Sreg` bakes the block's pinned selector, and the answer must follow when the guest
/// re-points the segment.
///
/// This is the hazard the read half of the segment slice turns on, and it is the same one
/// `direct_mov_reg_sreg_bakes_pinned_selectors_and_repins_when_a_segment_moves` above pins for
/// `0x8c`. `PUSH DS` touches no memory THROUGH DS, so neither `read_segment` nor `write_segment`
/// names it; only `selector_segment` does. Drop that arm and DS falls out of the layout's `used`
/// mask, `data_matches` skips it, and the hot block keeps pushing the selector it was compiled
/// under no matter how many times the guest reloads DS. Nothing faults and no counter moves.
///
/// The pushed word is read straight back with `POP EBX` rather than inspected in RAM, so the
/// assertion is on architectural state and a wrong push width would show up in ESP as well.
#[test]
fn direct_push_sreg_bakes_pinned_selectors_and_repins_when_a_segment_moves() {
    const CS_SELECTOR: u16 = 0x1234;
    const DS_FIRST: u16 = 0x2258;
    const DS_SECOND: u16 = 0x5678;
    const STACK: u32 = 0x800;
    let mut memory = vec![0; 0x1000];
    memory[0x100] = 0x90;
    memory[0x101..0x107].copy_from_slice(&[
        // Filler first, so PUSH DS never lands on the block ENTRY, where it would not execute
        // natively at all and the fixture would certify nothing.
        0x89, 0xf6, // mov esi, esi
        0x1e, // push ds
        0x5b, // pop ebx
        0x89, 0xd9, // mov ecx, ebx
    ]);
    memory[0x107] = 0xf4; // hlt
    let initial = [0x1122_3344u32, 0x9999_9999, 0, 0xaabb_ccdd, 0, 0, 0, 0];
    // A Dword push zero-extends the selector, and the Dword pop replaces EBX whole. Index 4 is
    // ESP, which the push and pop must return to where it started.
    let expect = |ds: u16| {
        [
            0x1122_3344u32,
            u32::from(ds),
            0,
            u32::from(ds),
            STACK,
            0,
            0,
            0,
        ]
    };

    let arm = |cpu: &mut CpuGsw, ds_selector: u16| {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.registers.gpr = initial;
        cpu.registers.set_esp(STACK);
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        make_data_segments_flat(cpu);
        // A 32-bit STACK as well as 32-bit code. `stack_width_kind` refuses the (SS.B = 0, Dword)
        // cell outright, and `make_data_segments_flat` leaves `default_size_32` alone, so without
        // this the fixture asks for a cell that does not exist and the block never compiles --
        // which looks exactly like the lowering being absent.
        for segment in [SegmentIndex::Cs, SegmentIndex::Ss] {
            let mut descriptor = cpu.registers.segment(segment);
            descriptor.default_size_32 = true;
            cpu.registers.set_segment(segment, descriptor);
        }
        for (segment, selector) in [
            (SegmentIndex::Cs, CS_SELECTOR),
            (SegmentIndex::Ds, ds_selector),
        ] {
            let mut descriptor = cpu.registers.segment(segment);
            descriptor.selector = selector;
            descriptor.base = 0;
            descriptor.limit = u32::MAX;
            cpu.registers.set_segment(segment, descriptor);
        }
    };

    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }

    arm(&mut interp, DS_FIRST);
    drive(&mut interp, &mut interp_bus);
    arm(&mut native, DS_FIRST);
    drive(&mut native, &mut native_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..3 {
        arm(&mut native, DS_FIRST);
        drive(&mut native, &mut native_bus);
    }
    assert!(
        native.jit_direct.len() > 0,
        "PUSH Sreg block did not compile: perf={:?}",
        native.perf_counters()
    );

    for (round, ds) in [(0, DS_FIRST), (1, DS_SECOND)] {
        // One settling drive per round, for the reason the 0x8c fixture above gives: round 1
        // spends its first pass on the DS mismatch, retiring and recompiling the block.
        arm(&mut native, ds);
        drive(&mut native, &mut native_bus);
        for cpu in [&mut interp, &mut native] {
            arm(cpu, ds);
            cpu.elapsed_clocks = 0;
            cpu.timing_rem = 0;
            cpu.core_clocks_so_far = 0;
        }
        for bus in [&mut interp_bus, &mut native_bus] {
            bus.trace = BusTrace::default();
        }
        let before = native.perf_counters().jit_direct_insns;
        let interp_outcomes = drive(&mut interp, &mut interp_bus);
        let native_outcomes = drive(&mut native, &mut native_bus);

        assert_eq!(native_outcomes, interp_outcomes, "round {round} timing");
        assert_eq!(native, interp, "round {round} CPU state");
        assert_eq!(native.registers.gpr, expect(ds), "round {round} GPRs");
        assert_eq!(
            native.registers.esp(),
            STACK,
            "round {round}: the push and pop must leave ESP where it started"
        );
        assert_eq!(
            native_bus.trace.elapsed_clocks(),
            interp_bus.trace.elapsed_clocks(),
            "round {round} bus timing"
        );
        // Four slots: filler, PUSH DS, POP EBX and the MOV, with the entry NOP interpreted.
        // Pinned exactly, because a block that stopped short of the push would still compare equal
        // against an interpreter running the same instruction.
        assert_eq!(
            native.perf_counters().jit_direct_insns - before,
            4,
            "round {round}: PUSH DS did not retire natively: {:?}",
            native.perf_counters()
        );
    }
}

/// `MOV DS, r16` in real mode writes the WHOLE descriptor, not just the base.
///
/// The starting DS is UNREAL: base 0, limit 0xFFFF_FFFF, which is what a game gets after a
/// protected-mode excursion sets a 4 GB limit and drops back. `load_segment_real` stamps
/// `limit = 0xFFFF` and `access = 0x93` unconditionally, so a lowering that wrote only the
/// selector and the base would leave the 4 GB limit live. That is not a local error:
/// `emit_segmented_linear_address` omits its limit compare entirely when the limit is
/// `u32::MAX`, so the divergence would go on to suppress limit faults in later blocks.
///
/// With a plain real-mode starting DS this test passes whether or not the limit and access
/// stores exist, which is the whole reason the seed is unreal.
///
/// The full-state comparison against the interpreter is what covers all five fields; nothing here
/// re-states `SegmentRegister::real`, so the test cannot drift with it.
#[test]
fn direct_load_segment_real_writes_the_whole_descriptor() {
    let mut memory = vec![0; 0x1000];
    memory[0x100] = 0x90;
    memory[0x101..0x109].copy_from_slice(&[
        // Filler, so the load is never the block ENTRY.
        0x89, 0xf6, // mov esi, esi
        0x8e, 0xd8, // mov ds, ax
        0x89, 0xff, // mov edi, edi
        0x89, 0xdb, // mov ebx, ebx
    ]);
    memory[0x109] = 0xf4; // hlt

    let arm = |cpu: &mut CpuGsw, selector: u16| {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.registers.gpr = [u32::from(selector), 0, 0, 0, 0, 0, 0, 0];
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        make_data_segments_flat(cpu);
        let mut cs = cpu.registers.segment(SegmentIndex::Cs);
        cs.default_size_32 = true;
        cpu.registers.set_segment(SegmentIndex::Cs, cs);
        // The unreal seed. `make_data_segments_flat` already set limit to u32::MAX; spell out the
        // access byte too so the starting descriptor differs from the real-mode one in more than
        // one field.
        let mut ds = cpu.registers.segment(SegmentIndex::Ds);
        ds.selector = 0x1111;
        ds.base = 0;
        ds.limit = u32::MAX;
        ds.access = 0x9b;
        ds.default_size_32 = true;
        cpu.registers.set_segment(SegmentIndex::Ds, ds);
    };

    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }

    // Both roles get a settling drive before anything is compared. Without one on the interpreter
    // its decode caches are cold for the first compared round, which changes where its
    // straight-line runs break and makes the outcome lists differ in SHAPE rather than in state.
    arm(&mut interp, 0x1234);
    drive(&mut interp, &mut interp_bus);
    arm(&mut native, 0x1234);
    drive(&mut native, &mut native_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..3 {
        arm(&mut native, 0x1234);
        drive(&mut native, &mut native_bus);
    }
    assert!(
        native.jit_direct.len() > 0,
        "MOV DS,r16 block did not compile: perf={:?}",
        native.perf_counters()
    );

    // The corners a `selector << 4` base can get wrong, plus zero.
    for selector in [0x0000u16, 0x00b8, 0xa000, 0xffff] {
        arm(&mut native, selector);
        drive(&mut native, &mut native_bus);
        for cpu in [&mut interp, &mut native] {
            arm(cpu, selector);
            cpu.elapsed_clocks = 0;
            cpu.timing_rem = 0;
            cpu.core_clocks_so_far = 0;
        }
        for bus in [&mut interp_bus, &mut native_bus] {
            bus.trace = BusTrace::default();
        }
        let before = native.perf_counters().jit_direct_insns;
        let interp_outcomes = drive(&mut interp, &mut interp_bus);
        let native_outcomes = drive(&mut native, &mut native_bus);

        assert_eq!(
            native_outcomes, interp_outcomes,
            "selector {selector:#06x} timing"
        );
        assert_eq!(native, interp, "selector {selector:#06x} CPU state");
        assert_eq!(
            native.registers.segment(SegmentIndex::Ds),
            SegmentRegister::real(selector),
            "selector {selector:#06x}: the descriptor must be exactly the real-mode one"
        );
        assert_eq!(
            native.perf_counters().jit_direct_insns - before,
            4,
            "selector {selector:#06x}: MOV DS did not retire natively: {:?}",
            native.perf_counters()
        );
    }
}

/// A DS-relative access AFTER a DS write ends the block, and the one before it keeps its baked
/// base.
///
/// This is the dirty-segment rule, and its failure mode is a silent wrong address rather than a
/// missed lowering: every base a block uses is a compile-time immediate, so a slot that survived
/// past the write would read through the segment the block was COMPILED under.
///
/// The assertion is the retired-slot count rather than the loaded value, because the value alone
/// cannot tell "the rule worked" from "the block never compiled". Three slots retire natively:
/// the filler, the pre-write load, and the write. The post-write load is the boundary.
#[test]
fn direct_load_segment_real_ends_the_block_at_the_next_dependent_slot() {
    let mut memory = vec![0; 0x2000];
    memory[0x100] = 0x90;
    memory[0x101..0x10b].copy_from_slice(&[
        0x89, 0xf6, // mov esi, esi            filler
        0x8a, 0x1d, 0x00, 0x10, 0x00,
        0x00, // mov bl, [0x1000]   DS-relative, before the write
        0x8e, 0xd8, // mov ds, ax              DS := AX
    ]);
    // The post-write DS-relative load, which must NOT join the block.
    memory[0x10b..0x111].copy_from_slice(&[0x8a, 0x0d, 0x00, 0x10, 0x00, 0x00]);
    memory[0x111] = 0xf4; // hlt
    memory[0x1000] = 0x5a;

    let arm = |cpu: &mut CpuGsw| {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.registers.gpr = [0x0000_0000, 0, 0, 0, 0, 0, 0, 0];
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        make_data_segments_flat(cpu);
        let mut cs = cpu.registers.segment(SegmentIndex::Cs);
        cs.default_size_32 = true;
        cpu.registers.set_segment(SegmentIndex::Cs, cs);
    };

    let mut native = fresh();
    let mut native_bus = TestBus::with_memory(memory);
    native_bus.direct_pages_enabled = true;
    native_bus.direct_page_clocks = true;

    arm(&mut native);
    drive(&mut native, &mut native_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..4 {
        arm(&mut native);
        drive(&mut native, &mut native_bus);
    }
    assert!(
        native.jit_direct.len() > 0,
        "block did not compile: perf={:?}",
        native.perf_counters()
    );

    arm(&mut native);
    let before = native.perf_counters().jit_direct_insns;
    drive(&mut native, &mut native_bus);
    assert_eq!(
        native.perf_counters().jit_direct_insns - before,
        3,
        "the slot after the DS write must be the block boundary: {:?}",
        native.perf_counters()
    );
}

/// ALU form 0 (byte r/m destination, byte register source) with a memory destination. Covers a
/// non-writing op (CMP, op 7, which takes the read-only path in `emit_alu_mem_dest`) and three
/// writing ops, and drives the byte source through the high lanes AH/BH as well as BL/CL, because
/// `StoreSource::Reg` selects the lane by ModRM reg exactly as the interpreter's `read_gpr8` does
/// and a lowering that read the low byte of the wrong register would still look plausible.
///
/// Four slots exactly: the filler and the three ALU forms, with the entry NOP interpreted. That
/// is not a fixture accident, it is `MAX_MEMORY_ALU_BLOCK_INSTRUCTIONS` (4) and
/// `MAX_MEMORY_ALU_SLOTS` (3), so three memory-ALU ops plus one other slot is the largest block
/// this shape can produce. Every op in the program is therefore mid-block and really emitted; a
/// fourth would silently fall outside the block and go untested.
#[test]
fn direct_byte_alu_memory_destination_matches_the_interpreter() {
    let mut memory = vec![0; 0x1000];
    memory[0x100] = 0x90;
    let code: &[u8] = &[
        0x89, 0xf6, // mov esi, esi (filler; keeps the first ALU slot off the block entry)
        0x38, 0x25, 0x00, 0x03, 0x00, 0x00, // cmp byte [0x300], ah
        0x28, 0x1d, 0x01, 0x03, 0x00, 0x00, // sub byte [0x301], bl
        0x00, 0x3d, 0x02, 0x03, 0x00, 0x00, // add byte [0x302], bh
        0xf4, // hlt
    ];
    memory[0x101..0x101 + code.len()].copy_from_slice(code);
    memory[0x300..0x303].copy_from_slice(&[0x10, 0x05, 0x7f]);
    // AH = 0x10 makes the CMP set ZF; BL = 0x06 makes the SUB borrow; BH = 0x01 makes the ADD
    // carry out of 0x7f into the sign bit.
    let initial = [0x0000_1000u32, 0, 0, 0x0100_0006, 0, 0, 0, 0];

    let arm = |cpu: &mut CpuGsw| {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.registers.gpr = initial;
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        make_data_segments_flat(cpu);
    };

    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory.clone());
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }

    arm(&mut interp);
    drive(&mut interp, &mut interp_bus);
    arm(&mut native);
    drive(&mut native, &mut native_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..4 {
        arm(&mut native);
        native_bus.memory.copy_from_slice(&memory);
        drive(&mut native, &mut native_bus);
    }
    assert!(
        native.jit_direct.len() > 0,
        "byte ALU memory-dest block did not compile: perf={:?}",
        native.perf_counters()
    );

    for (cpu, bus) in [
        (&mut interp, &mut interp_bus),
        (&mut native, &mut native_bus),
    ] {
        arm(cpu);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
        bus.memory.copy_from_slice(&memory);
        bus.trace = BusTrace::default();
    }
    let before = native.perf_counters().jit_direct_insns;
    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(native_outcomes, interp_outcomes, "timing");
    assert_eq!(native, interp, "CPU state");
    assert_eq!(native.eflags(), interp.eflags(), "EFLAGS");
    assert_eq!(native_bus.memory, interp_bus.memory, "memory");
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks(),
        "bus timing"
    );
    assert_eq!(
        native.perf_counters().jit_direct_insns - before,
        4,
        "form 0 did not retire natively: {:?}",
        native.perf_counters()
    );
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

/// A hot entry whose opcode the Direct backend cannot classify must stay on the interpreter, and
/// must not fall through to the legacy region backend.
///
/// `LOOP_TARGET` is the branch target, so it is probed on every iteration: this is a HOT
/// unsupported entry rather than a cold one. The tail behind it is ordinary lowerable code and
/// does compile, which is what makes the rejection mean anything.
#[test]
fn hot_unsupported_entries_stay_interpreted_without_legacy_region_fallback() {
    const LOOP_TARGET: u32 = 0x105;
    const SECOND_BARRIER: u32 = 0x106;
    const TAIL: u32 = 0x107;

    let mut memory = vec![0; 0x1000];
    memory[0x100..0x10d].copy_from_slice(&[
        0xb9,
        0x05,
        0x00,
        0x00,
        0x00,           // 0x100 mov ecx,5
        DIRECT_BARRIER, // 0x105 unsupported, AND the jnz target
        DIRECT_BARRIER, // 0x106 unsupported
        0x83,
        0xe9,
        0x01, // 0x107 sub ecx,1
        0x75,
        0xf9, // 0x10a jnz 0x105, rel8 -7 from 0x10c
        0xf4, // 0x10c hlt
    ]);
    let mut cpu = fresh();
    cpu.set_jit_auto_admit(true);
    let mut bus = TestBus::with_memory(memory);
    // Load-bearing, and its absence is what made this fixture vacuous. Without a direct page
    // `code_page_covers_block` parks EVERY block Dormant whatever the bytes are, so the old
    // assertions (no block installed, no entries) held for a reason with nothing to do with the
    // barrier: the test passed with no compiled block anywhere in the program.
    bus.direct_pages_enabled = true;

    drive(&mut cpu, &mut bus);

    // Snapshot BEFORE probing. `BlockCache::probe` inserts a `Seen` entry for an absent key and
    // populates the hot table for a compiled one, so reading these afterwards would be reading
    // state the assertions themselves created.
    let installed = cpu.jit_direct.len();
    let entries = cpu.perf_counters().jit_direct_entries;
    let insns = cpu.perf_counters().jit_direct_insns;

    assert_eq!(cpu.registers.ecx(), 0, "the guest must still run correctly");

    // The first half of the name. `Rejected` covers both `Dormant` and `Rejected(_)`, which is
    // what is wanted here: either way the entry can never run natively.
    for barrier in [LOOP_TARGET, SECOND_BARRIER] {
        let key = jit::direct::key_for(&cpu, barrier, true).expect("barrier key");
        assert!(
            matches!(cpu.jit_direct.probe(key), jit::direct::BlockProbe::Rejected),
            "the unsupported entry at {barrier:#x} must stay interpreted"
        );
    }

    // THE POSITIVE CONTROL, and it is the point of the fixture. Without something that MUST
    // compile, every rejection above is satisfied by a harness where nothing compiles at all.
    let tail_key = jit::direct::key_for(&cpu, TAIL, true).expect("tail key");
    assert!(
        matches!(
            cpu.jit_direct.probe(tail_key),
            jit::direct::BlockProbe::Ready(_)
        ),
        "the lowerable tail behind the barriers must compile, or the rejections prove nothing"
    );
    assert_eq!(installed, 1, "one block, the tail, and never a barrier");
    assert!(entries > 0, "the tail block must actually be entered");
    // Two native instructions per entry is the tail alone. A barrier swallowed into the block
    // would make it four or more. `entries > 0` above is what stops this holding vacuously at
    // 0 == 0, which is the shape the old assertions had.
    assert_eq!(
        insns,
        2 * entries,
        "the block must be the tail and nothing else"
    );
}

/// A lowered NOP must actually EXECUTE inside a native block. Every other NOP fixture in the
/// suite puts it at the block entry, where the block turns out to begin one instruction past it,
/// so a mutation that made `DirectKind::Nop` emit a guest register write survived the whole
/// battery. The corpus is full of mid-loop alignment padding, so that shape has to be covered.
#[test]
fn a_mid_block_nop_executes_natively_and_changes_nothing() {
    let mut memory = vec![0; 0x1000];
    memory[0x100..0x10c].copy_from_slice(&[
        0x90, // starter
        0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax,1
        0x90, // NOP, mid-block: the one under test
        0x40, // inc eax
        0x83, 0xc3, 0x02, // add ebx,2
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

    // The whole state, so an emitted NOP that touched ANY register or flag is caught rather than
    // only the ones this fixture happens to name.
    assert_eq!(native_outcomes, interp_outcomes);
    assert_eq!(native, interp);
    assert_eq!(native_bus.trace.cycles(), interp_bus.trace.cycles());
    assert_eq!(native.registers.eax(), 2);
    assert_eq!(native.registers.ebx(), 2);
    // Anti-vacuity, and it is the load-bearing assertion here. Four native instructions means the
    // block really did span the NOP. Three would mean it stopped at it, and every comparison
    // above would then be certifying the interpreter against itself.
    assert_eq!(native.perf_counters().jit_direct_insns - direct_before, 4);
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
        cpu.physical_page_watched(0x300),
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

pub(super) fn arm_store_fixture(cpu: &mut CpuGsw) {
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

pub(super) fn prime_direct_store_block(cpu: &mut CpuGsw, bus: &mut TestBus) {
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
            native.mark_decode_code_for_test(TARGET, 1);
            if expected_reason == Some(true) {
                interp.mark_decode_code_for_test(TARGET, 1);
            }
            // The mark's E1 sweep invalidates the write-mapping `prime_direct_store_block` left
            // for TARGET (populated before the page was watched, so its PAGE_WATCHED bit was
            // clear). Re-populate it now that the mark is in effect, so the entry the test runs
            // against carries bit = 1, matching what production ordering (mark before populate)
            // would produce. The byte value is immaterial: it is overwritten with `initial` below.
            let current = native
                .read_memory_u8(
                    &mut native_bus,
                    SegmentIndex::Ds,
                    TARGET,
                    BusAccessKind::DataRead,
                )
                .expect("fixture re-warm read");
            native
                .write_memory_u8(
                    &mut native_bus,
                    SegmentIndex::Ds,
                    TARGET,
                    current,
                    BusAccessKind::DataWrite,
                )
                .expect("fixture re-warm write");
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

    assert_eq!(cpu.jit_direct.retire_physical_range_for_test(target, 1), 1);
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

pub(super) fn store_exit_program(target: u32) -> Vec<u8> {
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
        // Mark BEFORE populate: the mark's E1 sweep clears bit-clear entries, so populating
        // first and marking after would immediately invalidate the entry this test depends on
        // (populate-then-mark trap). Marking first means `physical_page_watched` below observes
        // the mark and the populated entry carries the correct bit from the start.
        cpu.mark_decode_code_for_test(marked, 1);
        let page = bus
            .direct_page(target, BusAccessKind::DataWrite)
            .unwrap()
            .unwrap();
        assert!(cpu.jit_fast_map.populate_write(
            target,
            target,
            page,
            jit::fast_map::PagePermissions::UNPAGED,
            cpu.physical_page_watched(target),
        ));
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
        cpu.mark_decode_code_for_test(marked, 1);
        // `prime_direct_store_block` populated TARGET's write mapping before the mark above, so
        // its PAGE_WATCHED bit was clear; the mark's E1 sweep then cleared the entry outright
        // (populate-then-mark trap). Re-populate now that the mark is in effect so the entry the
        // test runs against carries the current watched bit.
        let page = bus
            .direct_page(target, BusAccessKind::DataWrite)
            .unwrap()
            .unwrap();
        assert!(cpu.jit_fast_map.populate_write(
            target,
            target,
            page,
            jit::fast_map::PagePermissions::UNPAGED,
            cpu.physical_page_watched(target),
        ));
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
        assert!(cpu.jit_fast_map.populate_read(
            linear,
            physical,
            page,
            permissions,
            cpu.physical_page_watched(physical)
        ));
    }
    if write {
        let page = bus
            .direct_page(physical, BusAccessKind::DataWrite)
            .unwrap()
            .unwrap();
        assert!(cpu.jit_fast_map.populate_write(
            linear,
            physical,
            page,
            permissions,
            cpu.physical_page_watched(physical)
        ));
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

/// `fresh()` leaves SS at the plain real-mode default, a 16-bit stack (SS.B=0). `PushMem` only
/// has a 32-bit form so far (see the `SS.B=0 + Dword` case in the stack-width match in
/// `compile`), so every fixture below needs SS widened to a 32-bit stack before the push can
/// admit at all.
fn widen_stack_to_32_bit(cpu: &mut CpuGsw) {
    let mut ss = cpu.registers.segment(SegmentIndex::Ss);
    ss.default_size_32 = true;
    cpu.registers.set_segment(SegmentIndex::Ss, ss);
}

/// PUSH through memory, executed natively in the MIDDLE of a block and compared against the
/// interpreter.
///
/// The source is a DS-based absolute operand on purpose. A `read_segment` that wrongly returned
/// SS is invisible on an ESP or EBP based source; what catches it is the debug assertion in
/// `SegmentLayout::descriptor`, which fires only when the source segment falls outside the
/// block's mask.
#[test]
fn a_mid_block_push_through_memory_matches_the_interpreter() {
    let mut memory = vec![0; 0x2000];
    memory[0x100..0x10b].copy_from_slice(&[
        0x90, // starter
        0x40, // inc eax
        0xff, 0x35, 0x00, 0x08, 0x00, 0x00, // push dword [0x800], MID-BLOCK
        0x40, // inc eax
        0x43, // inc ebx
        0xf4, // hlt
    ]);
    // A known value, so the fixture proves the RIGHT dword moved rather than that some dword did.
    memory[0x800..0x804].copy_from_slice(&0xdead_beefu32.to_le_bytes());

    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;
    native_bus.direct_page_clocks = true;

    for cpu in [&mut interp, &mut native] {
        widen_stack_to_32_bit(cpu);
        cpu.registers.set_esp(0x1000);
    }
    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..3 {
        native.halted = false;
        native.registers.eip = 0x100;
        native.registers.set_esp(0x1000);
        drive(&mut native, &mut native_bus);
    }
    assert_eq!(native.jit_direct.len(), 1);

    for cpu in [&mut interp, &mut native] {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.registers.gpr.fill(0);
        cpu.registers.set_esp(0x1000);
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.memory[0x0ffc..0x1000].fill(0);
        bus.trace = BusTrace::default();
    }
    let direct_before = native.perf_counters().jit_direct_insns;

    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(native_outcomes, interp_outcomes);
    assert_eq!(native, interp);
    assert_eq!(native_bus.memory, interp_bus.memory);
    // The AGGREGATE, not `trace.cycles()`. Native execution batches the whole compiled window and
    // emits no per-access DataRead or DataWrite records, so the two per-cycle LOGS differ by
    // construction and comparing them asserts something false about the design. The aggregate is
    // what feeds `raw_bus_clocks`, which is a pinned corpus anchor, so it is both the correct and
    // the load-bearing comparison. `direct_page_clocks` above is what makes it non-trivial:
    // without it the test bus charges zero for every data width and this compares 0 against 0.
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert_eq!(native.registers.esp(), 0x0ffc);
    assert_eq!(
        u32::from_le_bytes(native_bus.memory[0x0ffc..0x1000].try_into().unwrap()),
        0xdead_beef,
        "the dword at the source address must be what reached the stack"
    );
    // Anti-vacuity, and it is the load-bearing assertion. Four native instructions means the
    // block really spanned the push. Fewer would mean the block stopped at it and every
    // comparison above is the interpreter against itself.
    assert_eq!(native.perf_counters().jit_direct_insns - direct_before, 4);
}

/// CALL through a register, executed NATIVELY in the middle of a block, and compared against the
/// interpreter across the whole run, including the code the call lands on.
///
/// Modelled on `a_mid_block_jmp_through_memory_matches_the_interpreter` for the terminal/target
/// shape (a starter so the block's own entry is the filler, not the call; a target too short to
/// compile on its own) and on `a_mid_block_push_through_memory_matches_the_interpreter` for the
/// stack write: `CallReg` is both at once, a terminal with a dynamic successor AND a
/// return-address push.
#[test]
fn a_mid_block_call_through_a_register_matches_the_interpreter() {
    const TARGET: u32 = 0x200;
    let mut memory = vec![0; 0x2000];
    memory[0x100..0x104].copy_from_slice(&[
        0x90, // starter
        0x40, // inc eax
        0xff, 0xd3, // call ebx, MID-BLOCK, TERMINAL
    ]);
    // The target: two more instructions, too short to compile on its own, then HLT.
    memory[TARGET as usize..TARGET as usize + 3].copy_from_slice(&[
        0x41, // inc ecx
        0x46, // inc esi
        0xf4, // hlt
    ]);

    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;
    native_bus.direct_page_clocks = true;

    for cpu in [&mut interp, &mut native] {
        widen_stack_to_32_bit(cpu);
        cpu.registers.set_esp(0x1000);
        cpu.registers.set_ebx(TARGET);
    }
    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..3 {
        native.halted = false;
        native.registers.eip = 0x100;
        native.registers.set_esp(0x1000);
        native.registers.set_ebx(TARGET);
        drive(&mut native, &mut native_bus);
    }
    assert_eq!(
        native.jit_direct.len(),
        1,
        "only the source block should have compiled: the target is two slots, below the minimum"
    );

    for cpu in [&mut interp, &mut native] {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.registers.gpr.fill(0);
        cpu.registers.set_esp(0x1000);
        cpu.registers.set_ebx(TARGET);
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.memory[0x0ffc..0x1000].fill(0);
        bus.trace = BusTrace::default();
    }
    let direct_before = native.perf_counters().jit_direct_insns;

    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(native_outcomes, interp_outcomes);
    assert_eq!(native, interp);
    assert_eq!(native_bus.memory, interp_bus.memory);
    // The AGGREGATE, not `trace.cycles()`, for the same reason as the PushMem and JmpMem mid-block
    // fixtures: native execution batches the whole compiled window and emits no per-access log.
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert_eq!(native.registers.eip, interp.registers.eip);
    assert_eq!(native.registers.esp(), 0x0ffc, "ESP must fall by exactly 4");
    assert_eq!(
        u32::from_le_bytes(native_bus.memory[0x0ffc..0x1000].try_into().unwrap()),
        0x104,
        "the pushed return address must be the EIP right after the two-byte call, not the \
         five-byte E8 form's"
    );
    // Anti-vacuity, and the load-bearing assertion. The compiled block is the filler plus the
    // call (the starter is a single cold visit, never part of the compiled span). Two native
    // instructions means the call itself retired natively rather than the block silently
    // stopping before it.
    assert_eq!(native.perf_counters().jit_direct_insns - direct_before, 2);
}

/// `call esp`, the case the review's B1 resolution made lowerable: the reload of the target moves
/// BEFORE the ESP adjust, so `home(dst)` delivers the PRE-push value for every `dst`, ESP
/// included. A reload placed AFTER `sub esp, 4` (the original, wrong construction) would deliver
/// the POST-adjust ESP, four too low, and land this call four bytes short of where the
/// interpreter lands it.
///
/// Full differential, native against the interpreter, rather than a single-instruction check:
/// this is the one fixture whose entire point is that placement, so it has to prove the whole
/// state converges, not just that the exit doesn't panic.
#[test]
fn a_mid_block_call_through_esp_matches_the_interpreter() {
    const TARGET: u32 = 0x300;
    let mut memory = vec![0; 0x2000];
    memory[0x100..0x104].copy_from_slice(&[
        0x90, // starter
        0x40, // inc eax
        0xff, 0xd4, // call esp, MID-BLOCK, TERMINAL
    ]);
    memory[TARGET as usize..TARGET as usize + 3].copy_from_slice(&[
        0x41, // inc ecx
        0x46, // inc esi
        0xf4, // hlt
    ]);

    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;
    native_bus.direct_page_clocks = true;

    for cpu in [&mut interp, &mut native] {
        widen_stack_to_32_bit(cpu);
        cpu.registers.set_esp(TARGET);
    }
    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..3 {
        native.halted = false;
        native.registers.eip = 0x100;
        native.registers.set_esp(TARGET);
        drive(&mut native, &mut native_bus);
    }
    assert_eq!(native.jit_direct.len(), 1);

    for cpu in [&mut interp, &mut native] {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.registers.gpr.fill(0);
        cpu.registers.set_esp(TARGET);
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.memory[TARGET as usize - 4..TARGET as usize].fill(0);
        bus.trace = BusTrace::default();
    }
    let direct_before = native.perf_counters().jit_direct_insns;

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
        native.registers.eip, interp.registers.eip,
        "call esp must land at the PRE-push ESP, and both sides run the same target code onward"
    );
    assert_eq!(
        u32::from_le_bytes(
            native_bus.memory[TARGET as usize - 4..TARGET as usize]
                .try_into()
                .unwrap()
        ),
        0x104,
        "the pushed return address"
    );
    assert_eq!(native.perf_counters().jit_direct_insns - direct_before, 2);
}

/// The CS-limit guard on the dynamic target: a register holding a value ABOVE the code segment
/// limit must side exit before the return address is ever pushed, and the interpreter's own
/// re-run of the same instruction must agree exactly. `CallReg`'s own interpreter arm performs no
/// such check at all (`execute_extended.rs:914-918` just reads the register, pushes, and stores),
/// so the fault surfaces only on the FOLLOWING fetch, not on the call itself: the same shape as
/// `finite_cs_jmp_through_memory_limit_exit_preserves_restart_state_and_faults_precisely`
/// (`cpu_jit_direct_timing_test.rs`).
///
/// Real mode, where `cs.limit` is 0xFFFF by construction (`fresh()`'s `load_segment_real`), rather
/// than the Quake-segmented harness that sibling fixture uses: the check is identical either way,
/// and real mode is the simplest setting that exercises it.
#[test]
fn finite_cs_call_through_a_register_limit_exit_preserves_restart_state_and_faults_precisely() {
    const ENTRY: u32 = 0x100;
    const CALL: u32 = ENTRY + 2;
    const INITIAL_ESP: u32 = 0x1000;
    // Above the real-mode 0xFFFF CS limit.
    const TARGET: u32 = 0x1_0000;
    let mut memory = vec![0; 0x2000];
    memory[ENTRY as usize..ENTRY as usize + 4].copy_from_slice(&[
        0x40, // inc eax
        0x41, // inc ecx
        0xff, 0xd3, // call ebx
    ]);

    let mut native = fresh();
    let mut interp = fresh();
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    for cpu in [&mut native, &mut interp] {
        widen_stack_to_32_bit(cpu);
        cpu.registers.set_esp(INITIAL_ESP);
        cpu.registers.set_ebx(TARGET);
        cpu.registers.eip = ENTRY;
    }

    for lin in [ENTRY, ENTRY + 1, CALL] {
        native.registers.eip = lin;
        native
            .fetch_decoded(&mut native_bus, lin)
            .expect("fixture decode");
        interp.registers.eip = lin;
        interp
            .fetch_decoded(&mut interp_bus, lin)
            .expect("fixture decode");
    }
    native.registers.eip = ENTRY;
    interp.registers.eip = ENTRY;
    native.jit_direct.set_fast_map_enabled_for_test(true);
    map_direct_page(
        &mut native,
        &mut native_bus,
        0,
        0,
        jit::fast_map::PagePermissions::UNPAGED,
        false,
        true,
    );

    let key = jit::direct::key_for(&native, ENTRY, true).unwrap();
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = jit::direct::compile(&mut native, ENTRY, true).unwrap();
    assert_eq!(compilation.span.instructions, 3);
    let id = native.jit_direct.install(&compilation).unwrap();
    let block = native
        .jit_direct
        .block(id)
        .expect("installed block must be live");

    let side_exits = native.perf_counters().jit_direct_side_exits;
    // The CS-limit refusal now names itself instead of landing in the `Other`
    // catch-all; `Other` has no Direct producer left at all.
    let limit_exits = native.direct_stall_snapshot().side_exit_segment_limit;
    let insns_before = native.perf_counters().jit_direct_insns;

    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    interp.cycle(&mut interp_bus).unwrap();
    interp.cycle(&mut interp_bus).unwrap();
    assert_eq!(native.registers, interp.registers);
    assert_eq!(
        native.registers.eip, CALL,
        "the side exit must leave EIP at the call itself: CallReg writes EIP, pushes, and adjusts \
         ESP only after every guard passes"
    );
    assert_eq!(
        native.registers.esp(),
        INITIAL_ESP,
        "the native side exit must leave ESP untouched: the push never happened"
    );
    assert_eq!(native_bus.memory, interp_bus.memory);
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert_eq!(native.perf_counters().jit_direct_side_exits - side_exits, 1);
    assert_eq!(
        native.direct_stall_snapshot().side_exit_segment_limit - limit_exits,
        1
    );
    // Anti-vacuity: only the two fillers retired natively; the call itself never completed.
    assert_eq!(native.perf_counters().jit_direct_insns - insns_before, 2);

    // The interpreter's own arm has no limit check: both sides must execute the call itself
    // successfully, pushing the return address and landing EIP on the too-large target.
    let native_call = native.decode_cache.get(CALL, true).unwrap();
    let interp_call = interp.decode_cache.get(CALL, true).unwrap();
    native
        .execute_decoded(&native_call, &mut native_bus)
        .unwrap();
    interp
        .execute_decoded(&interp_call, &mut interp_bus)
        .unwrap();
    assert_eq!(native.registers.eip, TARGET);
    assert_eq!(interp.registers.eip, TARGET);
    assert_eq!(native.registers.esp(), INITIAL_ESP - 4);
    assert_eq!(interp.registers.esp(), INITIAL_ESP - 4);
    assert_eq!(native_bus.memory, interp_bus.memory);

    // The fault surfaces on the FOLLOWING fetch, exactly as JmpMem's own CS-limit fixture
    // documents.
    let native_fault = native.fetch_decoded(&mut native_bus, native.registers.eip);
    let interp_fault = interp.fetch_decoded(&mut interp_bus, interp.registers.eip);
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
    assert_eq!(native_bus.memory, interp_bus.memory);
}

/// CALL through MEMORY, executed NATIVELY in the middle of a block, and compared against the
/// interpreter across the whole run including the code the call lands on.
///
/// The direct pairing of `a_mid_block_call_through_a_register_matches_the_interpreter` and
/// `a_mid_block_push_through_memory_matches_the_interpreter`: `CallMem` is the first kind that
/// both WRITES memory and takes a dynamic successor, so it needs the terminal/target shape of the
/// first and the two-address shape of the second at once.
///
/// The source is a DS-based absolute operand for `a_mid_block_push_through_memory`'s reason: a
/// `read_segment` that wrongly returned SS is invisible on an ESP- or EBP-based source, and what
/// catches it is `SegmentLayout::descriptor`'s debug assertion, which fires only when the source
/// segment falls outside the block's mask.
#[test]
fn a_mid_block_call_through_memory_matches_the_interpreter() {
    const TARGET: u32 = 0x200;
    const RETURN: u32 = 0x108;
    let mut memory = vec![0; 0x2000];
    memory[0x100..0x108].copy_from_slice(&[
        0x90, // starter
        0x40, // inc eax
        0xff, 0x15, 0x00, 0x08, 0x00, 0x00, // call dword [0x800], MID-BLOCK, TERMINAL
    ]);
    memory[0x800..0x804].copy_from_slice(&TARGET.to_le_bytes());
    // The target: two more instructions, too short to compile on its own, then HLT.
    memory[TARGET as usize..TARGET as usize + 3].copy_from_slice(&[
        0x41, // inc ecx
        0x46, // inc esi
        0xf4, // hlt
    ]);

    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;
    native_bus.direct_page_clocks = true;

    for cpu in [&mut interp, &mut native] {
        widen_stack_to_32_bit(cpu);
        cpu.registers.set_esp(0x1000);
    }
    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..3 {
        native.halted = false;
        native.registers.eip = 0x100;
        native.registers.set_esp(0x1000);
        drive(&mut native, &mut native_bus);
    }
    assert_eq!(
        native.jit_direct.len(),
        1,
        "only the source block should have compiled: the target is two slots, below the minimum"
    );

    for cpu in [&mut interp, &mut native] {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.registers.gpr.fill(0);
        cpu.registers.set_esp(0x1000);
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.memory[0x0ffc..0x1000].fill(0);
        bus.trace = BusTrace::default();
    }
    let direct_before = native.perf_counters().jit_direct_insns;
    let loads_before = native.perf_counters().jit_native_load_hits;
    let stores_before = native.perf_counters().jit_native_store_hits;

    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(native_outcomes, interp_outcomes);
    assert_eq!(native, interp);
    assert_eq!(native_bus.memory, interp_bus.memory);
    // The AGGREGATE, not `trace.cycles()`, for the same reason as the CallReg, PushMem and JmpMem
    // mid-block fixtures: native execution batches the whole compiled window and emits no
    // per-access log.
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert_eq!(native.registers.eip, interp.registers.eip);
    assert_eq!(native.registers.esp(), 0x0ffc, "ESP must fall by exactly 4");
    assert_eq!(
        u32::from_le_bytes(native_bus.memory[0x0ffc..0x1000].try_into().unwrap()),
        RETURN,
        "the pushed return address must be the EIP right after the SIX-byte call"
    );
    // The DYNAMIC counter lanes, and they are here rather than in the compile-outcome fixture
    // because that one can only see the STATIC registration. `run.rs` folds `NativeExit`'s
    // accumulated read and write counts into these two counters and into the bus charge, so this
    // says the emitted code actually performed one of each at runtime rather than merely
    // declaring it at compile time.
    assert_eq!(
        native.perf_counters().jit_native_load_hits - loads_before,
        1,
        "one native dword read: the branch target"
    );
    assert_eq!(
        native.perf_counters().jit_native_store_hits - stores_before,
        1,
        "one native dword write: the return-address push, and it must reach NativeExit through \
         the RAM dword-write lane"
    );
    // Anti-vacuity, and the load-bearing assertion. Two native instructions means the call itself
    // retired natively rather than the block silently stopping before it.
    assert_eq!(native.perf_counters().jit_direct_insns - direct_before, 2);
}

/// `call dword [esp-4]`: the target is read from the very dword the return address is about to be
/// pushed onto. This is the ORDERING fixture, and it is the CallMem analogue of
/// `a_push_through_memory_of_the_identical_address_matches_the_interpreter`.
///
/// The interpreter reads the operand and only then pushes (`execute_extended.rs`, group 5 arm 2),
/// so the target is the PRE-push contents. `emit_call_mem` emits the source read first for exactly
/// that reason. Swap the two halves and the "target" becomes the return address the store just
/// wrote, and the guest jumps to 0x106 instead of 0x300 -- a divergence no register-sourced or
/// disjoint-address fixture can see, because in those the read is correct whichever order it runs
/// in.
///
/// The page guard forces both addresses to 4-byte alignment, so source and destination are either
/// identical or fully disjoint, never partially overlapping. Identical is the shape that makes the
/// ordering observable at all.
#[test]
fn a_mid_block_call_through_its_own_stack_slot_matches_the_interpreter() {
    const TARGET: u32 = 0x300;
    const RETURN: u32 = 0x106;
    const SLOT: usize = 0x0ffc;
    let mut memory = vec![0; 0x2000];
    memory[0x100..0x106].copy_from_slice(&[
        0x90, // starter
        0x40, // inc eax
        0xff, 0x54, 0x24, 0xfc, // call dword [esp-4], MID-BLOCK, TERMINAL
    ]);
    memory[TARGET as usize..TARGET as usize + 3].copy_from_slice(&[
        0x41, // inc ecx
        0x46, // inc esi
        0xf4, // hlt
    ]);

    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;
    native_bus.direct_page_clocks = true;

    for cpu in [&mut interp, &mut native] {
        widen_stack_to_32_bit(cpu);
        cpu.registers.set_esp(0x1000);
    }
    // The call OVERWRITES its own source, so the seed has to be re-laid before every run rather
    // than once at the top the way the PushMem sibling can: there the value is written straight
    // back unchanged, here it is replaced by the return address.
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.memory[SLOT..SLOT + 4].copy_from_slice(&TARGET.to_le_bytes());
    }
    drive(&mut interp, &mut interp_bus);
    interp_bus.memory[SLOT..SLOT + 4].copy_from_slice(&TARGET.to_le_bytes());
    drive(&mut native, &mut native_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..3 {
        native.halted = false;
        native.registers.eip = 0x100;
        native.registers.set_esp(0x1000);
        native_bus.memory[SLOT..SLOT + 4].copy_from_slice(&TARGET.to_le_bytes());
        drive(&mut native, &mut native_bus);
    }
    assert_eq!(native.jit_direct.len(), 1);

    for cpu in [&mut interp, &mut native] {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.registers.gpr.fill(0);
        cpu.registers.set_esp(0x1000);
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.memory[SLOT..SLOT + 4].copy_from_slice(&TARGET.to_le_bytes());
        bus.trace = BusTrace::default();
    }
    let direct_before = native.perf_counters().jit_direct_insns;

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
        native.registers.eip, interp.registers.eip,
        "the call must have gone to the PRE-push contents of the slot"
    );
    assert_eq!(native.registers.esp(), SLOT as u32);
    assert_eq!(
        u32::from_le_bytes(native_bus.memory[SLOT..SLOT + 4].try_into().unwrap()),
        RETURN,
        "the return address replaced the target in the slot they share"
    );
    assert_eq!(native.perf_counters().jit_direct_insns - direct_before, 2);
}

/// A CallMem whose SOURCE address falls in the mode-13 aperture must side exit rather than lower.
///
/// The sibling of `a_push_through_memory_whose_source_is_the_mode13_aperture_side_exits`, and it
/// pins the same counter-ordering argument: a mode-13 read completion increments the dynamic
/// mode-13 read count as soon as the read resolves, while the stack store's guards can still side
/// exit afterwards, and a side exit reports dynamic counters against a static snapshot taken
/// before the slot. `run.rs`'s `dword_reads - exit.mode13_dword_reads` would go negative there.
/// Refusing the source kind outright makes that state unreachable.
#[test]
fn a_call_through_memory_whose_source_is_the_mode13_aperture_side_exits() {
    const TARGET: u32 = 0x200;
    let mut memory = vec![0; 0x000b_1000];
    memory[0x100..0x108].copy_from_slice(&[
        0x90, // starter
        0x40, // inc eax
        0xff, 0x15, 0x00, 0x00, 0x0a, 0x00, // call dword [0xa0000], MID-BLOCK
    ]);
    memory[0x000a_0000..0x000a_0004].copy_from_slice(&TARGET.to_le_bytes());
    memory[TARGET as usize..TARGET as usize + 3].copy_from_slice(&[
        0x41, // inc ecx
        0x46, // inc esi
        0xf4, // hlt
    ]);

    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;
    native_bus.direct_page_clocks = true;

    // The aperture sits past the 0xFFFF real-mode limit, so DS must be widened or the access
    // faults instead of exercising the refusal.
    for cpu in [&mut interp, &mut native] {
        make_data_segments_flat(cpu);
        widen_stack_to_32_bit(cpu);
        cpu.registers.set_esp(0x1000);
    }
    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..3 {
        native.halted = false;
        native.registers.eip = 0x100;
        native.registers.set_esp(0x1000);
        drive(&mut native, &mut native_bus);
    }
    assert_eq!(native.jit_direct.len(), 1);

    for cpu in [&mut interp, &mut native] {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.registers.gpr.fill(0);
        cpu.registers.set_esp(0x1000);
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.memory[0x0ffc..0x1000].fill(0);
        bus.trace = BusTrace::default();
    }
    let direct_before = native.perf_counters().jit_direct_insns;
    let unavailable_before = native.perf_counters().jit_direct_exit_unavailable_or_kind;

    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(native_outcomes, interp_outcomes);
    assert_eq!(native, interp);
    assert_eq!(native_bus.memory, interp_bus.memory);
    assert_eq!(native_bus.trace.cycles(), interp_bus.trace.cycles());
    assert_eq!(native.registers.esp(), 0x0ffc);
    assert_eq!(
        u32::from_le_bytes(native_bus.memory[0x0ffc..0x1000].try_into().unwrap()),
        0x108,
        "the instruction ran somewhere: the return address reached the stack even though the \
         native slot side exited"
    );
    assert_eq!(
        native.perf_counters().jit_direct_exit_unavailable_or_kind - unavailable_before,
        1,
        "the source's non-RAM page kind must be what triggers the side exit"
    );
    // Anti-vacuity: one native instruction is the `inc eax` before the call. The call itself never
    // completes natively, so it is never counted.
    assert_eq!(native.perf_counters().jit_direct_insns - direct_before, 1);
}

/// The CS-limit guard on a MEMORY-sourced dynamic target: a dword above the code segment limit
/// must side exit before the return address is ever pushed, and the interpreter's own re-run must
/// agree exactly.
///
/// The sibling of the CallReg limit fixture above, and it pins the one thing that fixture cannot:
/// the limit check's POSITION. `CallReg` can check first because its target needs no load.
/// `CallMem` has to load first, so the check sits between the load and the store -- the only
/// position that is both after the value exists and before any guest byte moves. Emitted after the
/// store instead, ESP and the pushed dword would both be live at the exit and the interpreter's
/// re-run would push a second time.
#[test]
fn finite_cs_call_through_memory_limit_exit_preserves_restart_state_and_faults_precisely() {
    const ENTRY: u32 = 0x100;
    const CALL: u32 = ENTRY + 2;
    const INITIAL_ESP: u32 = 0x1000;
    // Above the real-mode 0xFFFF CS limit.
    const TARGET: u32 = 0x1_0000;
    let mut memory = vec![0; 0x2000];
    memory[ENTRY as usize..ENTRY as usize + 8].copy_from_slice(&[
        0x40, // inc eax
        0x41, // inc ecx
        0xff, 0x15, 0x00, 0x08, 0x00, 0x00, // call dword [0x800]
    ]);
    memory[0x800..0x804].copy_from_slice(&TARGET.to_le_bytes());

    let mut native = fresh();
    let mut interp = fresh();
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    for cpu in [&mut native, &mut interp] {
        widen_stack_to_32_bit(cpu);
        cpu.registers.set_esp(INITIAL_ESP);
        cpu.registers.eip = ENTRY;
    }

    for lin in [ENTRY, ENTRY + 1, CALL] {
        native.registers.eip = lin;
        native
            .fetch_decoded(&mut native_bus, lin)
            .expect("fixture decode");
        interp.registers.eip = lin;
        interp
            .fetch_decoded(&mut interp_bus, lin)
            .expect("fixture decode");
    }
    native.registers.eip = ENTRY;
    interp.registers.eip = ENTRY;
    native.jit_direct.set_fast_map_enabled_for_test(true);
    // Page 0 covers the source dword at 0x800 AND the stack cell at 0x0ffc, so both lanes are
    // available and the CS limit is the only thing that can refuse.
    map_direct_page(
        &mut native,
        &mut native_bus,
        0,
        0,
        jit::fast_map::PagePermissions::UNPAGED,
        true,
        true,
    );

    let key = jit::direct::key_for(&native, ENTRY, true).unwrap();
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = jit::direct::compile(&mut native, ENTRY, true).unwrap();
    assert_eq!(compilation.span.instructions, 3);
    let id = native.jit_direct.install(&compilation).unwrap();
    let block = native
        .jit_direct
        .block(id)
        .expect("installed block must be live");

    let side_exits = native.perf_counters().jit_direct_side_exits;
    let limit_exits = native.direct_stall_snapshot().side_exit_segment_limit;
    let insns_before = native.perf_counters().jit_direct_insns;

    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    interp.cycle(&mut interp_bus).unwrap();
    interp.cycle(&mut interp_bus).unwrap();
    assert_eq!(native.registers, interp.registers);
    assert_eq!(
        native.registers.eip, CALL,
        "the side exit must leave EIP at the call itself"
    );
    assert_eq!(
        native.registers.esp(),
        INITIAL_ESP,
        "the native side exit must leave ESP untouched: the push never happened"
    );
    assert_eq!(
        native_bus.memory, interp_bus.memory,
        "and no return address reached the stack"
    );
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert_eq!(native.perf_counters().jit_direct_side_exits - side_exits, 1);
    assert_eq!(
        native.direct_stall_snapshot().side_exit_segment_limit - limit_exits,
        1
    );
    // Anti-vacuity: only the two fillers retired natively.
    assert_eq!(native.perf_counters().jit_direct_insns - insns_before, 2);

    // The interpreter's own arm has no limit check: both sides must execute the call itself
    // successfully, pushing the return address and landing EIP on the too-large target.
    let native_call = native.decode_cache.get(CALL, true).unwrap();
    let interp_call = interp.decode_cache.get(CALL, true).unwrap();
    native
        .execute_decoded(&native_call, &mut native_bus)
        .unwrap();
    interp
        .execute_decoded(&interp_call, &mut interp_bus)
        .unwrap();
    assert_eq!(native.registers.eip, TARGET);
    assert_eq!(interp.registers.eip, TARGET);
    assert_eq!(native.registers.esp(), INITIAL_ESP - 4);
    assert_eq!(interp.registers.esp(), INITIAL_ESP - 4);
    assert_eq!(native_bus.memory, interp_bus.memory);

    // The fault surfaces on the FOLLOWING fetch.
    let native_fault = native.fetch_decoded(&mut native_bus, native.registers.eip);
    let interp_fault = interp.fetch_decoded(&mut interp_bus, interp.registers.eip);
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
    assert_eq!(native_bus.memory, interp_bus.memory);
}

/// The return-address push landing on WATCHED CODE must side exit rather than write.
///
/// `emit_call_mem` inherits `emit_watched_store_guard` from `emit_push_mem`, and nothing else in
/// the suite compiles a CallMem block at all, so without this fixture deleting that guard from the
/// new emitter is invisible: every other CallMem fixture pushes onto a stack cell no code was ever
/// fetched from, where the guard is emitted but never taken.
///
/// Run through `try_run_direct_block_for_test` rather than `drive`, for the CS-limit fixture's
/// reason: the write is refused, so the block's state before and after is the assertion, and a
/// driven run would immediately re-enter and re-decide.
#[test]
fn a_call_through_memory_whose_push_lands_on_watched_code_side_exits() {
    const ENTRY: u32 = 0x100;
    const CALL: u32 = ENTRY + 2;
    const INITIAL_ESP: u32 = 0x1000;
    const TARGET: u32 = 0x200;
    const STACK_CELL: u32 = INITIAL_ESP - 4;
    let mut memory = vec![0; 0x2000];
    memory[ENTRY as usize..ENTRY as usize + 8].copy_from_slice(&[
        0x40, // inc eax
        0x41, // inc ecx
        0xff, 0x15, 0x00, 0x08, 0x00, 0x00, // call dword [0x800]
    ]);
    memory[0x800..0x804].copy_from_slice(&TARGET.to_le_bytes());

    let mut native = fresh();
    let mut interp = fresh();
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    for cpu in [&mut native, &mut interp] {
        widen_stack_to_32_bit(cpu);
        cpu.registers.set_esp(INITIAL_ESP);
        cpu.registers.eip = ENTRY;
    }

    for lin in [ENTRY, ENTRY + 1, CALL] {
        native.registers.eip = lin;
        native
            .fetch_decoded(&mut native_bus, lin)
            .expect("fixture decode");
        interp.registers.eip = lin;
        interp
            .fetch_decoded(&mut interp_bus, lin)
            .expect("fixture decode");
    }
    native.registers.eip = ENTRY;
    interp.registers.eip = ENTRY;
    native.jit_direct.set_fast_map_enabled_for_test(true);
    map_direct_page(
        &mut native,
        &mut native_bus,
        0,
        0,
        jit::fast_map::PagePermissions::UNPAGED,
        true,
        true,
    );

    let key = jit::direct::key_for(&native, ENTRY, true).unwrap();
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = jit::direct::compile(&mut native, ENTRY, true).unwrap();
    assert_eq!(compilation.span.instructions, 3);
    let id = native.jit_direct.install(&compilation).unwrap();
    let block = native
        .jit_direct
        .block(id)
        .expect("installed block must be live");

    // AFTER the compile, so the block itself is unaffected: the stack cell now reads as code to
    // the interpreter's own watch table, which is one of the two tables the emitted guard probes.
    // Marked on BOTH cpus so their invalidation work matches instruction for instruction.
    for cpu in [&mut native, &mut interp] {
        cpu.mark_decode_code_for_test(STACK_CELL, 4);
    }

    let side_exits = native.perf_counters().jit_direct_side_exits;
    let watch_exits = native.perf_counters().jit_direct_exit_code_watch;
    let insns_before = native.perf_counters().jit_direct_insns;

    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    interp.cycle(&mut interp_bus).unwrap();
    interp.cycle(&mut interp_bus).unwrap();

    assert_eq!(native.registers, interp.registers);
    assert_eq!(
        native.registers.eip, CALL,
        "the side exit must leave EIP at the call itself"
    );
    assert_eq!(
        native.registers.esp(),
        INITIAL_ESP,
        "the refused push must leave ESP untouched"
    );
    assert_eq!(
        native_bus.memory, interp_bus.memory,
        "and no return address reached the watched cell"
    );
    assert_eq!(native.perf_counters().jit_direct_side_exits - side_exits, 1);
    assert_eq!(
        native.perf_counters().jit_direct_exit_code_watch - watch_exits,
        1,
        "the code-watch guard must be what refuses, not the kind or permission lane"
    );
    // Anti-vacuity: only the two fillers retired natively.
    assert_eq!(native.perf_counters().jit_direct_insns - insns_before, 2);
}

/// A ring-3 `CallMem` block must not panic at emit time, and once compiled it must produce the
/// same guest state as the interpreter.
///
/// The sibling of `cpl3_push_through_memory_does_not_panic_and_matches_the_interpreter`, and it
/// exists for the same recorded hazard: `emit_call_mem` builds TWO `MemorySideExits` and passes
/// `memory.cpl3` to both `append_stubs` calls. Hardcoding either to `false` leaves a
/// referenced-but-unplaced label and panics the encoder on any CPL3 block. Every other CallMem
/// fixture runs at CPL0, where neither permission branch is emitted at all.
///
/// Two phases. Phase 1 is an ordinary permitted CPL3 access, proving the encoder path works end to
/// end. Phase 2 forces the SOURCE page supervisor-only so the runtime read permission check trips
/// before the stack write is reached, proving `emit_read_permission_check`'s label is placed and
/// not merely unexercised.
#[test]
fn cpl3_call_through_memory_does_not_panic_and_matches_the_interpreter() {
    // Phase 1. `CallMem` is a terminal, so the block is just the filler plus the call -- no
    // `DIRECT_BARRIER` tail is needed the way the PushMem sibling needs one, and HLT (CPL0-only,
    // with no IDT in this harness) never enters the picture.
    const PHASE1_ENTRY: u32 = 0x100;
    const PHASE1_TARGET: u32 = 0x200;
    let mut memory = vec![0; 0x2000];
    memory[PHASE1_ENTRY as usize..PHASE1_ENTRY as usize + 7].copy_from_slice(&[
        0x40, // inc eax
        0xff, 0x15, 0x00, 0x08, 0x00, 0x00, // call dword [0x800]
    ]);
    memory[0x800..0x804].copy_from_slice(&PHASE1_TARGET.to_le_bytes());

    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;

    for cpu in [&mut interp, &mut native] {
        promote_to_cpl3(cpu);
        cpu.registers.eip = PHASE1_ENTRY;
        cpu.registers.set_esp(0x1000);
    }

    // Warm the decode cache: `key_for` reads the physical start straight out of it.
    for lin in [PHASE1_ENTRY, PHASE1_ENTRY + 1] {
        native.registers.eip = lin;
        native
            .fetch_decoded(&mut native_bus, lin)
            .expect("fixture decode");
    }
    native.registers.eip = PHASE1_ENTRY;

    let source_page = native_bus
        .direct_page(0x800, BusAccessKind::DataRead)
        .unwrap()
        .unwrap();
    assert!(native.jit_fast_map.populate_read(
        0x800,
        0x800,
        source_page,
        jit::fast_map::PagePermissions {
            writable: true,
            user: true,
        },
        native.physical_page_watched(0x800),
    ));
    let stack_cell = 0x1000u32.wrapping_sub(4);
    let stack_page = native_bus
        .direct_page(stack_cell, BusAccessKind::DataWrite)
        .unwrap()
        .unwrap();
    assert!(native.jit_fast_map.populate_write(
        stack_cell,
        stack_cell,
        stack_page,
        jit::fast_map::PagePermissions {
            writable: true,
            user: true,
        },
        native.physical_page_watched(stack_cell),
    ));

    let key = jit::direct::key_for(&native, PHASE1_ENTRY, true).unwrap();
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = jit::direct::compile(&mut native, PHASE1_ENTRY, true).unwrap();
    assert_eq!(
        compilation.span.instructions, 2,
        "the block must span inc/call, or this proves nothing about CallMem at CPL3"
    );
    let id = native.jit_direct.install(&compilation).unwrap();
    let block = native
        .jit_direct
        .block(id)
        .expect("installed block must be live");

    // Not a panic: a compiled CPL3 CallMem block runs and does not crash the encoder.
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    for _ in 0..2 {
        interp.cycle_no_interrupt_check(&mut interp_bus).unwrap();
    }

    assert_eq!(native.registers, interp.registers, "registers differ");
    assert_eq!(
        native.pending_flags, interp.pending_flags,
        "pending flags differ"
    );
    assert_eq!(native_bus.memory, interp_bus.memory, "memory differs");
    assert_eq!(native.registers.eip, PHASE1_TARGET);
    assert_eq!(native.registers.esp(), 0x0ffc);
    assert_eq!(
        u32::from_le_bytes(native_bus.memory[0x0ffc..0x1000].try_into().unwrap()),
        PHASE1_ENTRY + 7,
        "the pushed return address is the EIP after the six-byte call"
    );

    // Phase 2: a supervisor-only SOURCE page must trip the read permission check and side exit
    // before the write side is reached. The stack lives in a DIFFERENT page from the source for
    // the PushMem sibling's recorded reason: `flags()` is one table per page shared by the read
    // and write maps, so a permissive write mapping on the source's own page would clear the
    // supervisor bit the read mapping set.
    const CALL_ENTRY: u32 = 0x100;
    let mut memory = vec![0; 0x3000];
    memory[CALL_ENTRY as usize..CALL_ENTRY as usize + 8].copy_from_slice(&[
        0x40, // inc eax
        0x41, // inc ecx
        0xff, 0x15, 0x00, 0x03, 0x00, 0x00, // call dword [0x300]
    ]);
    memory[0x300..0x304].copy_from_slice(&0x0000_0400u32.to_le_bytes());

    let mut cpu = fresh();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    promote_to_cpl3(&mut cpu);
    cpu.registers.eip = CALL_ENTRY;
    cpu.registers.set_esp(0x2000);

    for lin in [CALL_ENTRY, CALL_ENTRY + 1, CALL_ENTRY + 2] {
        cpu.registers.eip = lin;
        cpu.fetch_decoded(&mut bus, lin).expect("fixture decode");
    }
    cpu.registers.eip = CALL_ENTRY;

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
        cpu.physical_page_watched(0x300),
    ));
    // The STACK side must be explicitly PERMISSIVE, and that is load-bearing rather than setup
    // noise: without it the stack lane refuses too and the fixture cannot say which lane produced
    // the exit. 0x1ffc is page 1; the source at 0x300 is page 0.
    let stack_cell = 0x2000u32.wrapping_sub(4);
    let stack_page = bus
        .direct_page(stack_cell, BusAccessKind::DataWrite)
        .unwrap()
        .unwrap();
    assert!(cpu.jit_fast_map.populate_write(
        stack_cell,
        stack_cell,
        stack_page,
        jit::fast_map::PagePermissions {
            writable: true,
            user: true,
        },
        cpu.physical_page_watched(stack_cell),
    ));

    let key = jit::direct::key_for(&cpu, CALL_ENTRY, true).unwrap();
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = jit::direct::compile(&mut cpu, CALL_ENTRY, true).unwrap();
    assert_eq!(compilation.span.instructions, 3);
    let id = cpu.jit_direct.install(&compilation).unwrap();
    let block = cpu
        .jit_direct
        .block(id)
        .expect("installed block must be live");

    let exits_before = cpu.perf_counters().jit_direct_side_exits;
    let permissions_before = cpu.perf_counters().jit_direct_exit_permission;
    assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    assert_eq!(
        cpu.registers.eip,
        CALL_ENTRY + 2,
        "must exit exactly at the call"
    );
    assert_eq!(
        cpu.registers.esp(),
        0x2000,
        "the refused read must leave ESP untouched: the push never ran"
    );
    assert_eq!(cpu.perf_counters().jit_direct_side_exits - exits_before, 1);
    assert_eq!(
        cpu.perf_counters().jit_direct_exit_permission - permissions_before,
        1,
        "the SOURCE lane's read permission check must be what refuses"
    );
}

/// PUSH through memory where source and destination are the SAME dword, `push dword [esp-4]`.
///
/// The page guard forces both addresses to 4-byte alignment, so the source and destination are
/// either identical or fully disjoint, never partially overlapping. That makes this the one
/// shape where emitting the stack store BEFORE the source read fails on semantics rather than on
/// a stale stash alone.
///
/// Why the store-before-read mutation fails HERE specifically. Source and destination are the
/// same dword, so an ordering bug cannot be caught by "the wrong address was read": both
/// addresses are correct. What catches it is the STASH. `emit_push_mem` parks the loaded value
/// in the native frame at `STACK_PUSH_MEM_VALUE`, and a store emitted before the read writes
/// whatever that slot happened to hold, which is host garbage on the first execution and the
/// garbage it wrote back on every one after. The seeded value is what makes that visible.
#[test]
fn a_push_through_memory_of_the_identical_address_matches_the_interpreter() {
    let mut memory = vec![0; 0x2000];
    memory[0x100..0x108].copy_from_slice(&[
        0x90, // starter
        0x40, // inc eax
        0xff, 0x74, 0x24, 0xfc, // push dword [esp-4], MID-BLOCK
        0x40, // inc eax
        0xf4, // hlt
    ]);
    // Seeded once, before any run. The push reads this address and writes it straight back, so
    // the value never changes across warm-up or measured runs, and the pre-run reset below must
    // not clear it: clearing it would be clearing the only source the instruction has.
    memory[0x0ffc..0x1000].copy_from_slice(&0xcafe_f00du32.to_le_bytes());

    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;
    native_bus.direct_page_clocks = true;

    for cpu in [&mut interp, &mut native] {
        widen_stack_to_32_bit(cpu);
        cpu.registers.set_esp(0x1000);
    }
    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..3 {
        native.halted = false;
        native.registers.eip = 0x100;
        native.registers.set_esp(0x1000);
        drive(&mut native, &mut native_bus);
    }
    assert_eq!(native.jit_direct.len(), 1);

    for cpu in [&mut interp, &mut native] {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.registers.gpr.fill(0);
        cpu.registers.set_esp(0x1000);
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    for bus in [&mut interp_bus, &mut native_bus] {
        // Deliberately not clearing 0x0ffc..0x1000. It is the source for this fixture.
        bus.trace = BusTrace::default();
    }
    let direct_before = native.perf_counters().jit_direct_insns;

    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(native_outcomes, interp_outcomes);
    assert_eq!(native, interp);
    assert_eq!(native_bus.memory, interp_bus.memory);
    // The AGGREGATE, not `trace.cycles()`. Native execution batches the whole compiled window and
    // emits no per-access DataRead or DataWrite records, so the two per-cycle LOGS differ by
    // construction and comparing them asserts something false about the design. The aggregate is
    // what feeds `raw_bus_clocks`, which is a pinned corpus anchor, so it is both the correct and
    // the load-bearing comparison. `direct_page_clocks` above is what makes it non-trivial:
    // without it the test bus charges zero for every data width and this compares 0 against 0.
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert_eq!(native.registers.esp(), 0x0ffc);
    assert_eq!(
        u32::from_le_bytes(native_bus.memory[0x0ffc..0x1000].try_into().unwrap()),
        0xcafe_f00d,
        "the push read the dword at its own destination and wrote it straight back, unchanged"
    );
    // Anti-vacuity. Three native instructions means the block spanned the push.
    assert_eq!(native.perf_counters().jit_direct_insns - direct_before, 3);
}

/// A PushMem whose SOURCE address falls in the mode-13 aperture must side exit rather than lower.
///
/// This pins the reason `emit_push_mem` refuses every page kind but plain RAM on BOTH accesses.
/// The alternative construction reads the source through the mode-13 completion path, which
/// increments a dynamic read counter as soon as the read resolves, while the stack store's guards
/// can still side exit afterwards. A side exit reports dynamic counters against a static snapshot
/// taken before the slot, so the RAM read count that `run.rs` derives by subtraction would go
/// negative there: a debug panic, and in release a wrap that gets saturating-multiplied into the
/// bus charge. Refusing the source kind outright means that state is never reached.
#[test]
fn a_push_through_memory_whose_source_is_the_mode13_aperture_side_exits() {
    let mut memory = vec![0; 0x000b_1000];
    memory[0x100..0x10a].copy_from_slice(&[
        0x90, // starter
        0x40, // inc eax
        0xff, 0x35, 0x00, 0x00, 0x0a, 0x00, // push dword [0xa0000], MID-BLOCK
        0x40, // inc eax
        0xf4, // hlt
    ]);
    // A known value, so the fixture proves the RIGHT dword still reached the stack.
    memory[0x000a_0000..0x000a_0004].copy_from_slice(&0x1357_9bdfu32.to_le_bytes());

    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;
    native_bus.direct_page_clocks = true;

    // The mode-13 aperture sits at 0xa0000, past the 0xffff real-mode segment limit, so DS (and
    // SS, for the stack side of the push) must be widened or every access there faults instead of
    // exercising the refusal this fixture is about.
    for cpu in [&mut interp, &mut native] {
        make_data_segments_flat(cpu);
        widen_stack_to_32_bit(cpu);
        cpu.registers.set_esp(0x1000);
    }
    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..3 {
        native.halted = false;
        native.registers.eip = 0x100;
        native.registers.set_esp(0x1000);
        drive(&mut native, &mut native_bus);
    }
    assert_eq!(native.jit_direct.len(), 1);

    for cpu in [&mut interp, &mut native] {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.registers.gpr.fill(0);
        cpu.registers.set_esp(0x1000);
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.memory[0x0ffc..0x1000].fill(0);
        bus.trace = BusTrace::default();
    }
    let direct_before = native.perf_counters().jit_direct_insns;
    let unavailable_before = native.perf_counters().jit_direct_exit_unavailable_or_kind;

    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(native_outcomes, interp_outcomes);
    assert_eq!(native, interp);
    assert_eq!(native_bus.memory, interp_bus.memory);
    assert_eq!(native_bus.trace.cycles(), interp_bus.trace.cycles());
    assert_eq!(native.registers.esp(), 0x0ffc);
    assert_eq!(
        u32::from_le_bytes(native_bus.memory[0x0ffc..0x1000].try_into().unwrap()),
        0x1357_9bdf,
        "the instruction ran somewhere: the pushed dword reached the stack even though it side \
         exited"
    );
    assert_eq!(
        native.perf_counters().jit_direct_exit_unavailable_or_kind - unavailable_before,
        1,
        "the source's non-RAM page kind must be what triggers the side exit"
    );
    // Anti-vacuity, and it is the point of the fixture. One native instruction is the inc eax
    // before the push; the push itself never completes natively, so it is never counted.
    assert_eq!(native.perf_counters().jit_direct_insns - direct_before, 1);
}

/// Promotes an already-constructed CPU to ring 3 with flat protected-mode segments, mirroring
/// `cpl3_fast_map_permission_side_exit_is_counted`'s setup exactly.
fn promote_to_cpl3(cpu: &mut CpuGsw) {
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
}

/// A ring-3 `PushMem` block must not panic at emit time, and once compiled it must produce the
/// same guest state as the interpreter.
///
/// `emit_push_mem`'s own comment records that hardcoding the read set's permission flag to
/// `false` "leaves a referenced-but-unplaced label and panics the encoder on any CPL3 block".
/// Every OTHER PushMem fixture in this file runs at CPL0, where `memory.cpl3` is false and
/// neither permission branch is ever taken. Nothing else in the suite compiles a CPL3 PushMem
/// block, so a regression there has zero coverage before this fixture.
///
/// Two phases. The first is an ordinary, permitted access at CPL3: the block must compile, run,
/// and match the interpreter exactly, proving the CPL3 encoder path works end to end rather than
/// merely not crashing. The second forces the SOURCE page supervisor-only while the CPU runs at
/// CPL3, so the runtime read permission check must trip a side exit before the stack write is
/// ever reached, proving `emit_read_permission_check`'s label is placed correctly and not just
/// exercised by luck on the happy path above.
#[test]
fn cpl3_push_through_memory_does_not_panic_and_matches_the_interpreter() {
    // Two things this fixture had to work around, neither of them about CPL3.
    //
    // HLT is CPL0-only, and this harness sets up no IDT, so ending a ring-3 program with `hlt`
    // double faults before any assertion runs. The block is stepped directly and ends on
    // DIRECT_BARRIER instead.
    //
    // HLT is also absent from `classify` entirely, so it is an unclassifiable barrier rather than
    // a terminal. A lone PushMem followed by `hlt` is one slot, which is under the compiler's
    // three-slot minimum and is structurally rejected. Two `inc` fillers precede the push.

    // Phase 1: an ordinary CPL3 access, differential against the interpreter. Steps a FIXED
    // instruction count directly through `try_run_direct_block_for_test` and
    // `cycle_no_interrupt_check`, rather than driving to HLT, and ends the block on
    // `DIRECT_BARRIER` at exactly three instructions.
    const PHASE1_ENTRY: u32 = 0x100;
    let mut memory = vec![0; 0x2000];
    memory[PHASE1_ENTRY as usize..PHASE1_ENTRY as usize + 9].copy_from_slice(&[
        0x40, // inc eax
        0xff,
        0x35,
        0x00,
        0x08,
        0x00,
        0x00,           // push dword [0x800]
        0x40,           // inc eax
        DIRECT_BARRIER, // stop the block here
    ]);
    memory[0x800..0x804].copy_from_slice(&0x0bad_c0deu32.to_le_bytes());

    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;

    for cpu in [&mut interp, &mut native] {
        promote_to_cpl3(cpu);
        cpu.registers.eip = PHASE1_ENTRY;
        cpu.registers.set_esp(0x1000);
    }

    // Warm the decode cache. `key_for` reads the physical start straight out of it and returns
    // `None` if nothing has fetched this line yet.
    for lin in [
        PHASE1_ENTRY,
        PHASE1_ENTRY + 1,
        PHASE1_ENTRY + 7,
        PHASE1_ENTRY + 8,
    ] {
        native.registers.eip = lin;
        native
            .fetch_decoded(&mut native_bus, lin)
            .expect("fixture decode");
    }
    native.registers.eip = PHASE1_ENTRY;

    // Populate the fast map for both accesses with PERMISSIVE flags, so this phase proves the
    // permitted path works rather than exercising the refusal Phase 2 covers.
    let source_page = native_bus
        .direct_page(0x800, BusAccessKind::DataRead)
        .unwrap()
        .unwrap();
    assert!(native.jit_fast_map.populate_read(
        0x800,
        0x800,
        source_page,
        jit::fast_map::PagePermissions {
            writable: true,
            user: true,
        },
        native.physical_page_watched(0x800),
    ));
    let stack_addr = 0x1000u32.wrapping_sub(4);
    let stack_page = native_bus
        .direct_page(stack_addr, BusAccessKind::DataWrite)
        .unwrap()
        .unwrap();
    assert!(native.jit_fast_map.populate_write(
        stack_addr,
        stack_addr,
        stack_page,
        jit::fast_map::PagePermissions {
            writable: true,
            user: true,
        },
        native.physical_page_watched(stack_addr),
    ));

    let key = jit::direct::key_for(&native, PHASE1_ENTRY, true).unwrap();
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = jit::direct::compile(&mut native, PHASE1_ENTRY, true).unwrap();
    assert_eq!(
        compilation.span.instructions, 3,
        "the block must span inc/push/inc, or this proves nothing about PushMem at CPL3"
    );
    let id = native.jit_direct.install(&compilation).unwrap();
    let block = native
        .jit_direct
        .block(id)
        .expect("installed block must be live");

    // Not a panic: a compiled CPL3 PushMem block runs and does not crash the encoder.
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    for _ in 0..3 {
        interp.cycle_no_interrupt_check(&mut interp_bus).unwrap();
    }

    assert_eq!(native.registers, interp.registers, "registers differ");
    assert_eq!(
        native.pending_flags, interp.pending_flags,
        "pending flags differ"
    );
    assert_eq!(native_bus.memory, interp_bus.memory, "memory differs");
    assert_eq!(native.registers.esp(), 0x0ffc);
    assert_eq!(
        u32::from_le_bytes(native_bus.memory[0x0ffc..0x1000].try_into().unwrap()),
        0x0bad_c0de,
        "the dword at the source address must be what reached the stack"
    );

    // Phase 2: a supervisor-only SOURCE page must trip the read permission check and side exit
    // before the write side is ever reached. Modelled on
    // `cpl3_fast_map_permission_side_exit_is_counted`: build and install the block directly, force
    // the source page's fast-map permission to `user: false`, and run it through the raw block
    // runner so the permission trap is isolated from the write side and from ordinary admission.
    // The two `inc` fillers put the push at the third slot, per the HLT-is-not-a-terminal note
    // above.
    const PUSH_ENTRY: u32 = 0x100;
    // 0x3000 so the STACK lives in a different PAGE from the source. `flags()` is one table
    // per page shared by the read and write maps, so a permissive write mapping on the
    // source's own page silently clears the supervisor bit the read mapping set and the
    // source stops refusing at all. Source 0x300 is page 0; the stack at 0x1ffc is page 1.
    let mut memory = vec![0; 0x3000];
    memory[PUSH_ENTRY as usize..PUSH_ENTRY as usize + 9].copy_from_slice(&[
        0x40, // inc eax
        0x41, // inc ecx
        0xff, 0x35, 0x00, 0x03, 0x00, 0x00, // push dword [0x300]
        0xf4, // hlt
    ]);
    memory[0x300..0x304].copy_from_slice(&0x2468_ace0u32.to_le_bytes());

    let mut cpu = fresh();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    promote_to_cpl3(&mut cpu);
    cpu.registers.eip = PUSH_ENTRY;
    cpu.registers.set_esp(0x2000);

    // Warm the decode cache, exactly as in Phase 1: `key_for` needs a fetched physical start.
    for lin in [PUSH_ENTRY, PUSH_ENTRY + 1, PUSH_ENTRY + 2, PUSH_ENTRY + 8] {
        cpu.registers.eip = lin;
        cpu.fetch_decoded(&mut bus, lin).expect("fixture decode");
    }
    cpu.registers.eip = PUSH_ENTRY;

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
        cpu.physical_page_watched(0x300),
    ));

    // The STACK side must be explicitly PERMISSIVE, and this is the load-bearing half of the
    // fixture rather than setup noise. Without it the stack lane refuses too, so a permission
    // side exit is counted whichever lane produced it, and the fixture cannot tell them apart.
    // A mutation battery proved that: deleting `emit_read_permission_check` from the source lane
    // SURVIVED this fixture until the stack was made permissive, because the exit was still
    // counted, just from the wrong lane. Now the source is the only thing that can refuse.
    //
    // The address is `ESP - 4`, NOT ESP. The write lands at 0x0ffc, which is in page 0, the same
    // page as the code. Populating page 0x1000 instead leaves page 0 carrying whatever the code
    // fetch gave it, which is supervisor-only, so the stack lane refuses and the mutation stays
    // invisible. That mistake was made once here already.
    let stack_addr = 0x2000u32.wrapping_sub(4);
    let stack_page = bus
        .direct_page(stack_addr, BusAccessKind::DataWrite)
        .unwrap()
        .unwrap();
    assert!(cpu.jit_fast_map.populate_write(
        stack_addr,
        stack_addr,
        stack_page,
        jit::fast_map::PagePermissions {
            writable: true,
            user: true,
        },
        cpu.physical_page_watched(stack_addr),
    ));

    let key = jit::direct::key_for(&cpu, PUSH_ENTRY, true).unwrap();
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = jit::direct::compile(&mut cpu, PUSH_ENTRY, true).unwrap();
    let id = cpu.jit_direct.install(&compilation).unwrap();
    let block = cpu
        .jit_direct
        .block(id)
        .expect("installed block must be live");

    let exits_before = cpu.perf_counters().jit_direct_side_exits;
    let permissions_before = cpu.perf_counters().jit_direct_exit_permission;
    assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    // The two fillers run natively before the side exit; only the push, the third slot, refuses.
    assert_eq!(
        cpu.registers.eip,
        PUSH_ENTRY + 2,
        "must exit exactly at the push"
    );
    assert_eq!(
        cpu.registers.eax(),
        1,
        "the filler before the push ran natively"
    );
    assert_eq!(
        cpu.registers.ecx(),
        1,
        "the second filler before the push ran natively"
    );
    assert_eq!(cpu.perf_counters().jit_direct_side_exits - exits_before, 1);
    assert_eq!(
        cpu.perf_counters().jit_direct_exit_permission - permissions_before,
        1
    );
}

/// JMP through memory, executed NATIVELY in the middle of a block, and compared against the
/// interpreter across the whole run, including the code the jump lands on.
///
/// `JmpMem` is a terminal, so unlike the `PushMem` mid-block fixture the jump does not sit
/// between two more slots of the SAME block: it ends this one, and the target is reached through
/// the normal continuation machinery (interpreted here, since the target is only two instructions
/// long and structurally too short to compile on its own).
///
/// The starter is deliberately sacrificial rather than counted: the FIRST cold visit to an
/// address is always interpreted once to mark it `Seen`, so the compiled block actually begins
/// one instruction AFTER the starter, at the filler. That is the entry-position trap from the
/// other side: the starter exists so the jump lands on the SECOND compiled slot, never the
/// block's own entry, without needing to assert anything about the starter itself.
///
/// The source is a DS-based absolute operand on purpose, exactly as the PushMem fixture: a
/// `read_segment` that wrongly returned SS is invisible on an ESP or EBP based source.
#[test]
fn a_mid_block_jmp_through_memory_matches_the_interpreter() {
    let mut memory = vec![0; 0x2000];
    memory[0x100..0x108].copy_from_slice(&[
        0x90, // starter
        0x40, // inc eax
        0xff, 0x25, 0x00, 0x08, 0x00, 0x00, // jmp dword [0x800], MID-BLOCK, TERMINAL
    ]);
    // The target: two more instructions, too short to compile on its own, then HLT.
    memory[0x200..0x203].copy_from_slice(&[
        0x43, // inc ebx
        0x46, // inc esi
        0xf4, // hlt
    ]);
    memory[0x800..0x804].copy_from_slice(&0x0000_0200u32.to_le_bytes());

    let mut interp = fresh();
    let mut native = fresh();
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
    assert_eq!(
        native.jit_direct.len(),
        1,
        "only the source block should have compiled: the target is two slots, below the minimum"
    );

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
    assert_eq!(native_bus.memory, interp_bus.memory);
    // The AGGREGATE, not `trace.cycles()`, for the same reason as the PushMem fixtures: native
    // execution batches the whole compiled window and emits no per-access log.
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert_eq!(native.registers.eip, interp.registers.eip);
    // Anti-vacuity, and the load-bearing assertion. The compiled block itself is the filler plus
    // the jump: the starter is a single cold visit, interpreted once to mark the address `Seen`,
    // and never part of the compiled span (the entry-position trap, from the other side). Two
    // native instructions means the jump itself retired natively rather than the block silently
    // stopping before it.
    assert_eq!(native.perf_counters().jit_direct_insns - direct_before, 2);
}

/// The dynamic link cell: run the jump once so the exit reports the MISS (the cell is still
/// unbound), then again so the transfer goes NATIVE. Only this fixture can see the
/// `dynamic_successor` registration: nothing else in the suite exercises `bind_dynamic_successor`
/// at all.
///
/// The target block is installed BEFORE the source ever runs, so the very first native call CAN
/// bind the cell, but the native code itself still takes the miss path on that call:
/// `run_direct_block` calls `bind_dynamic_successor` only after the entry function returns and
/// reports a nonzero `dynamic_link_cell`, so the bind happens in Rust, one call late. The SECOND
/// call finds the cell bound and a live portal, and jumps `jmp_r64` straight into the target's
/// body without a dispatcher round-trip.
#[test]
fn a_jmp_through_memory_links_and_transfers_natively_on_the_second_entry() {
    const SOURCE: u32 = 0x100;
    const TARGET: u32 = 0x300;
    const MEM: u32 = 0x800;
    let mut memory = vec![0; 0x2000];
    memory[SOURCE as usize..SOURCE as usize + 8].copy_from_slice(&[
        0x40, // inc eax
        0x43, // inc ebx
        0xff, 0x25, 0x00, 0x08, 0x00, 0x00, // jmp dword [0x800]
    ]);
    memory[TARGET as usize..TARGET as usize + 4].copy_from_slice(&[
        0x42, // inc edx
        0x46, // inc esi
        0x47, // inc edi
        0xf4, // hlt
    ]);
    memory[MEM as usize..MEM as usize + 4].copy_from_slice(&TARGET.to_le_bytes());

    let mut cpu = fresh();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    // A full interpreted run populates the decode cache for both blocks, exactly as the CPL3 and
    // x87 differential fixtures do before taking manual control. It does NOT populate the native
    // fast map: plain interpreted reads never touch it, only the JIT's own runtime population
    // calls do (the CPL3 fixture's `populate_read`, for instance). `JmpMem`'s dword read needs
    // `NativeMapBases` to exist before `compile` will emit it, so force the map on the way the
    // compile-outcome fixtures do.
    drive(&mut cpu, &mut bus);
    cpu.jit_direct.set_fast_map_enabled_for_test(true);
    cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0, BusAccessKind::DataRead)
        .expect("initialize direct map");

    cpu.halted = false;
    cpu.registers.gpr.fill(0);
    cpu.registers.eip = TARGET;
    let target_key = jit::direct::key_for(&cpu, TARGET, true).expect("target decode");
    assert!(matches!(
        cpu.jit_direct.probe(target_key),
        jit::direct::BlockProbe::Interpret
    ));
    let target_compilation = jit::direct::compile(&mut cpu, TARGET, true).expect("target block");
    assert_eq!(target_compilation.span.instructions, 3);
    cpu.jit_direct
        .install(&target_compilation)
        .expect("target install");

    cpu.registers.eip = SOURCE;
    let source_key = jit::direct::key_for(&cpu, SOURCE, true).expect("source decode");
    assert!(matches!(
        cpu.jit_direct.probe(source_key),
        jit::direct::BlockProbe::Interpret
    ));
    let source_compilation = jit::direct::compile(&mut cpu, SOURCE, true).expect("source block");
    assert_eq!(source_compilation.span.instructions, 3);
    assert!(source_compilation.dynamic_successor);
    let source_id = cpu
        .jit_direct
        .install(&source_compilation)
        .expect("source install");
    let source_block = cpu
        .jit_direct
        .block(source_id)
        .expect("source block remains live");

    cpu.registers.eip = SOURCE;
    let transfers_before = cpu.perf_counters().jit_direct_linked_transfers;
    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, source_block)
            .unwrap()
    );
    assert_eq!(
        cpu.registers.eip, TARGET,
        "the target must be set even on the first, unresolved pass: JmpMem writes EIP before the \
         link-cell check ever runs"
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_linked_transfers,
        transfers_before,
        "the first pass cannot be a linked transfer: the cell is unbound when the native call is \
         made, and it is only bound afterward, in Rust, once the miss is reported"
    );

    cpu.registers.eip = SOURCE;
    let insns_before = cpu.perf_counters().jit_direct_insns;
    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, source_block)
            .unwrap()
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_linked_transfers - transfers_before,
        1,
        "the second pass must find the now-bound cell and its live portal, and jump straight into \
         the target without a dispatcher round-trip"
    );
    // Anti-vacuity. Three from source (two fillers plus the jump) and three from target (three
    // fillers up to its own hlt boundary): six means the chain really crossed into the target
    // natively, rather than the source jump landing back in the dispatcher.
    assert_eq!(cpu.perf_counters().jit_direct_insns - insns_before, 6);
    assert_eq!(cpu.registers.eip, TARGET + 3);
}

/// The dynamic link cell for `CallReg`, modelled on
/// `a_jmp_through_memory_links_and_transfers_natively_on_the_second_entry`: run the call once so
/// the exit reports the MISS (the cell is still unbound), then again so the transfer goes NATIVE.
///
/// The target block is installed BEFORE the source ever runs, so the very first native call CAN
/// bind the cell, but the native code itself still takes the miss path on that call:
/// `run_direct_block` calls `bind_dynamic_successor` only after the entry function returns and
/// reports a nonzero `dynamic_link_cell`, so the bind happens in Rust, one call late. The SECOND
/// call finds the cell bound and a live portal, and jumps `jmp_r64` straight into the target's
/// body without a dispatcher round-trip.
#[test]
fn a_call_through_a_register_links_and_transfers_natively_on_the_second_entry() {
    const SOURCE: u32 = 0x100;
    const TARGET: u32 = 0x300;
    let mut memory = vec![0; 0x2000];
    memory[SOURCE as usize..SOURCE as usize + 4].copy_from_slice(&[
        0x40, // inc eax
        0x41, // inc ecx
        0xff, 0xd3, // call ebx
    ]);
    memory[TARGET as usize..TARGET as usize + 4].copy_from_slice(&[
        0x42, // inc edx
        0x46, // inc esi
        0x47, // inc edi
        0xf4, // hlt
    ]);

    let mut cpu = fresh();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    widen_stack_to_32_bit(&mut cpu);
    // The register CallReg reads its target from, and the stack it pushes onto, must both be
    // live BEFORE the very first interpreted pass: unlike JmpMem's target dword, which is fixed
    // up front in memory, `ebx` here is guest state that the warm-up run itself executes `call
    // ebx` through.
    cpu.registers.set_ebx(TARGET);
    cpu.registers.set_esp(0x1000);
    // A full interpreted run populates the decode cache for both blocks, exactly as the JmpMem
    // link fixture does before taking manual control. It does NOT populate the native fast map:
    // plain interpreted reads never touch it. `CallReg`'s return push needs `NativeMapBases` to
    // exist, AND the stack's own page populated with a WRITE mapping, before `compile`'s emitted
    // store guard will let the push through rather than side exit at `UnavailableOrKind`. The
    // stack write lands at 0xffc, page 0, the same page the code sits on.
    drive(&mut cpu, &mut bus);
    cpu.jit_direct.set_fast_map_enabled_for_test(true);
    map_direct_page(
        &mut cpu,
        &mut bus,
        0,
        0,
        jit::fast_map::PagePermissions::UNPAGED,
        false,
        true,
    );

    cpu.halted = false;
    cpu.registers.gpr.fill(0);
    cpu.registers.eip = TARGET;
    let target_key = jit::direct::key_for(&cpu, TARGET, true).expect("target decode");
    assert!(matches!(
        cpu.jit_direct.probe(target_key),
        jit::direct::BlockProbe::Interpret
    ));
    let target_compilation = jit::direct::compile(&mut cpu, TARGET, true).expect("target block");
    assert_eq!(target_compilation.span.instructions, 3);
    cpu.jit_direct
        .install(&target_compilation)
        .expect("target install");

    cpu.registers.eip = SOURCE;
    cpu.registers.set_ebx(TARGET);
    cpu.registers.set_esp(0x1000);
    let source_key = jit::direct::key_for(&cpu, SOURCE, true).expect("source decode");
    assert!(matches!(
        cpu.jit_direct.probe(source_key),
        jit::direct::BlockProbe::Interpret
    ));
    let source_compilation = jit::direct::compile(&mut cpu, SOURCE, true).expect("source block");
    assert_eq!(source_compilation.span.instructions, 3);
    assert!(source_compilation.dynamic_successor);
    let source_id = cpu
        .jit_direct
        .install(&source_compilation)
        .expect("source install");
    let source_block = cpu
        .jit_direct
        .block(source_id)
        .expect("source block remains live");

    cpu.registers.eip = SOURCE;
    cpu.registers.set_ebx(TARGET);
    let transfers_before = cpu.perf_counters().jit_direct_linked_transfers;
    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, source_block)
            .unwrap()
    );
    assert_eq!(
        cpu.registers.eip, TARGET,
        "the target must be set even on the first, unresolved pass: CallReg writes EIP before \
         the link-cell check ever runs"
    );
    assert_eq!(
        cpu.registers.esp(),
        0x0ffc,
        "the return push and ESP adjust still ran on the unresolved pass"
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_linked_transfers,
        transfers_before,
        "the first pass cannot be a linked transfer: the cell is unbound when the native call is \
         made, and it is only bound afterward, in Rust, once the miss is reported"
    );

    cpu.registers.eip = SOURCE;
    cpu.registers.set_ebx(TARGET);
    cpu.registers.set_esp(0x1000);
    let insns_before = cpu.perf_counters().jit_direct_insns;
    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, source_block)
            .unwrap()
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_linked_transfers - transfers_before,
        1,
        "the second pass must find the now-bound cell and its live portal, and jump straight \
         into the target without a dispatcher round-trip"
    );
    // Anti-vacuity. Three from source (two fillers plus the call) and three from target (three
    // fillers up to its own hlt boundary): six means the chain really crossed into the target
    // natively, rather than the source call landing back in the dispatcher.
    assert_eq!(cpu.perf_counters().jit_direct_insns - insns_before, 6);
    assert_eq!(cpu.registers.eip, TARGET + 3);
}

/// A ring-3 `JmpMem` whose source page is supervisor-only must side exit at the permission check
/// before the dword is ever read, rather than completing the jump.
///
/// Unlike `PushMem`'s CPL3 fixture, `JmpMem` has no second access that needs to stay permissive:
/// the whole instruction is the one dword read, so the source page needs no page of its own
/// separate from the code, and there is nothing else to keep permissive.
///
/// `JmpMem` is a terminal, so the block ends at the jump. There is no HLT to work around (HLT is
/// CPL0-only and this harness sets up no IDT), because the terminal jump never gets that far.
#[test]
fn cpl3_jmp_through_memory_permission_side_exit_is_counted() {
    const JMP_ENTRY: u32 = 0x1301;
    let mut cpu = fresh();
    let mut memory = vec![0; 0x2000];
    memory[JMP_ENTRY as usize..JMP_ENTRY as usize + 8].copy_from_slice(&[
        0x40, // inc eax
        0x41, // inc ecx
        0xff, 0x25, 0x00, 0x13, 0x00, 0x00, // jmp dword [0x1300]
    ]);
    // JMP_ENTRY (0x1301) and the read target (0x1300) share a page, so the read dword at 0x1300
    // falls partly inside these instruction bytes. Benign: the fixture never inspects the read
    // value, and the user bit does not gate code fetch.

    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    promote_to_cpl3(&mut cpu);
    cpu.registers.eip = JMP_ENTRY;

    // Warm the decode cache: `key_for` reads the physical start straight out of it.
    for lin in [JMP_ENTRY, JMP_ENTRY + 1, JMP_ENTRY + 2] {
        cpu.registers.eip = lin;
        cpu.fetch_decoded(&mut bus, lin).expect("fixture decode");
    }
    cpu.registers.eip = JMP_ENTRY;

    let page = bus
        .direct_page(0x1300, BusAccessKind::DataRead)
        .unwrap()
        .unwrap();
    assert!(cpu.jit_fast_map.populate_read(
        0x1300,
        0x1300,
        page,
        jit::fast_map::PagePermissions {
            writable: true,
            user: false,
        },
        cpu.physical_page_watched(0x1300),
    ));

    let key = jit::direct::key_for(&cpu, JMP_ENTRY, true).unwrap();
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = jit::direct::compile(&mut cpu, JMP_ENTRY, true).unwrap();
    assert_eq!(
        compilation.span.instructions, 3,
        "the block must span inc/inc/jmp, or this proves nothing about JmpMem at CPL3"
    );
    let id = cpu.jit_direct.install(&compilation).unwrap();
    let block = cpu
        .jit_direct
        .block(id)
        .expect("installed block must be live");

    let exits_before = cpu.perf_counters().jit_direct_side_exits;
    let permissions_before = cpu.perf_counters().jit_direct_exit_permission;
    let insns_before = cpu.perf_counters().jit_direct_insns;
    assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    // The two fillers run natively before the side exit; only the jump, the third slot, refuses.
    assert_eq!(
        cpu.registers.eip,
        JMP_ENTRY + 2,
        "must exit exactly at the jump, EIP untouched: JmpMem writes EIP only after every guard \
         passes"
    );
    assert_eq!(
        cpu.registers.eax(),
        1,
        "the filler before the jump ran natively"
    );
    assert_eq!(
        cpu.registers.ecx(),
        1,
        "the second filler before the jump ran natively"
    );
    assert_eq!(cpu.perf_counters().jit_direct_side_exits - exits_before, 1);
    assert_eq!(
        cpu.perf_counters().jit_direct_exit_permission - permissions_before,
        1
    );
    // Anti-vacuity: only the two fillers retired natively; the jump itself never completed.
    assert_eq!(cpu.perf_counters().jit_direct_insns - insns_before, 2);
}

/// A ring-3 `CallReg` whose STACK page is supervisor-only must side exit at the write permission
/// check before the return address is ever pushed, rather than completing the call.
///
/// Modelled on `cpl3_jmp_through_memory_permission_side_exit_is_counted`, with the guard moved
/// from a read (the jump target dword) to a write (the return-address push): `CallReg` has no
/// memory READ at all, so the stack write is the only access there is to test.
///
/// The code page and the stack page are kept SEPARATE, the lesson the PushMem CPL3 fixture's own
/// history records: `flags()` is one table per page shared by the read and write maps, so a stack
/// write landing on the same page as the (permissive) code fetch would silently inherit that
/// page's permissive flags, and the mutation this fixture exists to catch would stay invisible.
///
/// `CallReg` is a terminal, so the block ends at the call. There is no HLT to work around (HLT is
/// CPL0-only and this harness sets up no IDT), because the terminal call never gets that far.
#[test]
fn cpl3_call_through_a_register_permission_side_exit_is_counted() {
    const CALL_ENTRY: u32 = 0x101;
    const INITIAL_ESP: u32 = 0x2000;
    let mut cpu = fresh();
    let mut memory = vec![0; 0x3000];
    memory[CALL_ENTRY as usize..CALL_ENTRY as usize + 4].copy_from_slice(&[
        0x40, // inc eax
        0x41, // inc ecx
        0xff, 0xd3, // call ebx
    ]);

    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    promote_to_cpl3(&mut cpu);
    cpu.registers.eip = CALL_ENTRY;
    cpu.registers.set_ebx(0x200); // The target is irrelevant: the call never completes.
    cpu.registers.set_esp(INITIAL_ESP);

    // Warm the decode cache: `key_for` reads the physical start straight out of it.
    for lin in [CALL_ENTRY, CALL_ENTRY + 1, CALL_ENTRY + 2] {
        cpu.registers.eip = lin;
        cpu.fetch_decoded(&mut bus, lin).expect("fixture decode");
    }
    cpu.registers.eip = CALL_ENTRY;

    // The STACK page, supervisor-only. INITIAL_ESP - 4 = 0x1ffc, page 1, well away from the code
    // at page 0.
    let stack_addr = INITIAL_ESP.wrapping_sub(4);
    let stack_page = bus
        .direct_page(stack_addr, BusAccessKind::DataWrite)
        .unwrap()
        .unwrap();
    assert!(cpu.jit_fast_map.populate_write(
        stack_addr,
        stack_addr,
        stack_page,
        jit::fast_map::PagePermissions {
            writable: true,
            user: false,
        },
        cpu.physical_page_watched(stack_addr),
    ));

    let key = jit::direct::key_for(&cpu, CALL_ENTRY, true).unwrap();
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = jit::direct::compile(&mut cpu, CALL_ENTRY, true).unwrap();
    assert_eq!(
        compilation.span.instructions, 3,
        "the block must span inc/inc/call, or this proves nothing about CallReg at CPL3"
    );
    let id = cpu.jit_direct.install(&compilation).unwrap();
    let block = cpu
        .jit_direct
        .block(id)
        .expect("installed block must be live");

    let exits_before = cpu.perf_counters().jit_direct_side_exits;
    let permissions_before = cpu.perf_counters().jit_direct_exit_permission;
    let insns_before = cpu.perf_counters().jit_direct_insns;
    assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    // The two fillers run natively before the side exit; only the call, the third slot, refuses.
    assert_eq!(
        cpu.registers.eip,
        CALL_ENTRY + 2,
        "must exit exactly at the call, EIP untouched: CallReg writes EIP only after every guard \
         passes"
    );
    assert_eq!(
        cpu.registers.esp(),
        INITIAL_ESP,
        "ESP must be untouched: a refused push must not have adjusted it"
    );
    assert_eq!(
        cpu.registers.eax(),
        1,
        "the filler before the call ran natively"
    );
    assert_eq!(
        cpu.registers.ecx(),
        1,
        "the second filler before the call ran natively"
    );
    assert_eq!(cpu.perf_counters().jit_direct_side_exits - exits_before, 1);
    assert_eq!(
        cpu.perf_counters().jit_direct_exit_permission - permissions_before,
        1
    );
    // Anti-vacuity: only the two fillers retired natively; the call itself never completed.
    assert_eq!(cpu.perf_counters().jit_direct_insns - insns_before, 2);
}

// ---------------------------------------------------------------------------------------------
// Slice 6 -- explicit segment overrides on lowered memory kinds.
//
// The admission is a change to `prefixes_supported_for` alone: `decode::parse_addressing_mode`
// folds `segment_override` into `AddrMode.segment`, `classify::direct_addr` copies it verbatim,
// and `read_segment`/`write_segment` return `addr.segment`. NOTHING in the tree exercised that
// path natively before these fixtures, because the gate refused every override, so "the plumbing
// is complete" was an argument and not a measurement. These make it a measurement.
// ---------------------------------------------------------------------------------------------

/// The five admitted segments with DISTINCT real-mode bases, so that reading or writing through
/// the wrong one lands somewhere else entirely. The data lives clear of page 0, where the code
/// sits: a store into the block's own page would take the code-watch side exit and the fixture
/// would measure that instead of the segment.
const OVERRIDE_CASES: [(SegmentIndex, u16, u8); 5] = [
    (SegmentIndex::Es, 0x0040, 0x26),
    (SegmentIndex::Ss, 0x0080, 0x36),
    (SegmentIndex::Ds, 0x0000, 0x3e),
    (SegmentIndex::Fs, 0x00c0, 0x64),
    (SegmentIndex::Gs, 0x0100, 0x65),
];

fn segment_override_cpu() -> CpuGsw {
    let mut cpu = fresh();
    for (segment, selector, _) in OVERRIDE_CASES {
        cpu.load_segment_real(segment, selector);
    }
    widen_stack_to_32_bit(&mut cpu);
    cpu
}

/// A load and a store through an explicit segment override, executed NATIVELY in the middle of a
/// block, differentially against a block-free interpreter -- once per admitted segment.
///
/// Three separate things make this non-vacuous, and all three are needed:
///
///  * **Mid-block.** The tested pair sits behind a NOP starter and an INC, because an opcode at a
///    block's entry never executes natively and the emitter would go untested.
///  * **The override is load-bearing.** Every segment gets a distinct base, and the payload at the
///    overridden address differs from the payload at the DEFAULT segment's address, so a lowering
///    that silently used DS reads the wrong dword. Native-vs-interpreter equality alone could not
///    catch that -- both could be wrong together -- so the loaded value is asserted absolutely and
///    the stored word is asserted to have landed at the overridden address.
///  * **The store half is the doom row's shape.** `0x89 /6` `mov word [ss:m], si` carries 9,776,315
///    doom exits and the load half `0x8B /2` `mov edx, [ss:m32]` another 9,776,202 -- together
///    97.63% of doom's whole rejected class.
#[test]
fn a_mid_block_access_through_a_segment_override_matches_the_interpreter() {
    // Well clear of the code page, and far enough apart that the five bases cannot alias.
    const OFFSET: u32 = 0x1800;
    const LOADED: u32 = 0xdead_beef;
    const DECOY: u32 = 0x1111_2222;
    const STORED: u16 = 0xc0de;

    for (segment, selector, prefix) in OVERRIDE_CASES {
        let base = u32::from(selector) << 4;
        let mut code = vec![
            0x90, // starter -- the entry position, deliberately not a tested opcode
            0x40, // inc eax
        ];
        // mov edx, [seg:OFFSET] -- the 0x8B /2 row
        code.push(prefix);
        code.extend_from_slice(&[0x8b, 0x15]);
        code.extend_from_slice(&OFFSET.to_le_bytes());
        // mov word [seg:OFFSET+8], si -- the 0x89 /6 row, doom's prefix mask 97
        code.push(prefix);
        code.extend_from_slice(&[0x66, 0x89, 0x35]);
        code.extend_from_slice(&(OFFSET + 8).to_le_bytes());
        code.extend_from_slice(&[
            0x43, // inc ebx
            0xf4, // hlt
        ]);

        let mut memory = vec![0; 0x4000];
        memory[0x100..0x100 + code.len()].copy_from_slice(&code);
        let hit = (base + OFFSET) as usize;
        memory[hit..hit + 4].copy_from_slice(&LOADED.to_le_bytes());
        // The decoy sits where the DEFAULT segment would reach. DS is the default for a `[disp32]`
        // operand, so the DS case has no decoy to place and is carried instead by the EBP fixture
        // below, where the default is SS and the override genuinely moves the access.
        if base != 0 {
            memory[OFFSET as usize..OFFSET as usize + 4].copy_from_slice(&DECOY.to_le_bytes());
        }

        let mut interp = segment_override_cpu();
        let mut native = segment_override_cpu();
        let mut interp_bus = TestBus::with_memory(memory.clone());
        let mut native_bus = TestBus::with_memory(memory);
        for bus in [&mut interp_bus, &mut native_bus] {
            bus.direct_pages_enabled = true;
            bus.direct_page_clocks = true;
        }
        for cpu in [&mut interp, &mut native] {
            cpu.registers.set_esp(0x0f00);
            cpu.registers.set_esi(u32::from(STORED));
        }
        drive(&mut interp, &mut interp_bus);
        drive(&mut native, &mut native_bus);
        native.set_jit_auto_admit(true);
        for _ in 0..3 {
            native.halted = false;
            native.registers.eip = 0x100;
            drive(&mut native, &mut native_bus);
        }
        assert_eq!(
            native.jit_direct.len(),
            1,
            "{segment:?}: one block installed"
        );

        for cpu in [&mut interp, &mut native] {
            cpu.halted = false;
            cpu.registers.eip = 0x100;
            cpu.registers.gpr.fill(0);
            cpu.registers.set_esp(0x0f00);
            cpu.registers.set_esi(u32::from(STORED));
            cpu.registers.eflags = 0x202;
            cpu.pending_flags = PendingFlags::default();
            cpu.elapsed_clocks = 0;
            cpu.timing_rem = 0;
            cpu.core_clocks_so_far = 0;
        }
        for bus in [&mut interp_bus, &mut native_bus] {
            bus.memory[hit + 8..hit + 12].fill(0);
            bus.trace = BusTrace::default();
        }
        let direct_before = native.perf_counters().jit_direct_insns;
        let side_before = native.perf_counters().jit_direct_side_exits;

        let interp_outcomes = drive(&mut interp, &mut interp_bus);
        let native_outcomes = drive(&mut native, &mut native_bus);

        assert_eq!(native_outcomes, interp_outcomes, "{segment:?}: outcomes");
        assert_eq!(native, interp, "{segment:?}: whole CPU state");
        assert_eq!(native_bus.memory, interp_bus.memory, "{segment:?}: RAM");
        assert_eq!(
            native_bus.trace.elapsed_clocks(),
            interp_bus.trace.elapsed_clocks(),
            "{segment:?}: aggregate bus clocks"
        );

        // The absolute controls: the OVERRIDE, not the default segment, chose both addresses.
        assert_eq!(
            native.registers.edx(),
            LOADED,
            "{segment:?}: the load must have read through the override, not through DS"
        );
        assert_eq!(
            u16::from_le_bytes(native_bus.memory[hit + 8..hit + 10].try_into().unwrap()),
            STORED,
            "{segment:?}: the store must have landed at the overridden address"
        );
        assert_eq!(
            u16::from_le_bytes(native_bus.memory[hit + 10..hit + 12].try_into().unwrap()),
            0,
            "{segment:?}: a Word store writes two bytes, not four"
        );

        // Anti-vacuity, and it is what makes every comparison above a statement about the EMITTER
        // rather than about the interpreter compared with itself.
        //
        // The installed block starts at the INC, not at the NOP: `run_budgeted` retires a run's
        // first instruction through `cycle_no_interrupt_check_with_budget` and only then takes a
        // direct continuation, so the starter is interpreted and the block key is ENTRY+1. That is
        // exactly what the starter is for -- it puts the overridden load and store at slots 2 and 3
        // of a four-slot block, where the entry-position trap cannot hide an untested emitter arm.
        let key = jit::direct::key_for(&native, 0x101, true).unwrap();
        assert!(
            matches!(
                native.jit_direct.probe(key),
                jit::direct::BlockProbe::Ready(_)
            ),
            "{segment:?}: the mid-block entry must be a live compiled block"
        );
        assert_eq!(
            native.perf_counters().jit_direct_insns - direct_before,
            4,
            "{segment:?}: the block must span inc/load/store/inc"
        );
        assert_eq!(
            native.perf_counters().jit_direct_side_exits - side_before,
            0,
            "{segment:?}: neither overridden access may side-exit -- they must RUN natively"
        );
    }
}

/// The DS override where it is load-bearing: an EBP-based operand defaults to SS, and `3E` moves
/// it to DS.
///
/// The case above cannot test DS. A `[disp32]` operand already defaults to DS, so the prefix is
/// architecturally inert there and that fixture would pass with the override dropped on the floor.
/// This one fails if the override is ignored, because the two segments have different bases.
#[test]
fn a_ds_override_on_a_stack_relative_operand_matches_the_interpreter() {
    const LOADED: u32 = 0x5555_aaaa;
    const DECOY: u32 = 0x1111_2222;
    // DS base 0, SS base 0x800. EBP picks the offset; the segment picks which payload is reached.
    const EBP: u32 = 0x1900;

    let code = [
        0x90, // starter
        0x40, // inc eax
        0x3e, 0x8b, 0x55, 0x00, // mov edx, [ds:ebp+0], MID-BLOCK
        0x43, // inc ebx
        0xf4, // hlt
    ];
    let mut memory = vec![0; 0x4000];
    memory[0x100..0x100 + code.len()].copy_from_slice(&code);
    memory[EBP as usize..EBP as usize + 4].copy_from_slice(&LOADED.to_le_bytes());
    memory[(0x800 + EBP) as usize..(0x800 + EBP) as usize + 4]
        .copy_from_slice(&DECOY.to_le_bytes());

    let mut interp = segment_override_cpu();
    let mut native = segment_override_cpu();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    for cpu in [&mut interp, &mut native] {
        cpu.registers.set_esp(0x0f00);
        cpu.registers.set_ebp(EBP);
    }
    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..3 {
        native.halted = false;
        native.registers.eip = 0x100;
        native.registers.set_ebp(EBP);
        drive(&mut native, &mut native_bus);
    }
    assert_eq!(native.jit_direct.len(), 1);

    for cpu in [&mut interp, &mut native] {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.registers.gpr.fill(0);
        cpu.registers.set_esp(0x0f00);
        cpu.registers.set_ebp(EBP);
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
    assert_eq!(native_bus.memory, interp_bus.memory);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert_eq!(
        native.registers.edx(),
        LOADED,
        "the DS override must have moved the EBP-relative access off SS"
    );
    // Three, not four: the starter NOP is the run's first instruction and is interpreted, so the
    // block is the inc/load/inc at ENTRY+1 and the override sits at its middle slot.
    assert_eq!(native.perf_counters().jit_direct_insns - direct_before, 3);
}

/// The STALE-BASE hazard, on a segment that ONLY an override puts in the block.
///
/// `DirectKind::read_segment` carries a comment calling itself "a correctness site, not
/// bookkeeping": a memory kind that answers `None` makes `kind_segment_access_supported` trivially
/// true AND keeps the segment out of `SegmentLayout.used`, and `data_matches` SKIPS unused
/// segments, so a cached block would keep matching after the guest reloads that segment and would
/// read through the base baked into its emitted code.
///
/// Before this slice the comment was untestable for anything but DS and SS, because those are the
/// only segments a lowered kind could name. **A mutation that made `Load` answer `None` survived
/// the whole crate** — every other fixture either has a Store declaring the same segment (which
/// puts the bit back in `used`) or uses DS, which the block reads anyway.
///
/// This fixture is the first that can see it. The block reads FS and nothing else touches FS, so
/// the ONLY thing that can put FS in the pinned set is `Load::read_segment`. The guest then moves
/// FS's base between two runs and the payload differs at the two bases, so a block that kept
/// matching returns the old dword and a block that correctly refused to match returns the new one.
#[test]
fn a_load_through_an_override_pins_that_segment_against_a_guest_reload() {
    const OFFSET: u32 = 0x1800;
    const FIRST_SELECTOR: u16 = 0x00c0; // base 0x0c00
    const SECOND_SELECTOR: u16 = 0x0140; // base 0x1400
    const FIRST_PAYLOAD: u32 = 0x1111_1111;
    const SECOND_PAYLOAD: u32 = 0x2222_2222;

    let code = [
        0x90, // starter
        0x40, // inc eax
        0x64, 0x8b, 0x15, 0x00, 0x18, 0x00, 0x00, // mov edx, [fs:0x1800] -- the ONLY FS user
        0x43, // inc ebx
        0xf4, // hlt
    ];
    let mut memory = vec![0; 0x4000];
    memory[0x100..0x100 + code.len()].copy_from_slice(&code);
    let first = ((u32::from(FIRST_SELECTOR) << 4) + OFFSET) as usize;
    let second = ((u32::from(SECOND_SELECTOR) << 4) + OFFSET) as usize;
    memory[first..first + 4].copy_from_slice(&FIRST_PAYLOAD.to_le_bytes());
    memory[second..second + 4].copy_from_slice(&SECOND_PAYLOAD.to_le_bytes());

    let mut interp = segment_override_cpu();
    let mut native = segment_override_cpu();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    for cpu in [&mut interp, &mut native] {
        cpu.load_segment_real(SegmentIndex::Fs, FIRST_SELECTOR);
        cpu.registers.set_esp(0x0f00);
    }
    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..3 {
        native.halted = false;
        native.registers.eip = 0x100;
        drive(&mut native, &mut native_bus);
    }
    assert_eq!(native.jit_direct.len(), 1);
    assert_eq!(
        native.registers.edx(),
        FIRST_PAYLOAD,
        "control: while FS still names the first base, the block must read the first payload"
    );

    // The guest reloads FS. The block's emitted code has the OLD base baked into it, so the only
    // thing that can stop it running is FS being in the pinned set.
    for cpu in [&mut interp, &mut native] {
        cpu.load_segment_real(SegmentIndex::Fs, SECOND_SELECTOR);
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.registers.gpr.fill(0);
        cpu.registers.set_esp(0x0f00);
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.trace = BusTrace::default();
    }

    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(native_outcomes, interp_outcomes);
    assert_eq!(native, interp);
    assert_eq!(
        native.registers.edx(),
        SECOND_PAYLOAD,
        "the reloaded FS must be honoured: a block that kept matching would return the payload at \
         the STALE base, which is exactly what `read_segment`'s comment warns about"
    );
}

/// The NULL-SELECTOR and ACCESS-RIGHTS hazards, and the point is the same as the limit fixture's:
/// slice 6 invents no path for either, it merely reaches the existing one for the first time.
///
/// An override can name a segment that is null, or that cannot be accessed the way the instruction
/// wants. Both die in `segment_access_supported`, consulted from `kind_segment_access_supported`
/// on every slot and again from `SegmentLayout::capture` at the end of the walk:
///
///  * a segment loaded with a null selector installs `SegmentRegister::default()` — `access == 0`,
///    so the present bit is clear (`control.rs:1248`: "a later memory access through it faults with
///    #GP(0)"), and
///  * a CODE descriptor in a data segment register fails the `!write || (!code && writable)` clause
///    on any store.
///
/// The refusal is **fail-closed and it is a `Retry`, not a `StructuralReject`**: no block forms, no
/// rejected span is installed, and the interpreter executes the instruction and takes whatever
/// fault its own rules say. That is what every already-admitted DS and SS access does today.
///
/// **A NEVER-LOADED FS or GS is a different case and is NOT refused, deliberately.**
/// `Registers::default` seeds all six data segments with `SegmentRegister::real(0)` — access 0x93,
/// present, writable, base 0, limit 0xFFFF — so a guest that enters protected mode without ever
/// loading FS has a perfectly accessible FS descriptor and an override through it COMPILES. That is
/// correct twice over: the JIT and the interpreter read the same `registers.segment(segment)`, so
/// they form the same linear address and take the same limit fault; and it matches real silicon,
/// where entering protected mode does not reload the descriptor caches. The compile-time check is
/// strictly stricter than the interpreter's runtime one, never looser. What keeps that safe over
/// time is not this fixture but
/// `a_load_through_an_override_pins_that_segment_against_a_guest_reload`, which pins the block
/// against the moment the guest finally does load the segment.
///
/// Each case carries a control that differs ONLY in the descriptor, so the refusal cannot be the
/// override's doing, the opcode's, or the fixture's shape.
#[test]
fn an_override_naming_an_inaccessible_segment_refuses_to_compile_at_all() {
    const ENTRY: u32 = 0x100;
    const NULL_SEGMENT: SegmentRegister = SegmentRegister {
        selector: 0,
        base: 0,
        limit: 0,
        access: 0,
        default_size_32: false,
    };

    let load = [0x40, 0x41, 0x64, 0x8b, 0x15, 0x00, 0x08, 0x00, 0x00];
    let store = [0x40, 0x41, 0x26, 0x89, 0x15, 0x00, 0x08, 0x00, 0x00];

    for (label, code, segment, refused, admitted) in [
        (
            "null FS on a read",
            load,
            SegmentIndex::Fs,
            NULL_SEGMENT,
            SegmentRegister::flat(0x10, 0x93),
        ),
        (
            "code-segment ES on a write",
            store,
            SegmentIndex::Es,
            SegmentRegister::flat(0x18, 0x9b),
            SegmentRegister::flat(0x10, 0x93),
        ),
    ] {
        for (expect_block, descriptor) in [(false, refused), (true, admitted)] {
            let mut memory = vec![0; 0x2000];
            memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
            let mut bus = TestBus::with_memory(memory);
            bus.direct_pages_enabled = true;

            let mut cpu = flat_stack_cpu(ENTRY);
            cpu.registers.set_segment(segment, descriptor);
            for lin in [ENTRY, ENTRY + 1, ENTRY + 2] {
                cpu.set_eip(lin);
                cpu.fetch_decoded(&mut bus, lin).expect("fixture decode");
            }
            cpu.set_eip(ENTRY);
            cpu.jit_direct.set_fast_map_enabled_for_test(true);
            map_direct_page(
                &mut cpu,
                &mut bus,
                0,
                0,
                jit::fast_map::PagePermissions::UNPAGED,
                true,
                true,
            );

            match jit::direct::compile(&mut cpu, ENTRY, true) {
                jit::direct::CompileOutcome::Compiled(compilation) => {
                    assert!(expect_block, "{label}: must NOT have compiled");
                    assert_eq!(
                        compilation.span.instructions, 3,
                        "{label} control: the block must span both INCs and the access"
                    );
                }
                jit::direct::CompileOutcome::Retry => assert!(
                    !expect_block,
                    "{label} control: the accessible descriptor must compile"
                ),
                jit::direct::CompileOutcome::StructuralReject(_) => panic!(
                    "{label}: an inaccessible segment must fail CLOSED as a Retry -- a structural \
                     reject would install a rejected span and a page watch for a block that is \
                     merely unformable in this segment state"
                ),
            }
        }
    }
}

/// The LIMIT hazard, and the point is that slice 6 invents NO fault path for it.
///
/// An override can name a segment whose limit excludes the access. Nothing new is needed:
/// `emit_segmented_linear_address` already compares the effective address against
/// `descriptor.limit - (width - 1)` for every segment whose limit is not 4 GB and jumps to the
/// `SegmentLimit` side exit when it fails, and the side exit rolls back to the instruction
/// boundary so the INTERPRETER re-executes and faults by its own rules. Because the override is
/// folded into `addr.segment` at decode, the interpreter's `segment_limit_fault(segment)` picks
/// the vector off the OVERRIDDEN segment too -- **#SS(12) for an SS override, #GP(13) otherwise**
/// -- which is the architectural rule, reached with no code written for it.
///
/// Both halves are load-bearing, and each pins something the other cannot.
///
///  * The **FS** case pins that the guard fires on a segment the DEFAULT would have allowed. The
///    operand is `[disp32]`, whose default segment is a flat 64 KB DS that admits offset 0x1800
///    without complaint; only the override moves it onto the short-limit FS. Drop the decode fold
///    and this half stops side-exiting at all.
///  * The **SS** case pins the vector SPLIT, and it is measured rather than tabulated: the two
///    vectors have distinct real-mode IVT handlers, and the assertion reads the delivered vector
///    back out of EIP after the interpreter's re-run. An earlier draft asserted
///    `segment_limit_fault(segment)` against a loop constant, which is a table test of a `const fn`
///    wearing a fixture's name -- it never touched either role's state and would have passed with
///    the whole slice reverted.
#[test]
fn an_overridden_access_past_the_segment_limit_side_exits_and_faults_by_its_own_segment() {
    const ENTRY: u32 = 0x100;
    const LOAD: u32 = ENTRY + 2;
    const OFFSET: u32 = 0x1800;
    const INITIAL_ESP: u32 = 0x0f00;
    // One byte short of the four the load needs: `max_start = limit - 3` refuses OFFSET.
    const SHORT_LIMIT: u32 = OFFSET + 2;
    // Indexed by `vector - 12`: #SS then #GP.
    const HANDLER_OFFSET: [u32; 2] = [0x0300, 0x0380];

    for (segment, prefix, base, vector) in [
        (SegmentIndex::Ss, 0x36u8, 0x0800u32, 12u32),
        (SegmentIndex::Fs, 0x64u8, 0x0c00u32, 13u32),
    ] {
        let mut code = vec![
            0x40, // inc eax
            0x41, // inc ecx
        ];
        code.push(prefix);
        code.extend_from_slice(&[0x8b, 0x15]);
        code.extend_from_slice(&OFFSET.to_le_bytes());
        let mut memory = vec![0; 0x4000];
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
        // A payload at the address the load WOULD reach. Without it EDX reads zero whether the
        // load faulted or completed, and the "the load never happened" assertion below would hold
        // vacuously.
        let payload = (base + OFFSET) as usize;
        memory[payload..payload + 4].copy_from_slice(&0xdead_beefu32.to_le_bytes());
        // Real-mode IVT entries for #SS(12) and #GP(13), at DISTINCT handler offsets in segment 0.
        // This is what turns the vector claim into a measurement: the vector is read back out of
        // EIP after the fault is delivered, rather than asserted from a table of loop constants.
        let handler = HANDLER_OFFSET[usize::try_from(vector).unwrap() - 12];
        for (v, offset) in [(12u32, HANDLER_OFFSET[0]), (13, HANDLER_OFFSET[1])] {
            let slot = (v * 4) as usize;
            memory[slot..slot + 2].copy_from_slice(&(offset as u16).to_le_bytes());
            memory[slot + 2..slot + 4].copy_from_slice(&0u16.to_le_bytes());
        }

        let mut native = segment_override_cpu();
        let mut interp = segment_override_cpu();
        let mut native_bus = TestBus::with_memory(memory.clone());
        let mut interp_bus = TestBus::with_memory(memory);
        for bus in [&mut native_bus, &mut interp_bus] {
            bus.direct_pages_enabled = true;
            bus.direct_page_clocks = true;
        }
        for cpu in [&mut native, &mut interp] {
            // Short enough to refuse the load, long enough that the SS case can still push the
            // fault frame -- otherwise the SS half would measure a double fault instead.
            let mut descriptor = cpu.registers.segment(segment);
            descriptor.limit = SHORT_LIMIT;
            cpu.registers.set_segment(segment, descriptor);
            cpu.registers.set_esp(INITIAL_ESP);
            cpu.registers.eip = ENTRY;
        }

        for lin in [ENTRY, ENTRY + 1, LOAD] {
            for (cpu, bus) in [
                (&mut native, &mut native_bus),
                (&mut interp, &mut interp_bus),
            ] {
                cpu.registers.eip = lin;
                cpu.fetch_decoded(bus, lin).expect("fixture decode");
            }
        }
        for cpu in [&mut native, &mut interp] {
            cpu.registers.eip = ENTRY;
        }
        native.jit_direct.set_fast_map_enabled_for_test(true);
        map_direct_page(
            &mut native,
            &mut native_bus,
            0,
            0,
            jit::fast_map::PagePermissions::UNPAGED,
            true,
            true,
        );

        let key = jit::direct::key_for(&native, ENTRY, true).unwrap();
        assert!(matches!(
            native.jit_direct.probe(key),
            jit::direct::BlockProbe::Interpret
        ));
        let compilation = jit::direct::compile(&mut native, ENTRY, true).unwrap();
        assert_eq!(
            compilation.span.instructions, 3,
            "{segment:?}: the limit is a RUNTIME guard, so the block must still span the load"
        );
        let id = native.jit_direct.install(&compilation).unwrap();
        let block = native
            .jit_direct
            .block(id)
            .expect("installed block must be live");

        let side_exits = native.perf_counters().jit_direct_side_exits;
        let limit_exits = native.direct_stall_snapshot().side_exit_segment_limit;
        let insns_before = native.perf_counters().jit_direct_insns;

        assert!(
            native
                .try_run_direct_block_for_test(&mut native_bus, block)
                .unwrap()
        );
        interp.cycle(&mut interp_bus).unwrap();
        interp.cycle(&mut interp_bus).unwrap();

        assert_eq!(native.registers, interp.registers, "{segment:?}: registers");
        assert_eq!(
            native.registers.eip, LOAD,
            "{segment:?}: the side exit must leave EIP at the load, before any effect"
        );
        assert_eq!(
            native.perf_counters().jit_direct_side_exits - side_exits,
            1,
            "{segment:?}: exactly one side exit"
        );
        assert_eq!(
            native.direct_stall_snapshot().side_exit_segment_limit - limit_exits,
            1,
            "{segment:?}: and it must be the SegmentLimit guard, not some other one"
        );
        // Anti-vacuity: the two fillers retired natively, the load did not.
        assert_eq!(
            native.perf_counters().jit_direct_insns - insns_before,
            2,
            "{segment:?}"
        );

        // The fault itself is the INTERPRETER's, taken on the re-run, and it is the same fault on
        // both roles.
        let native_step = native.cycle(&mut native_bus);
        let interp_step = interp.cycle(&mut interp_bus);
        assert_eq!(
            format!("{native_step:?}"),
            format!("{interp_step:?}"),
            "{segment:?}: the re-run must produce the same outcome on both roles"
        );
        assert_eq!(
            native.registers, interp.registers,
            "{segment:?}: post-fault"
        );
        assert_eq!(
            native_bus.memory, interp_bus.memory,
            "{segment:?}: post-fault RAM"
        );
        assert_eq!(
            native.registers.edx(),
            0,
            "{segment:?}: the load must have FAULTED, not completed -- the payload is 0xdeadbeef"
        );
        assert_ne!(
            native.registers.eip, LOAD,
            "{segment:?}: the interpreter's re-run must have taken the fault and vectored away"
        );
        // THE VECTOR SPLIT, measured. EIP now holds whichever IVT handler the delivered vector
        // named, and the two handlers are distinct, so this reads the vector out of the machine
        // rather than asserting it from the loop's own constants. An SS override must take
        // #SS(12) and an FS override #GP(13) -- the architectural rule, which falls out of the
        // decode fold putting the override into `addr.segment` before `segment_limit_fault` ever
        // sees it. Cross-checked against `segment_limit_fault`'s own answer so that a change to
        // either the delivery path or the classifier alone breaks this.
        assert_eq!(
            native.registers.eip, handler,
            "{segment:?}: the fault must vector through IVT[{vector}]"
        );
        assert!(
            matches!(
                segment_limit_fault(segment),
                InternalFault::Exception { vector: v, error_code: Some(0) }
                    if u32::from(v) == vector
            ),
            "{segment:?}: and the classifier must name the same vector the machine delivered"
        );
    }
}

/// T4 of the watched-page-bit battery (the fast path really is fast): with the bit emission ON,
/// an unwatched-by-bit store consults NOTHING — proven by deliberately constructing the
/// incoherent state the strict-edge sweeps exist to prevent (a live bit-clear entry for a page
/// the published tables say is watched, via a RAW `mark_range` that bypasses the sweep) and
/// observing the store LAND natively with zero code-watch exits. The OFF arm emits the
/// pre-slice unconditional guard against the identical state and takes the exit — which is what
/// proves the ON arm's skip is the bit test, not an accident of the fixture.
#[test]
fn a_stale_clear_bit_skips_the_guard_and_the_off_arm_still_probes() {
    let target = 0x4100u32;
    for (bit_on, expected_watch_exits) in [(true, 0u64), (false, 1u64)] {
        let mut cpu = fresh();
        cpu.jit_direct.watch_page_bit = bit_on;
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
            cpu.physical_page_watched(target),
        ));
        assert!(!cpu.jit_fast_map.page_watched_bit_for_test(target));
        // The raw mark: the published table gains the page, no sweep runs, the entry above
        // keeps its clear bit. Production cannot reach this state — T1/T2 in
        // `cpu_jit_watch_bit_test.rs` are what pin that — so constructing it is the only way
        // to observe which of the two the emitted store believes.
        let _ = cpu.decode_cache.native_code_watch.mark_range(target, 1);
        bus.trace = BusTrace::default();
        arm_store_fixture(&mut cpu);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
        let watch_exits = cpu.perf_counters().jit_direct_exit_code_watch;
        drive(&mut cpu, &mut bus);
        assert_eq!(
            cpu.perf_counters().jit_direct_exit_code_watch - watch_exits,
            expected_watch_exits,
            "bit_on={bit_on}"
        );
        assert_eq!(
            &bus.memory[target as usize..target as usize + 4],
            &0x1234_5678u32.to_le_bytes(),
            "bit_on={bit_on}: the store must land either way (natively or via the exit's re-run)"
        );
    }
}
