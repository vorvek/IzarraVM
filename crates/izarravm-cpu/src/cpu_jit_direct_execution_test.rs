// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const GAME_LOOP_ENTRY: u32 = 0x101;

fn invoke_native_entry(
    cpu: &mut CpuGsw,
    block: jit::direct::CompiledBlock,
    quota: u32,
) -> jit::direct::NativeExit {
    let mut exit = jit::direct::NativeExit::default();
    let entry: jit::direct::DirectEntryFn = unsafe { std::mem::transmute(block.entry_ptr()) };
    let flags = cpu.eflags();
    unsafe {
        entry(
            cpu as *mut CpuGsw,
            flags,
            quota,
            &mut exit as *mut jit::direct::NativeExit,
        );
    }
    exit
}

fn assert_unresolved_reason_deltas(
    before: &PerfCounters,
    after: &PerfCounters,
    expected: [u64; 4],
) {
    let actual = [
        after.jit_direct_unresolved_static_unbound - before.jit_direct_unresolved_static_unbound,
        after.jit_direct_unresolved_static_hidden - before.jit_direct_unresolved_static_hidden,
        after.jit_direct_unresolved_dynamic_miss_or_unbound
            - before.jit_direct_unresolved_dynamic_miss_or_unbound,
        after.jit_direct_unresolved_dynamic_hidden - before.jit_direct_unresolved_dynamic_hidden,
    ];
    assert_eq!(actual, expected);
    assert_eq!(
        after.jit_direct_unresolved_exits - before.jit_direct_unresolved_exits,
        actual.into_iter().sum::<u64>()
    );
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

    // The first RET above observed and bound RETURN. On the next identical call chain, the RET
    // stays native and reaches the return site without another host entry.
    arm_stack_fixture(&mut native, ENTRY, INITIAL_ESP);
    arm_stack_fixture(&mut interp, ENTRY, INITIAL_ESP);
    native_bus.memory.copy_from_slice(&pristine);
    interp_bus.memory.copy_from_slice(&pristine);
    native_bus.trace = BusTrace::default();
    interp_bus.trace = BusTrace::default();
    let entries = native.perf_counters().jit_direct_entries;
    let transfers = native.perf_counters().jit_direct_linked_transfers;

    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, entry_block)
            .unwrap()
    );
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
    assert_eq!(native.registers.eip, HALT);
    assert_eq!(native.perf_counters().jit_direct_entries - entries, 1);
    assert_eq!(
        native.perf_counters().jit_direct_linked_transfers - transfers,
        4
    );
}

#[test]
fn direct_ret_pic_keeps_two_return_sites_hot_and_matches_interpreter() {
    const ENTRY: u32 = 0x180;
    const FIRST: u32 = 0x200;
    const FIRST_HALT: u32 = 0x210;
    const SECOND: u32 = 0x220;
    const SECOND_HALT: u32 = 0x230;
    const INITIAL_ESP: u32 = 0x3000;

    let mut pristine = vec![0; 0x5000];
    pristine[ENTRY as usize] = 0xc3;
    pristine[FIRST as usize..FIRST as usize + 9].copy_from_slice(&[
        0xb8, 0x11, 0, 0, 0, // mov eax,0x11
        0x89, 0xc1, // mov ecx,eax
        0xeb, 0x07, // jmp FIRST_HALT
    ]);
    pristine[FIRST_HALT as usize] = 0xf4;
    pristine[SECOND as usize..SECOND as usize + 9].copy_from_slice(&[
        0xb8, 0x22, 0, 0, 0, // mov eax,0x22
        0x89, 0xc1, // mov ecx,eax
        0xeb, 0x07, // jmp SECOND_HALT
    ]);
    pristine[SECOND_HALT as usize] = 0xf4;

    let mut native = flat_stack_cpu(ENTRY);
    let mut interp = flat_stack_cpu(ENTRY);
    let mut native_bus = TestBus::with_memory(pristine.clone());
    let mut interp_bus = TestBus::with_memory(pristine.clone());
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [
        ENTRY,
        FIRST,
        FIRST + 5,
        FIRST + 7,
        SECOND,
        SECOND + 5,
        SECOND + 7,
    ];
    decode_fixture(&mut native, &mut native_bus, &starts);
    decode_fixture(&mut interp, &mut interp_bus, &starts);
    map_direct_page(
        &mut native,
        &mut native_bus,
        INITIAL_ESP,
        INITIAL_ESP,
        jit::fast_map::PagePermissions::UNPAGED,
        true,
        false,
    );
    let ret_block = install_fixture_block(&mut native, ENTRY);
    let first_block = install_fixture_block(&mut native, FIRST);
    let second_block = install_fixture_block(&mut native, SECOND);

    // Populate both ways. The first execution has quota one but still reports the target for
    // binding. The second target misses the first tag and occupies the other way.
    for target in [FIRST, SECOND] {
        arm_stack_fixture(&mut native, ENTRY, INITIAL_ESP);
        native_bus.memory.copy_from_slice(&pristine);
        native_bus.memory[INITIAL_ESP as usize..INITIAL_ESP as usize + 4]
            .copy_from_slice(&target.to_le_bytes());
        let before = native.perf_counters().clone();
        assert!(
            native
                .try_run_direct_block_for_test(&mut native_bus, ret_block)
                .unwrap()
        );
        assert_eq!(native.registers.eip, target);
        assert_unresolved_reason_deltas(&before, native.perf_counters(), [0, 0, 1, 0]);
    }

    for (target, expected_halt, expected_value) in [
        (FIRST, FIRST_HALT, 0x11),
        (SECOND, SECOND_HALT, 0x22),
        (FIRST, FIRST_HALT, 0x11),
        (SECOND, SECOND_HALT, 0x22),
    ] {
        arm_stack_fixture(&mut native, ENTRY, INITIAL_ESP);
        arm_stack_fixture(&mut interp, ENTRY, INITIAL_ESP);
        native_bus.memory.copy_from_slice(&pristine);
        interp_bus.memory.copy_from_slice(&pristine);
        native_bus.memory[INITIAL_ESP as usize..INITIAL_ESP as usize + 4]
            .copy_from_slice(&target.to_le_bytes());
        interp_bus.memory[INITIAL_ESP as usize..INITIAL_ESP as usize + 4]
            .copy_from_slice(&target.to_le_bytes());
        native_bus.trace = BusTrace::default();
        interp_bus.trace = BusTrace::default();
        let before = native.perf_counters().clone();

        assert!(
            native
                .try_run_direct_block_for_test(&mut native_bus, ret_block)
                .unwrap()
        );
        for _ in 0..4 {
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
        assert_eq!(native.registers.eip, expected_halt);
        assert_eq!(native.registers.eax(), expected_value);
        assert_eq!(native.registers.ecx(), expected_value);
        assert_eq!(
            native.perf_counters().jit_direct_entries - before.jit_direct_entries,
            1
        );
        assert_eq!(
            native.perf_counters().jit_direct_linked_transfers - before.jit_direct_linked_transfers,
            1
        );
        assert_unresolved_reason_deltas(&before, native.perf_counters(), [1, 0, 0, 0]);
    }

    // Exercise the first PIC way independently. A matching tag whose real portal has no body is
    // a hidden target, not an unbound dynamic miss.
    assert!(native.jit_direct.hide_portal_for_test(first_block.id()));
    assert!(!native.jit_direct.is_link_visible(first_block.id()));
    assert!(native.jit_direct.is_link_visible(second_block.id()));
    arm_stack_fixture(&mut native, ENTRY, INITIAL_ESP);
    native_bus.memory.copy_from_slice(&pristine);
    native_bus.memory[INITIAL_ESP as usize..INITIAL_ESP as usize + 4]
        .copy_from_slice(&FIRST.to_le_bytes());
    let before = native.perf_counters().clone();
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, ret_block)
            .unwrap()
    );
    assert_eq!(native.registers.eip, FIRST);
    assert_eq!(native.registers.eax(), 0);
    assert_eq!(native.registers.ecx(), 0);
    assert_unresolved_reason_deltas(&before, native.perf_counters(), [0, 0, 0, 1]);

    native.jit_direct.set_auto_admit(true);
    native.jit_direct.set_admission_heat_for_test(1);
    native.set_eip(FIRST);
    native
        .try_direct_continuation_for_test(&mut native_bus, FIRST, true)
        .unwrap();
    native.jit_direct.set_auto_admit(false);
    assert!(native.jit_direct.is_link_visible(first_block.id()));

    // Hide only the second target's body. Its PIC tag and logical edge remain in place while the
    // first way stays live, so a return to SECOND must commit the transfer and stop at the hidden
    // portal instead of falling through to either native target.
    assert!(native.jit_direct.hide_portal_for_test(second_block.id()));
    assert!(native.jit_direct.is_link_visible(ret_block.id()));
    assert!(native.jit_direct.is_link_visible(first_block.id()));
    assert!(!native.jit_direct.is_link_visible(second_block.id()));

    arm_stack_fixture(&mut native, ENTRY, INITIAL_ESP);
    arm_stack_fixture(&mut interp, ENTRY, INITIAL_ESP);
    native_bus.memory.copy_from_slice(&pristine);
    interp_bus.memory.copy_from_slice(&pristine);
    native_bus.memory[INITIAL_ESP as usize..INITIAL_ESP as usize + 4]
        .copy_from_slice(&SECOND.to_le_bytes());
    interp_bus.memory[INITIAL_ESP as usize..INITIAL_ESP as usize + 4]
        .copy_from_slice(&SECOND.to_le_bytes());
    native_bus.trace = BusTrace::default();
    interp_bus.trace = BusTrace::default();
    let memory_before = native_bus.memory.clone();
    let flags_before = native.registers.eflags;
    let before = native.perf_counters().clone();

    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, ret_block)
            .unwrap()
    );
    interp.cycle(&mut interp_bus).unwrap();

    assert_eq!(native.registers, interp.registers);
    assert_eq!(native.pending_flags, interp.pending_flags);
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
    assert_eq!(native_bus.memory, interp_bus.memory);
    assert_eq!(native_bus.memory, memory_before);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert_eq!(native.registers.eip, SECOND);
    assert_eq!(native.registers.esp(), INITIAL_ESP + 4);
    assert_eq!(native.registers.eax(), 0);
    assert_eq!(native.registers.ecx(), 0);
    assert_eq!(native.registers.eflags, flags_before);
    assert_eq!(native.perf_counters().instructions - before.instructions, 1);
    assert_eq!(
        native.perf_counters().jit_direct_entries - before.jit_direct_entries,
        1
    );
    assert_eq!(
        native.perf_counters().jit_direct_insns - before.jit_direct_insns,
        1
    );
    assert_eq!(
        native.perf_counters().jit_direct_linked_transfers - before.jit_direct_linked_transfers,
        0
    );
    assert_eq!(
        native.perf_counters().jit_direct_unresolved_exits - before.jit_direct_unresolved_exits,
        1
    );
    assert_unresolved_reason_deltas(&before, native.perf_counters(), [0, 0, 0, 1]);
    assert_eq!(
        native.perf_counters().jit_direct_side_exits - before.jit_direct_side_exits,
        0
    );
    assert!(!native.jit_direct.is_link_visible(second_block.id()));

    // The normal checked continuation path validates the still-live decode lines and republishes
    // the portal. Reset after that probe, then prove the RET reaches the target in the same entry.
    native.jit_direct.set_auto_admit(true);
    native.jit_direct.set_admission_heat_for_test(1);
    native.set_eip(SECOND);
    native
        .try_direct_continuation_for_test(&mut native_bus, SECOND, true)
        .unwrap();
    native.jit_direct.set_auto_admit(false);
    assert!(native.jit_direct.is_link_visible(second_block.id()));

    // A live matching edge that exhausts its quota commits the RET but neither enters the target
    // nor reports an unresolved reason.
    arm_stack_fixture(&mut native, ENTRY, INITIAL_ESP);
    native_bus.memory.copy_from_slice(&pristine);
    native_bus.memory[INITIAL_ESP as usize..INITIAL_ESP as usize + 4]
        .copy_from_slice(&SECOND.to_le_bytes());
    let quota_exit = invoke_native_entry(&mut native, ret_block, 1);
    assert_eq!(native.registers.eip, SECOND);
    assert_eq!(native.registers.esp(), INITIAL_ESP + 4);
    assert_eq!(quota_exit.instructions, 1);
    assert_eq!(quota_exit.linked_transfers, 0);
    assert_eq!(
        quota_exit.unresolved_reason,
        jit::direct::UnresolvedReason::None
    );
    assert_eq!(quota_exit.dynamic_link_cell, 0);
    assert_eq!(quota_exit.dynamic_target_eip, 0);

    arm_stack_fixture(&mut native, ENTRY, INITIAL_ESP);
    native_bus.memory.copy_from_slice(&pristine);
    native_bus.memory[INITIAL_ESP as usize..INITIAL_ESP as usize + 4]
        .copy_from_slice(&SECOND.to_le_bytes());
    native_bus.trace = BusTrace::default();
    let before = native.perf_counters().clone();

    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, ret_block)
            .unwrap()
    );
    assert_eq!(native.registers.eip, SECOND_HALT);
    assert_eq!(native.registers.esp(), INITIAL_ESP + 4);
    assert_eq!(native.registers.eax(), 0x22);
    assert_eq!(native.registers.ecx(), 0x22);
    assert_eq!(native.perf_counters().instructions - before.instructions, 4);
    assert_eq!(
        native.perf_counters().jit_direct_entries - before.jit_direct_entries,
        1
    );
    assert_eq!(
        native.perf_counters().jit_direct_insns - before.jit_direct_insns,
        4
    );
    assert_eq!(
        native.perf_counters().jit_direct_linked_transfers - before.jit_direct_linked_transfers,
        1
    );
    assert_eq!(
        native.perf_counters().jit_direct_unresolved_exits - before.jit_direct_unresolved_exits,
        1
    );
    assert_unresolved_reason_deltas(&before, native.perf_counters(), [1, 0, 0, 0]);
}

#[test]
fn static_hidden_portal_reports_reason_before_target_body() {
    const ENTRY: u32 = 0x300;
    const TARGET: u32 = 0x320;

    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize..ENTRY as usize + 10].copy_from_slice(&[
        0xb8, 1, 0, 0, 0, // mov eax,1
        0xe9, 0x16, 0, 0, 0, // jmp TARGET
    ]);
    memory[TARGET as usize..TARGET as usize + 2].copy_from_slice(&[
        0xeb, 0xfe, // jmp TARGET
    ]);

    let mut cpu = flat_stack_cpu(ENTRY);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    decode_fixture(&mut cpu, &mut bus, &[ENTRY, ENTRY + 5, TARGET]);
    let source = install_fixture_block(&mut cpu, ENTRY);
    let target = install_fixture_block(&mut cpu, TARGET);
    assert!(cpu.jit_direct.has_linked_successor(source));

    assert!(cpu.jit_direct.hide_portal_for_test(target.id()));
    assert!(cpu.jit_direct.is_link_visible(source.id()));
    assert!(!cpu.jit_direct.is_link_visible(target.id()));

    arm_stack_fixture(&mut cpu, ENTRY, 0x800);
    let before = cpu.perf_counters().clone();
    assert!(cpu.try_run_direct_block_for_test(&mut bus, source).unwrap());

    assert_eq!(cpu.registers.eip, TARGET);
    assert_eq!(cpu.registers.eax(), 1);
    assert_eq!(cpu.perf_counters().instructions - before.instructions, 2);
    assert_eq!(
        cpu.perf_counters().jit_direct_linked_transfers - before.jit_direct_linked_transfers,
        0
    );
    assert_unresolved_reason_deltas(&before, cpu.perf_counters(), [0, 1, 0, 0]);
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
    assert_eq!(native.registers, registers);
    assert_eq!(native.pending_flags, pending);
    assert_eq!(native_bus.memory, memory);
    assert_eq!(native.perf_counters().jit_direct_exit_code_watch - exits, 1);
    for _ in 0..3 {
        native.cycle(&mut native_bus).unwrap();
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
fn direct_memory_alu_watched_writes_exit_transactionally() {
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

/// Run one program natively and interpreted from identical state and require identical
/// architectural results, INCLUDING the raw `pending_flags` descriptor.
///
/// Comparing `eflags()` alone is not enough and that is the whole point of these fixtures: a
/// single-bit CF write that eagerly materializes agrees with the interpreter on `eflags()` and
/// differs on every byte of the descriptor, which is exactly the divergence class the campaign's
/// anchors would surface only on a real corpus run.
fn assert_native_matches_interpreter(code: &[u8], starts: &[u32], arm: impl Fn(&mut CpuGsw)) {
    const ENTRY: u32 = 0x101;
    let mut pristine = vec![0; 0x5000];
    pristine[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(code);

    let mut native = flat_stack_cpu(ENTRY);
    let mut interp = flat_stack_cpu(ENTRY);
    let mut native_bus = TestBus::with_memory(pristine.clone());
    let mut interp_bus = TestBus::with_memory(pristine.clone());
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    decode_fixture(&mut native, &mut native_bus, starts);
    decode_fixture(&mut interp, &mut interp_bus, starts);

    let block = install_fixture_block(&mut native, ENTRY);
    assert_eq!(
        usize::from(block.span().instructions),
        starts.len(),
        "every instruction in the fixture must be lowered, or the comparison proves nothing"
    );

    native.registers.eip = ENTRY;
    interp.registers.eip = ENTRY;
    arm(&mut native);
    arm(&mut interp);

    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    for _ in 0..starts.len() {
        interp.cycle(&mut interp_bus).unwrap();
    }

    assert_eq!(
        native.registers, interp.registers,
        "architectural registers"
    );
    assert_eq!(
        native.pending_flags, interp.pending_flags,
        "the raw pending-flags descriptor, not just eflags()"
    );
    assert_eq!(native.eflags(), interp.eflags(), "materialized eflags");
}

#[test]
fn hybrid_cld_preserves_gprs_raw_lazy_flags_and_accounting() {
    const ENTRY: u32 = 0x101;
    let code = [
        0x89, 0xc0, 0x89, 0xc9, 0x89, 0xd2, 0x89, 0xdb, 0xfc, 0x89, 0xe4, 0x89, 0xed, 0x89, 0xf6,
        0x89, 0xff,
    ];
    let starts = [
        ENTRY,
        ENTRY + 2,
        ENTRY + 4,
        ENTRY + 6,
        ENTRY + 8,
        ENTRY + 9,
        ENTRY + 11,
        ENTRY + 13,
        ENTRY + 15,
    ];
    let mut memory = vec![0; 0x5000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    let mut native = flat_stack_cpu(ENTRY);
    let mut interp = flat_stack_cpu(ENTRY);
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        bus.uniform_native_fetches = true;
    }
    native.jit_direct.set_direct_helpers_enabled_for_test(true);
    decode_fixture(&mut native, &mut native_bus, &starts);
    decode_fixture(&mut interp, &mut interp_bus, &starts);
    let block = install_fixture_block(&mut native, ENTRY);
    assert!(block.has_helper());
    assert_eq!(block.span().instructions, 9);

    for cpu in [&mut native, &mut interp] {
        cpu.registers.gpr = [
            0x0123_4567,
            0x89ab_cdef,
            0x1020_3040,
            0x5566_7788,
            0x2000,
            0x2110,
            0x2220,
            0x2330,
        ];
        cpu.registers.eflags = 0x2 | FLAG_DF | FLAG_CF | FLAG_ZF | FLAG_OF;
        cpu.jit_set_pending_add(0xffff_ffff, 1);
        cpu.registers.eip = ENTRY;
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 11;
        cpu.core_clocks_so_far = 0;
    }
    let expected_pending = native.pending_flags;

    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    for _ in starts {
        interp.cycle(&mut interp_bus).unwrap();
    }

    assert_eq!(native.registers, interp.registers);
    assert_eq!(native.pending_flags, expected_pending);
    assert_eq!(native.pending_flags, interp.pending_flags);
    assert_eq!(native.eflags(), interp.eflags());
    assert_eq!(native.registers.eflags & FLAG_DF, 0);
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
    assert_eq!(native.timing_rem, interp.timing_rem);
    assert_eq!(
        native_bus.in_batch_scaled_bus_clocks(),
        interp_bus.in_batch_scaled_bus_clocks()
    );
    let counters = native.direct_helper_counters();
    assert_eq!(counters.calls, 1);
    assert_eq!(counters.retired, 1);
    assert_eq!(counters.continue_count, 1);
    assert_eq!(counters.retired_exit, 0);
    assert_eq!(counters.retry_interpret, 0);
    assert_eq!(counters.hard_stop, 0);
    assert_eq!(counters.lost_link_transfers, 1);
    assert_eq!(native.perf.instructions, 9);
    assert_eq!(native.perf.jit_direct_insns, 8);
}

#[test]
fn hybrid_memory_prefix_and_suffix_match_interpreter_accounting() {
    const ENTRY: u32 = 0x101;
    let code = [
        0xa1, 0x00, 0x30, 0x00, 0x00, // mov eax,[0x3000]
        0xa3, 0x04, 0x30, 0x00, 0x00, // mov [0x3004],eax
        0x89, 0xc9, // mov ecx,ecx
        0x89, 0xdb, // mov ebx,ebx
        0xfc, // cld
        0x8b, 0x15, 0x10, 0x30, 0x00, 0x00, // mov edx,[0x3010]
        0xa3, 0x14, 0x30, 0x00, 0x00, // mov [0x3014],eax
        0x89, 0xe4, // mov esp,esp
        0x89, 0xed, // mov ebp,ebp
    ];
    let starts = [
        ENTRY,
        ENTRY + 5,
        ENTRY + 10,
        ENTRY + 12,
        ENTRY + 14,
        ENTRY + 15,
        ENTRY + 21,
        ENTRY + 26,
        ENTRY + 28,
    ];
    let mut memory = vec![0; 0x5000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[0x3000..0x3004].copy_from_slice(&3u32.to_le_bytes());
    memory[0x3010..0x3014].copy_from_slice(&5u32.to_le_bytes());
    let mut native = flat_stack_cpu(ENTRY);
    let mut interp = flat_stack_cpu(ENTRY);
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        bus.uniform_native_fetches = true;
        bus.report_batch_clocks = true;
    }
    native.jit_direct.set_direct_helpers_enabled_for_test(true);
    decode_fixture(&mut native, &mut native_bus, &starts);
    decode_fixture(&mut interp, &mut interp_bus, &starts);
    map_direct_page(
        &mut native,
        &mut native_bus,
        0x3000,
        0x3000,
        jit::fast_map::PagePermissions::UNPAGED,
        true,
        true,
    );
    let block = install_fixture_block(&mut native, ENTRY);
    assert!(block.has_helper());
    assert_eq!(block.span().instructions, 9);

    for cpu in [&mut native, &mut interp] {
        cpu.registers.gpr = [0, 0, 0, 0, 0x2100, 0x2200, 0x2300, 0x2400];
        cpu.registers.eflags = 0x2 | FLAG_DF;
        cpu.pending_flags = PendingFlags::default();
        cpu.registers.eip = ENTRY;
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 7;
        cpu.core_clocks_so_far = 0;
    }

    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    for _ in starts {
        interp.cycle(&mut interp_bus).unwrap();
    }

    assert_eq!(native.registers, interp.registers);
    assert_eq!(native.pending_flags, interp.pending_flags);
    assert_eq!(native.eflags(), interp.eflags());
    assert_eq!(native_bus.memory, interp_bus.memory);
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
    assert_eq!(native.timing_rem, interp.timing_rem);
    assert_eq!(
        native_bus.in_batch_scaled_bus_clocks(),
        interp_bus.in_batch_scaled_bus_clocks()
    );
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert_eq!(native.perf.instructions, 9);
    assert_eq!(native.perf.jit_direct_insns, 8);
    assert_eq!(native.perf.data_direct_reads, 2);
    assert_eq!(native.perf.data_direct_writes, 2);
    assert_eq!(native.direct_helper_counters().continue_count, 1);
}

fn hybrid_inc_fixture() -> (CpuGsw, TestBus, jit::direct::CompiledBlock) {
    const ENTRY: u32 = 0x101;
    let code = [0x40, 0x41, 0x42, 0x43, 0xfc, 0x44, 0x45, 0x46, 0x47];
    let starts: Vec<_> = (0..code.len())
        .map(|offset| ENTRY + offset as u32)
        .collect();
    let mut memory = vec![0; 0x5000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    let mut cpu = flat_stack_cpu(ENTRY);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    bus.uniform_native_fetches = true;
    cpu.jit_direct.set_direct_helpers_enabled_for_test(true);
    decode_fixture(&mut cpu, &mut bus, &starts);
    let block = install_fixture_block(&mut cpu, ENTRY);
    assert!(block.has_helper());
    cpu.registers.gpr = [10, 11, 12, 13, 14, 15, 16, 17];
    cpu.registers.eflags = 0x2 | FLAG_DF;
    cpu.pending_flags = PendingFlags::default();
    cpu.registers.eip = ENTRY;
    (cpu, bus, block)
}

#[test]
fn hybrid_test_forcing_executes_helper_first_middle_and_last() {
    const ENTRY: u32 = 0x101;
    let shapes = [
        [0xfc, 0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47],
        [0x40, 0x41, 0x42, 0x43, 0xfc, 0x44, 0x45, 0x46, 0x47],
        [0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0xfc],
    ];

    for code in shapes {
        let starts: Vec<_> = (0..code.len())
            .map(|offset| ENTRY + offset as u32)
            .collect();
        let mut memory = vec![0; 0x5000];
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
        let mut cpu = flat_stack_cpu(ENTRY);
        let mut bus = TestBus::with_memory(memory);
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        bus.uniform_native_fetches = true;
        cpu.jit_direct.set_direct_helpers_enabled_for_test(true);
        cpu.jit_direct.set_direct_helper_edges_for_test(true);
        decode_fixture(&mut cpu, &mut bus, &starts);
        let block = install_fixture_block(&mut cpu, ENTRY);
        assert!(block.has_helper());
        cpu.registers.gpr = [10, 11, 12, 13, 14, 15, 16, 17];
        cpu.registers.eflags = 0x2 | FLAG_DF;
        cpu.pending_flags = PendingFlags::default();
        cpu.registers.eip = ENTRY;

        assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());

        assert_eq!(cpu.registers.gpr, [11, 12, 13, 14, 15, 16, 17, 18]);
        assert_eq!(cpu.registers.eip, ENTRY + code.len() as u32);
        assert_eq!(cpu.registers.eflags & FLAG_DF, 0);
        assert_eq!(cpu.perf.instructions, 9);
        assert_eq!(cpu.perf.jit_direct_insns, 8);
        let counters = cpu.direct_helper_counters();
        assert_eq!(counters.calls, 1);
        assert_eq!(counters.retired, 1);
        assert_eq!(counters.continue_count, 1);
    }
}

fn assert_hybrid_prefix_only_gprs(cpu: &CpuGsw) {
    assert_eq!(cpu.registers.gpr, [11, 12, 13, 14, 14, 15, 16, 17]);
}

#[test]
fn hybrid_stale_decode_retries_without_retiring_or_running_the_suffix() {
    const ENTRY: u32 = 0x101;
    let (mut cpu, mut bus, block) = hybrid_inc_fixture();
    cpu.jit_direct
        .set_direct_helper_force_for_test(jit::DirectHelperTestForce::StaleDecode);

    assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());

    assert_hybrid_prefix_only_gprs(&cpu);
    assert_eq!(cpu.registers.eip, ENTRY + 4);
    assert_ne!(cpu.registers.eflags & FLAG_DF, 0);
    assert_eq!(cpu.perf.instructions, 4);
    assert_eq!(cpu.perf.jit_direct_insns, 4);
    let counters = cpu.direct_helper_counters();
    assert_eq!(counters.calls, 1);
    assert_eq!(counters.retired, 0);
    assert_eq!(counters.retry_interpret, 1);
    assert_eq!(counters.stale_decode_exits, 1);
    assert_eq!(counters.prefix_only_exits, 1);
}

#[test]
fn hybrid_generation_change_after_retirement_blocks_the_suffix() {
    const ENTRY: u32 = 0x101;
    let (mut cpu, mut bus, block) = hybrid_inc_fixture();
    let generation = cpu.jit_direct.direct_generation();
    cpu.jit_direct
        .set_direct_helper_force_for_test(jit::DirectHelperTestForce::GenerationAfterRetire);

    assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());

    assert_hybrid_prefix_only_gprs(&cpu);
    assert_eq!(cpu.registers.eip, ENTRY + 5);
    assert_eq!(cpu.registers.eflags & FLAG_DF, 0);
    assert!(cpu.jit_direct.direct_generation() > generation);
    assert_eq!(cpu.perf.instructions, 5);
    assert_eq!(cpu.perf.jit_direct_insns, 4);
    let counters = cpu.direct_helper_counters();
    assert_eq!(counters.retired, 1);
    assert_eq!(counters.retired_exit, 1);
    assert_eq!(counters.generation_exits, 1);
    assert_eq!(counters.continue_count, 0);
}

#[test]
fn hybrid_current_block_invalidation_after_retirement_blocks_the_suffix() {
    const ENTRY: u32 = 0x101;
    let (mut cpu, mut bus, block) = hybrid_inc_fixture();
    let id = block.id();
    let generation = cpu.jit_direct.direct_generation();
    cpu.jit_direct
        .set_direct_helper_force_for_test(jit::DirectHelperTestForce::InvalidateCurrentAfterRetire);

    assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());

    assert_hybrid_prefix_only_gprs(&cpu);
    assert_eq!(cpu.registers.eip, ENTRY + 5);
    assert_eq!(cpu.registers.eflags & FLAG_DF, 0);
    assert!(cpu.jit_direct.direct_generation() > generation);
    assert!(cpu.jit_direct.block(id).is_none());
    assert_eq!(cpu.jit_direct.direct_native_frame_depth, 0);
    assert_eq!(cpu.perf.instructions, 5);
    assert_eq!(cpu.perf.jit_direct_insns, 4);
    let counters = cpu.direct_helper_counters();
    assert_eq!(counters.retired, 1);
    assert_eq!(counters.retired_exit, 1);
    assert_eq!(counters.generation_exits, 1);
    assert_eq!(counters.continue_count, 0);
}

#[test]
fn hybrid_state_changes_after_retirement_block_the_suffix() {
    const ENTRY: u32 = 0x101;
    let forces = [
        jit::DirectHelperTestForce::EipAfterRetire,
        jit::DirectHelperTestForce::ModeAfterRetire,
        jit::DirectHelperTestForce::SegmentAfterRetire,
        jit::DirectHelperTestForce::InterruptAfterRetire,
        jit::DirectHelperTestForce::HaltAfterRetire,
        jit::DirectHelperTestForce::RepAfterRetire,
    ];

    for force in forces {
        let (mut cpu, mut bus, block) = hybrid_inc_fixture();
        cpu.jit_direct.set_direct_helper_force_for_test(force);

        assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());

        assert_hybrid_prefix_only_gprs(&cpu);
        if force == jit::DirectHelperTestForce::EipAfterRetire {
            assert_eq!(cpu.registers.eip, ENTRY + 6);
        } else {
            assert_eq!(cpu.registers.eip, ENTRY + 5);
        }
        let counters = cpu.direct_helper_counters();
        assert_eq!(counters.calls, 1, "{force:?}");
        assert_eq!(counters.retired, 1, "{force:?}");
        assert_eq!(counters.retired_exit, 1, "{force:?}");
        assert_eq!(counters.state_change_exits, 1, "{force:?}");
        assert_eq!(counters.continue_count, 0, "{force:?}");
    }
}

#[test]
fn hybrid_step_break_after_helper_retirement_blocks_the_suffix() {
    const ENTRY: u32 = 0x101;
    let (mut cpu, mut bus, block) = hybrid_inc_fixture();
    bus.io_touched = true;

    assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());

    assert_hybrid_prefix_only_gprs(&cpu);
    assert_eq!(cpu.registers.eip, ENTRY + 5);
    let counters = cpu.direct_helper_counters();
    assert_eq!(counters.retired, 1);
    assert_eq!(counters.retired_exit, 1);
    assert_eq!(counters.state_change_exits, 1);
}

#[test]
fn hybrid_clear_after_retirement_is_deferred_until_native_return() {
    const ENTRY: u32 = 0x101;
    let (mut cpu, mut bus, block) = hybrid_inc_fixture();
    let id = block.id();
    cpu.jit_direct
        .set_direct_helper_force_for_test(jit::DirectHelperTestForce::ClearAfterRetire);

    assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());

    assert_hybrid_prefix_only_gprs(&cpu);
    assert_eq!(cpu.registers.eip, ENTRY + 5);
    assert_eq!(cpu.registers.eflags & FLAG_DF, 0);
    assert_eq!(cpu.jit_direct.direct_native_frame_depth, 0);
    assert!(!cpu.jit_direct.direct_reset_pending_for_test());
    assert!(cpu.jit_direct.block(id).is_none());
    let counters = cpu.direct_helper_counters();
    assert_eq!(counters.retired, 1);
    assert_eq!(counters.retired_exit, 1);
    assert_eq!(counters.generation_exits, 1);
}

#[test]
fn hybrid_hard_error_is_relayed_after_native_accounting() {
    const ENTRY: u32 = 0x101;
    let (mut cpu, mut bus, block) = hybrid_inc_fixture();
    cpu.jit_direct
        .set_direct_helper_force_for_test(jit::DirectHelperTestForce::HardError);

    let error = cpu
        .try_run_direct_block_for_test(&mut bus, block)
        .expect_err("forced helper error");

    assert_eq!(error, CpuError::DivideError);
    assert_hybrid_prefix_only_gprs(&cpu);
    assert_eq!(cpu.registers.eip, ENTRY + 4);
    assert_ne!(cpu.registers.eflags & FLAG_DF, 0);
    assert_eq!(cpu.perf.instructions, 4);
    assert_eq!(cpu.perf.jit_direct_insns, 4);
    assert_eq!(cpu.jit_direct.direct_native_frame_depth, 0);
    let counters = cpu.direct_helper_counters();
    assert_eq!(counters.calls, 1);
    assert_eq!(counters.retired, 0);
    assert_eq!(counters.hard_stop, 1);
    assert_eq!(counters.cpu_errors, 1);
}

#[test]
fn hybrid_panic_resumes_only_after_leaving_the_native_frame() {
    const ENTRY: u32 = 0x101;
    let (mut cpu, mut bus, block) = hybrid_inc_fixture();
    cpu.jit_direct
        .set_direct_helper_force_for_test(jit::DirectHelperTestForce::Panic);

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cpu.try_run_direct_block_for_test(&mut bus, block)
    }))
    .expect_err("forced helper panic");

    let message = panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str));
    assert_eq!(message, Some("forced Direct helper panic"));
    assert_hybrid_prefix_only_gprs(&cpu);
    assert_eq!(cpu.registers.eip, ENTRY + 4);
    assert_ne!(cpu.registers.eflags & FLAG_DF, 0);
    assert_eq!(cpu.perf.instructions, 4);
    assert_eq!(cpu.perf.jit_direct_insns, 4);
    assert_eq!(cpu.jit_direct.direct_native_frame_depth, 0);
    assert!(!cpu.jit_direct.direct_reset_pending_for_test());
    let counters = cpu.direct_helper_counters();
    assert_eq!(counters.calls, 1);
    assert_eq!(counters.retired, 0);
    assert_eq!(counters.hard_stop, 1);
    assert_eq!(counters.panics, 1);
}

#[test]
fn hybrid_clear_then_panic_reclaims_only_after_leaving_native_code() {
    const ENTRY: u32 = 0x101;
    let (mut cpu, mut bus, block) = hybrid_inc_fixture();
    let id = block.id();
    cpu.jit_direct
        .set_direct_helper_force_for_test(jit::DirectHelperTestForce::ClearThenPanic);

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cpu.try_run_direct_block_for_test(&mut bus, block)
    }))
    .expect_err("forced helper clear and panic");

    let message = panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str));
    assert_eq!(message, Some("forced Direct helper panic after clear"));
    assert_hybrid_prefix_only_gprs(&cpu);
    assert_eq!(cpu.registers.eip, ENTRY + 4);
    assert_eq!(cpu.jit_direct.direct_native_frame_depth, 0);
    assert!(!cpu.jit_direct.direct_reset_pending_for_test());
    assert!(cpu.jit_direct.block(id).is_none());
    assert_eq!(cpu.perf.instructions, 4);
    assert_eq!(cpu.perf.jit_direct_insns, 4);
    let counters = cpu.direct_helper_counters();
    assert_eq!(counters.calls, 1);
    assert_eq!(counters.retired, 0);
    assert_eq!(counters.hard_stop, 1);
    assert_eq!(counters.panics, 1);
}

#[test]
fn hybrid_rejects_nonuniform_fetches_without_entering_native_code() {
    const ENTRY: u32 = 0x101;
    let (mut cpu, mut bus, block) = hybrid_inc_fixture();
    bus.uniform_native_fetches = false;
    let registers = cpu.registers.clone();
    let pending_flags = cpu.pending_flags;

    assert!(!cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());

    assert_eq!(cpu.registers, registers);
    assert_eq!(cpu.pending_flags, pending_flags);
    assert_eq!(cpu.registers.eip, ENTRY);
    assert_eq!(cpu.perf.instructions, 0);
    let counters = cpu.direct_helper_counters();
    assert_eq!(counters.calls, 0);
    assert_eq!(counters.reject_nonuniform, 1);
}

#[test]
fn hybrid_observer_mode_rejects_before_entering_native_code() {
    let (mut cpu, mut bus, block) = hybrid_inc_fixture();
    cpu.profile.enabled = true;

    assert!(!cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());

    assert_eq!(cpu.perf.instructions, 0);
    assert_eq!(cpu.perf.jit_direct_reject_observer, 1);
    assert_eq!(cpu.direct_helper_counters().calls, 0);
}

#[test]
fn hybrid_event_cap_sweep_never_executes_a_partial_unit() {
    let mut rejected = 0;
    let mut completed = 0;
    for cap in 0..=128 {
        let (mut cpu, mut bus, block) = hybrid_inc_fixture();
        let ran = cpu
            .try_run_direct_block_with_cap_for_test(&mut bus, block, cap)
            .unwrap();
        let counters = cpu.direct_helper_counters();
        if ran {
            completed += 1;
            assert_eq!(cpu.registers.gpr, [11, 12, 13, 14, 15, 16, 17, 18]);
            assert_eq!(cpu.perf.instructions, 9);
            assert_eq!(cpu.perf.jit_direct_insns, 8);
            assert_eq!(counters.calls, 1);
            assert_eq!(counters.retired, 1);
            assert_eq!(counters.continue_count, 1);
        } else {
            rejected += 1;
            assert_eq!(cpu.registers.gpr, [10, 11, 12, 13, 14, 15, 16, 17]);
            assert_eq!(cpu.perf.instructions, 0);
            assert_eq!(counters.calls, 0);
            assert_eq!(counters.full_unit_budget_rejects, 1);
        }
    }
    assert_ne!(rejected, 0);
    assert_ne!(completed, 0);
}

#[test]
fn hybrid_helper_bridge_survives_one_hundred_thousand_real_entries() {
    const ENTRY: u32 = 0x101;
    const CALLS: u64 = 100_000;
    let code = [
        0x89, 0xc0, 0x89, 0xc9, 0x89, 0xd2, 0x89, 0xdb, 0xfc, 0x89, 0xe4, 0x89, 0xed, 0x89, 0xf6,
        0x89, 0xff,
    ];
    let starts = [
        ENTRY,
        ENTRY + 2,
        ENTRY + 4,
        ENTRY + 6,
        ENTRY + 8,
        ENTRY + 9,
        ENTRY + 11,
        ENTRY + 13,
        ENTRY + 15,
    ];
    let sentinels = [
        0x1020_3040,
        0x5060_7080,
        0x90a0_b0c0,
        0xd0e0_f001,
        0x2110,
        0x3120,
        0x4130,
        0x5140,
    ];
    let mut memory = vec![0; 0x5000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    let mut cpu = flat_stack_cpu(ENTRY);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    bus.uniform_native_fetches = true;
    cpu.jit_direct.set_direct_helpers_enabled_for_test(true);
    decode_fixture(&mut cpu, &mut bus, &starts);
    let block = install_fixture_block(&mut cpu, ENTRY);
    assert!(block.has_helper());
    assert_eq!(block.span().instructions, 9);
    let generation = cpu.jit_direct.direct_generation();
    let rsp_before: usize;
    unsafe {
        core::arch::asm!(
            "mov {}, rsp",
            out(reg) rsp_before,
            options(nomem, nostack, preserves_flags)
        );
    }
    cpu.registers.gpr = sentinels;
    cpu.pending_flags = PendingFlags::default();

    for _ in 0..CALLS {
        cpu.registers.eip = ENTRY;
        cpu.registers.eflags = 0x2 | FLAG_DF;
        assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    }

    let rsp_after: usize;
    unsafe {
        core::arch::asm!(
            "mov {}, rsp",
            out(reg) rsp_after,
            options(nomem, nostack, preserves_flags)
        );
    }
    assert_eq!(rsp_after, rsp_before);
    assert_eq!(cpu.registers.gpr, sentinels);
    assert_eq!(cpu.registers.eip, ENTRY + code.len() as u32);
    assert_eq!(cpu.registers.eflags & FLAG_DF, 0);
    assert_eq!(cpu.pending_flags, PendingFlags::default());
    assert_eq!(cpu.jit_direct.direct_generation(), generation);
    assert_eq!(cpu.jit_direct.direct_native_frame_depth, 0);
    assert!(!cpu.jit_direct.direct_reset_pending_for_test());
    assert_eq!(cpu.perf.instructions, CALLS * 9);
    assert_eq!(cpu.perf.jit_direct_insns, CALLS * 8);
    let counters = cpu.direct_helper_counters();
    assert_eq!(counters.calls, CALLS);
    assert_eq!(counters.retired, CALLS);
    assert_eq!(counters.continue_count, CALLS);
    assert_eq!(counters.retired_exit, 0);
    assert_eq!(counters.retry_interpret, 0);
    assert_eq!(counters.hard_stop, 0);
    assert_eq!(counters.panics, 0);
    assert_eq!(counters.lost_link_transfers, CALLS);
}

struct HybridCldBenchFixture {
    cpu: CpuGsw,
    bus: TestBus,
}

impl HybridCldBenchFixture {
    const STARTER: u32 = 0x100;
    const ENTRY: u32 = 0x101;
    const SUFFIX_EIP: u32 = Self::ENTRY + 5;

    fn new(helpers_enabled: bool) -> Self {
        let code = [0x40, 0x41, 0x42, 0x43, 0xfc, 0x44, 0x45, 0x46, 0x47];
        let mut starts: Vec<_> = (0..code.len())
            .map(|offset| Self::ENTRY + offset as u32)
            .collect();
        starts.insert(0, Self::STARTER);
        let mut memory = vec![0; 0x5000];
        memory[Self::STARTER as usize] = 0x90;
        memory[Self::ENTRY as usize..Self::ENTRY as usize + code.len()].copy_from_slice(&code);
        let mut cpu = flat_stack_cpu(Self::STARTER);
        let mut bus = TestBus::with_memory(memory);
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        bus.uniform_native_fetches = true;
        cpu.jit_direct
            .set_direct_helpers_enabled_for_test(helpers_enabled);
        cpu.set_jit_auto_admit(true);
        cpu.jit_direct.set_admission_heat_for_test(1);
        decode_fixture(&mut cpu, &mut bus, &starts);

        let first = install_fixture_block(&mut cpu, Self::ENTRY);
        if helpers_enabled {
            assert!(first.has_helper());
            assert_eq!(first.span().instructions, 9);
        } else {
            assert!(!first.has_helper());
            assert_eq!(first.span().instructions, 4);
            let suffix = install_fixture_block(&mut cpu, Self::SUFFIX_EIP);
            assert!(!suffix.has_helper());
            assert_eq!(suffix.span().instructions, 4);
        }
        Self { cpu, bus }
    }

    #[inline(always)]
    fn arm_occurrence(&mut self) {
        self.cpu.registers.gpr = [10, 11, 12, 13, 14, 15, 16, 17];
        self.cpu.registers.eflags = 0x2 | FLAG_DF;
        self.cpu.pending_flags = PendingFlags::default();
        self.cpu.registers.eip = Self::STARTER;
        self.cpu.elapsed_clocks = 0;
        self.cpu.timing_rem = 0;
        self.cpu.core_clocks_so_far = 0;
    }

    fn run_occurrences(&mut self, occurrences: u32) -> u64 {
        let mut checksum = 0u64;
        for _ in 0..occurrences {
            self.arm_occurrence();
            let outcome = self.cpu.run_budgeted(&mut self.bus, u64::MAX).unwrap();
            debug_assert!(!outcome.halted);
            checksum = checksum.wrapping_add(u64::from(self.cpu.registers.eax()));
        }
        checksum
    }

    fn assert_final_state(&mut self) {
        assert_eq!(self.cpu.registers.gpr, [11, 12, 13, 14, 15, 16, 17, 18]);
        assert_eq!(self.cpu.registers.eip, Self::ENTRY + 9);
        assert_eq!(self.cpu.registers.eflags & FLAG_DF, 0);
    }
}

fn measure_hybrid_cld_batch(fixture: &mut HybridCldBenchFixture, occurrences: u32) -> u128 {
    let instructions_before = fixture.cpu.perf.instructions;
    let started = std::time::Instant::now();
    let checksum = fixture.run_occurrences(occurrences);
    let elapsed = started.elapsed().as_nanos();
    std::hint::black_box(checksum);
    assert_eq!(
        fixture.cpu.perf.instructions - instructions_before,
        u64::from(occurrences) * 10
    );
    fixture.assert_final_state();
    elapsed
}

fn median_ns_per_occurrence(mut samples: Vec<u128>, occurrences: u32) -> f64 {
    samples.sort_unstable();
    let middle = samples.len() / 2;
    let median_ns = if samples.len().is_multiple_of(2) {
        (samples[middle - 1] + samples[middle]) as f64 / 2.0
    } else {
        samples[middle] as f64
    };
    median_ns / f64::from(occurrences)
}

#[test]
#[ignore = "release-only Direct helper performance gate"]
fn hybrid_cld_release_microbenchmark_beats_dispatcher_reentry() {
    const WARMUP_OCCURRENCES: u32 = 10_000;
    const MEASURED_OCCURRENCES: u32 = 50_000;
    const PAIRS: usize = 8;

    assert!(
        !std::hint::black_box(cfg!(debug_assertions)),
        "run this ignored performance gate with cargo test --release"
    );
    let mut dispatcher = HybridCldBenchFixture::new(false);
    let mut hybrid = HybridCldBenchFixture::new(true);

    let _ = dispatcher.run_occurrences(WARMUP_OCCURRENCES);
    dispatcher.assert_final_state();
    let _ = hybrid.run_occurrences(WARMUP_OCCURRENCES);
    hybrid.assert_final_state();

    let mut dispatcher_samples = Vec::with_capacity(PAIRS);
    let mut hybrid_samples = Vec::with_capacity(PAIRS);
    for pair in 0..PAIRS {
        if pair % 2 == 0 {
            dispatcher_samples.push(measure_hybrid_cld_batch(
                &mut dispatcher,
                MEASURED_OCCURRENCES,
            ));
            hybrid_samples.push(measure_hybrid_cld_batch(&mut hybrid, MEASURED_OCCURRENCES));
        } else {
            hybrid_samples.push(measure_hybrid_cld_batch(&mut hybrid, MEASURED_OCCURRENCES));
            dispatcher_samples.push(measure_hybrid_cld_batch(
                &mut dispatcher,
                MEASURED_OCCURRENCES,
            ));
        }
    }

    let dispatcher_ns = median_ns_per_occurrence(dispatcher_samples, MEASURED_OCCURRENCES);
    let hybrid_ns = median_ns_per_occurrence(hybrid_samples, MEASURED_OCCURRENCES);
    let speedup = dispatcher_ns / hybrid_ns;
    let saved_ns = dispatcher_ns - hybrid_ns;
    eprintln!(
        "hybrid Direct CLD: dispatcher={dispatcher_ns:.2} ns/occurrence, \
         hybrid={hybrid_ns:.2} ns/occurrence, speedup={speedup:.3}x, saved={saved_ns:.2} ns"
    );

    assert!(
        speedup >= 1.20,
        "hybrid speedup {speedup:.3}x is below 1.20x"
    );
    assert!(
        saved_ns >= 25.0,
        "hybrid saved {saved_ns:.2} ns per occurrence, below 25 ns"
    );
    assert_eq!(
        hybrid.cpu.direct_helper_counters().continue_count,
        u64::from(WARMUP_OCCURRENCES) + u64::from(MEASURED_OCCURRENCES) * PAIRS as u64
    );
    assert_eq!(dispatcher.cpu.direct_helper_counters().calls, 0);
    assert_eq!(hybrid.cpu.registers, dispatcher.cpu.registers);
    assert_eq!(hybrid.cpu.pending_flags, dispatcher.cpu.pending_flags);
}

#[test]
fn hybrid_helper_preserves_cf_override_for_native_adc_and_jcc() {
    const ENTRY: u32 = 0x101;
    let code = [
        0x89, 0xc9, // mov ecx,ecx
        0x89, 0xd2, // mov edx,edx
        0x89, 0xdb, // mov ebx,ebx
        0x89, 0xf6, // mov esi,esi
        0xfc, // cld, precise helper
        0x15, 0x00, 0x00, 0x00, 0x00, // adc eax,0
        0x89, 0xff, // mov edi,edi, preserving ADC flags
        0x72, 0x02, // jc +2
    ];
    let starts = [
        ENTRY,
        ENTRY + 2,
        ENTRY + 4,
        ENTRY + 6,
        ENTRY + 8,
        ENTRY + 9,
        ENTRY + 14,
        ENTRY + 16,
    ];
    let mut memory = vec![0; 0x5000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    let mut native = flat_stack_cpu(ENTRY);
    let mut interp = flat_stack_cpu(ENTRY);
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        bus.uniform_native_fetches = true;
    }
    native.jit_direct.set_direct_helpers_enabled_for_test(true);
    decode_fixture(&mut native, &mut native_bus, &starts);
    decode_fixture(&mut interp, &mut interp_bus, &starts);
    let block = install_fixture_block(&mut native, ENTRY);
    assert!(block.has_helper());
    assert_eq!(block.span().instructions, 8);

    for cpu in [&mut native, &mut interp] {
        cpu.registers.gpr = [
            0xffff_ffff,
            0x1122_3344,
            0x5566_7788,
            0x99aa_bbcc,
            0x2110,
            0x3120,
            0x4130,
            0x5140,
        ];
        cpu.registers.eflags = 0x2 | FLAG_DF | FLAG_ZF | FLAG_OF;
        cpu.jit_set_pending_add(1, 1);
        cpu.set_flag(FLAG_CF, true);
        assert_eq!(cpu.pending_flags.cf_override(), Some(true));
        cpu.registers.eip = ENTRY;
    }
    let incoming_pending = native.pending_flags;

    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    for _ in starts {
        interp.cycle(&mut interp_bus).unwrap();
    }

    assert_ne!(incoming_pending, PendingFlags::default());
    assert_eq!(native.registers, interp.registers);
    assert_eq!(native.pending_flags, interp.pending_flags);
    assert_eq!(native.eflags(), interp.eflags());
    assert_eq!(native.registers.eax(), 0);
    assert_eq!(native.registers.eip, ENTRY + 20);
    assert_ne!(native.registers.eflags & FLAG_CF, 0);
    assert_eq!(native.registers.eflags & FLAG_DF, 0);
    let counters = native.direct_helper_counters();
    assert_eq!(counters.calls, 1);
    assert_eq!(counters.retired, 1);
    assert_eq!(counters.continue_count, 1);
    assert_eq!(counters.hard_stop, 0);
}

/// BT with NO live descriptor: `set_flag` falls through to `set_flag_live` and CF goes straight
/// into eflags.
#[test]
fn bt_register_form_matches_interpreter_without_a_live_descriptor() {
    for (bit, name) in [(3u8, "set"), (2u8, "clear"), (31u8, "top"), (0u8, "zero")] {
        // mov eax,0x8000_0008 ; mov ecx,bit ; bt eax,ecx ; hlt
        let code = [
            0xb8, 0x08, 0x00, 0x00, 0x80, // mov eax,0x80000008
            0xb9, bit, 0x00, 0x00, 0x00, // mov ecx,bit
            0x0f, 0xa3, 0xc8, // bt eax,ecx
        ];
        assert_native_matches_interpreter(&code, &[0x101, 0x106, 0x10b], |_| {});
        let _ = name;
    }
}

/// BT with a LIVE descriptor, which is the case that distinguishes a correct CF-only publish from
/// a bare RBP write or from eager materialization. The ADD arms a descriptor; the BT must patch
/// its CF override in place and leave `a`, `b` and `result` untouched.
#[test]
fn bt_register_form_matches_interpreter_with_a_live_descriptor() {
    // mov eax,0x8000_0008 ; mov edx,1 ; add edx,edx ; mov ecx,3 ; bt eax,ecx
    let code = [
        0xb8, 0x08, 0x00, 0x00, 0x80, // mov eax,0x80000008
        0xba, 0x01, 0x00, 0x00, 0x00, // mov edx,1
        0x01, 0xd2, // add edx,edx  (arms the descriptor)
        0xb9, 0x03, 0x00, 0x00, 0x00, // mov ecx,3
        0x0f, 0xa3, 0xc8, // bt eax,ecx
    ];
    assert_native_matches_interpreter(&code, &[0x101, 0x106, 0x10b, 0x10d, 0x112], |_| {});
}

/// The index mask. A missing `& 31` reads a bit that does not exist; the host takes the offset
/// modulo 32 for a register operand, which is what makes the guest's mask free.
#[test]
fn bt_register_form_masks_the_index_to_five_bits() {
    // mov eax,2 ; mov ecx,33 ; bt eax,ecx  -> bit 1 of eax, which is set, so CF = 1
    let code = [
        0xb8, 0x02, 0x00, 0x00, 0x00, // mov eax,2
        0xb9, 0x21, 0x00, 0x00, 0x00, // mov ecx,33
        0x0f, 0xa3, 0xc8, // bt eax,ecx
    ];
    assert_native_matches_interpreter(&code, &[0x101, 0x106, 0x10b], |_| {});
}

/// Byte INC/DEC on the LOW lanes and on the HIGH lanes. AH..BH are the high bytes of eAX..eBX, so
/// a lowering that treated the index as a home index would reach guest EBP/ESI/EDI instead.
#[test]
fn byte_inc_dec_matches_interpreter_on_every_lane() {
    for (modrm, label) in [
        (0xc0u8, "inc al"),
        (0xc4u8, "inc ah"),
        (0xc3u8, "inc bl"),
        (0xc7u8, "inc bh"),
        (0xc8u8, "dec al"),
        (0xccu8, "dec ah"),
    ] {
        // mov eax,0x11223344 ; mov ebx,0x55667788 ; fe /r
        let code = [
            0xb8, 0x44, 0x33, 0x22, 0x11, // mov eax,0x11223344
            0xbb, 0x88, 0x77, 0x66, 0x55, // mov ebx,0x55667788
            0xfe, modrm,
        ];
        assert_native_matches_interpreter(&code, &[0x101, 0x106, 0x10b], |_| {});
        let _ = label;
    }
}

/// The overflow edge, and the CF-preserved rule. 0x7f + 1 sets OF and SF; CF must come through
/// the descriptor unchanged from the ADD that armed it.
#[test]
fn byte_inc_preserves_carry_across_the_overflow_edge() {
    // mov eax,0x7f ; stc-equivalent via add ; inc al
    let code = [
        0xb8, 0x7f, 0x00, 0x00, 0x00, // mov eax,0x7f
        0xba, 0xff, 0xff, 0xff, 0xff, // mov edx,0xffffffff
        0x01, 0xd2, // add edx,edx  (sets CF, arms the descriptor)
        0xfe, 0xc0, // inc al
    ];
    assert_native_matches_interpreter(&code, &[0x101, 0x106, 0x10b, 0x10d], |_| {});
}

/// Clock pins for both new kinds.
///
/// The differential fixtures above compare architectural state, and core clocks are not part of
/// it, so nothing there catches a wrong charge. A mutation battery confirmed that dropping BT's
/// `raw_clocks` arm passes every other test in the suite while undercharging by 4 per BT, which
/// is the same gap the LEAVE slice found a day earlier. This is the test for it.
#[test]
fn bt_and_byte_inc_dec_charge_the_interpreter_clocks() {
    const ENTRY: u32 = 0x101;

    // mov eax,imm32 (2) ; mov ecx,imm32 (2) ; bt eax,ecx (6) = 10.
    let mut pristine = vec![0; 0x5000];
    let bt = [
        0xb8, 0x08, 0x00, 0x00, 0x80, 0xb9, 0x03, 0x00, 0x00, 0x00, 0x0f, 0xa3, 0xc8,
    ];
    pristine[ENTRY as usize..ENTRY as usize + bt.len()].copy_from_slice(&bt);
    let mut cpu = flat_stack_cpu(ENTRY);
    let mut bus = TestBus::with_memory(pristine.clone());
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    decode_fixture(&mut cpu, &mut bus, &[0x101, 0x106, 0x10b]);
    let block = install_fixture_block(&mut cpu, ENTRY);
    assert_eq!(block.span().instructions, 3);
    assert_eq!(
        block.raw_clocks(),
        10,
        "two 2-clock moves plus a 6-clock BT; the `_ => 2` default undercharges BT by 4"
    );

    // mov eax,imm32 (2) ; mov ebx,imm32 (2) ; inc al (2) = 6. The byte form deliberately has NO
    // `raw_clocks` arm: the interpreter charges 2 for 0xFE, identical to 0xFF /0 and 0x40..0x4f,
    // so the default is the right answer. This pins that, so a well-meaning future arm is caught.
    let mut pristine = vec![0; 0x5000];
    let inc = [
        0xb8, 0x44, 0x33, 0x22, 0x11, 0xbb, 0x88, 0x77, 0x66, 0x55, 0xfe, 0xc4,
    ];
    pristine[ENTRY as usize..ENTRY as usize + inc.len()].copy_from_slice(&inc);
    let mut cpu = flat_stack_cpu(ENTRY);
    let mut bus = TestBus::with_memory(pristine);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    decode_fixture(&mut cpu, &mut bus, &[0x101, 0x106, 0x10b]);
    let block = install_fixture_block(&mut cpu, ENTRY);
    assert_eq!(block.span().instructions, 3);
    assert_eq!(
        block.raw_clocks(),
        6,
        "0xFE INC/DEC r8 charges 2, like 0xFF"
    );
}
