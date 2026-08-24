// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const GAME_LOOP_ENTRY: u32 = 0x101;

fn invoke_native_entry(
    cpu: &mut CpuGsw,
    block: jit::direct::CompiledBlock,
    quota: u32,
) -> jit::direct::NativeExit {
    // This helper enters native code WITHOUT going through `run_direct_block`, so it never
    // publishes `CpuGsw::native_callout`. A block carrying an interpreter call-out slot would
    // therefore load a null helper pointer and `call` address zero. Refuse such a block here
    // rather than leave the trap latent: any fixture that wants a call-out must drive it through
    // `try_run_direct_block_for_test`, which does publish (see cpu_jit_callout_test.rs).
    debug_assert_eq!(
        block.callout_slots(),
        0,
        "invoke_native_entry does not publish a call-out table; use try_run_direct_block_for_test"
    );
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

        cpu.mark_decode_code_for_test(marked, 1);
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
    // A headroom canary under the real one-host-page install cap (4096), not a page-size claim.
    // History: the per-granule watch probe once pushed this loop to 4039 bytes and the ceiling
    // was 4000 after the window-probe fix. The watched-page bit (D3) consciously spends ~20
    // bytes per store site on its re-read + skip test — measured 4007 on this loop — buying a
    // 16-instruction hot-path saving per unwatched store; the design's carry-by-duplication
    // first draft cost 4081 here and was rejected for exactly this canary. If this ceiling is
    // hit again, check installs-refused-for-size before widening: growth that spends the LAST
    // ~50 bytes of headroom starts refusing real store-dense blocks.
    assert!(
        root.code.len() <= 4_050,
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
    assert!(cpu.jit_direct.has_linked_successor(source.id()));

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
    assert_eq!(&native_bus.memory[..], &pristine[..]);
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
    // Mark TARGET watched BEFORE populating its fast-map entry: the mark's E1 sweep clears any
    // entry whose PAGE_WATCHED bit is clear, so populating first and marking after would
    // invalidate the very entry the watched-store guard needs (populate-then-mark trap).
    native.mark_decode_code_for_test(TARGET, width);
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

/// The class counters close on `jit_direct_unresolved_static_unbound` on the REAL exit path, not
/// just as a match arm. The two unit pins in `jit/direct_test.rs` check the classifier and the
/// unlink sites in isolation, and both of them still pass if `classify_unbound_exit` goes back to
/// dropping the `key_for` refusal on the floor -- which is precisely the hole that made the class
/// totals fall short of the counter they are meant to attribute. This drives the dispatcher.
///
/// Both exit sites are covered: an ordinary uncompiled successor (`absent`) and a successor whose
/// address `key_for` refuses outright (`no_key`, the BIOS window at `0xff000`).
#[test]
fn unbound_exit_classes_sum_to_the_static_unbound_counter() {
    const ENTRY: u32 = 0x300;
    const COLD_TARGET: u32 = 0x400;
    // Inside the 0xff000..0xff400 window `key_for` refuses without probing the entry map.
    const UNKEYABLE_TARGET: u32 = 0xff_100;
    const UNKEYABLE_ENTRY: u32 = 0x500;

    let mut memory = vec![0; 0x0010_0000];
    memory[ENTRY as usize..ENTRY as usize + 10].copy_from_slice(&[
        0xb8, 1, 0, 0, 0, // mov eax,1
        0xe9, 0xf6, 0, 0, 0, // jmp COLD_TARGET
    ]);
    memory[COLD_TARGET as usize..COLD_TARGET as usize + 2].copy_from_slice(&[
        0xeb, 0xfe, // jmp COLD_TARGET
    ]);
    memory[UNKEYABLE_ENTRY as usize..UNKEYABLE_ENTRY as usize + 10].copy_from_slice(&[
        0xb8, 2, 0, 0, 0, // mov eax,2
        0xe9, 0xf6, 0xeb, 0x0f, 0x00, // jmp UNKEYABLE_TARGET
    ]);
    memory[UNKEYABLE_TARGET as usize..UNKEYABLE_TARGET as usize + 2].copy_from_slice(&[
        0xeb, 0xfe, // jmp UNKEYABLE_TARGET
    ]);

    let mut cpu = flat_stack_cpu(ENTRY);
    cpu.jit_direct.enable_barrier_census_for_test();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    decode_fixture(
        &mut cpu,
        &mut bus,
        &[
            ENTRY,
            ENTRY + 5,
            COLD_TARGET,
            UNKEYABLE_ENTRY,
            UNKEYABLE_ENTRY + 5,
        ],
    );
    let cold_source = install_fixture_block(&mut cpu, ENTRY);
    let unkeyable_source = install_fixture_block(&mut cpu, UNKEYABLE_ENTRY);

    let before = cpu.perf_counters().clone();
    for _ in 0..3 {
        arm_stack_fixture(&mut cpu, ENTRY, 0x800);
        assert!(
            cpu.try_run_direct_block_for_test(&mut bus, cold_source)
                .unwrap()
        );
        assert_eq!(cpu.registers.eip, COLD_TARGET);
    }
    for _ in 0..2 {
        arm_stack_fixture(&mut cpu, UNKEYABLE_ENTRY, 0x800);
        assert!(
            cpu.try_run_direct_block_for_test(&mut bus, unkeyable_source)
                .unwrap()
        );
        assert_eq!(cpu.registers.eip, UNKEYABLE_TARGET);
    }

    let exits = cpu.perf_counters().jit_direct_unresolved_static_unbound
        - before.jit_direct_unresolved_static_unbound;
    assert_eq!(exits, 5, "every entry must end on a static-unbound exit");

    let snapshot = cpu
        .direct_barrier_census_snapshot()
        .expect("census must be allocated for this test");
    let classes: Vec<(&str, u64)> = snapshot.unbound_targets.clone();
    assert_eq!(
        classes.iter().map(|(_, n)| n).sum::<u64>(),
        exits,
        "class totals must close on the exit counter exactly, got {classes:?}"
    );
    let of = |label: &str| {
        classes
            .iter()
            .find(|(l, _)| *l == label)
            .unwrap_or_else(|| panic!("missing class {label}"))
            .1
    };
    assert_eq!(of("absent"), 3, "the cold successor was never probed");
    assert_eq!(
        of("no_key"),
        2,
        "a successor `key_for` refuses must be classified, not dropped"
    );
}

#[cfg(feature = "direct-link-refusal-census")]
#[test]
fn link_refusal_census_runtime_id_is_written_only_for_static_unbound() {
    const ENTRY: u32 = 0x300;
    const TARGET: u32 = 0x400;
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize..ENTRY as usize + 10]
        .copy_from_slice(&[0xb8, 1, 0, 0, 0, 0xe9, 0xf6, 0, 0, 0]);
    memory[TARGET as usize..TARGET as usize + 2].copy_from_slice(&[0xeb, 0xfe]);
    let mut cpu = flat_stack_cpu(ENTRY);
    cpu.enable_direct_link_refusal_census(true);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    decode_fixture(&mut cpu, &mut bus, &[ENTRY, ENTRY + 5, TARGET]);
    let source = install_fixture_block(&mut cpu, ENTRY);

    arm_stack_fixture(&mut cpu, ENTRY, 0x800);
    let unbound = invoke_native_entry(&mut cpu, source, 4);
    assert_eq!(
        unbound.unresolved_reason,
        jit::direct::UnresolvedReason::StaticUnbound
    );
    assert_eq!(unbound.direct_link_refusal_census_id, 1);

    let target = install_fixture_block(&mut cpu, TARGET);
    assert!(cpu.jit_direct.hide_portal_for_test(target.id()));
    arm_stack_fixture(&mut cpu, ENTRY, 0x800);
    let hidden = invoke_native_entry(&mut cpu, source, 4);
    assert_eq!(
        hidden.unresolved_reason,
        jit::direct::UnresolvedReason::StaticHidden
    );
    assert_eq!(hidden.direct_link_refusal_census_id, 0);

    let target_key = jit::direct::key_for(&cpu, TARGET, true).unwrap();
    cpu.jit_direct
        .revalidate_translation(target_key)
        .expect("target revalidation");
    arm_stack_fixture(&mut cpu, ENTRY, 0x800);
    let linked = invoke_native_entry(&mut cpu, source, 1);
    assert_eq!(
        linked.unresolved_reason,
        jit::direct::UnresolvedReason::None
    );
    assert_eq!(linked.direct_link_refusal_census_id, 0);

    const RET: u32 = 0x500;
    cpu.jit_direct.clear();
    cpu.enable_direct_link_refusal_census(true);
    bus.memory[RET as usize] = 0xc3;
    decode_fixture(&mut cpu, &mut bus, &[RET]);
    map_direct_page(
        &mut cpu,
        &mut bus,
        0x800,
        0x800,
        jit::fast_map::PagePermissions::UNPAGED,
        true,
        false,
    );
    let ret = install_fixture_block(&mut cpu, RET);
    arm_stack_fixture(&mut cpu, RET, 0x800);
    bus.memory[0x800..0x804].copy_from_slice(&TARGET.to_le_bytes());
    let dynamic = invoke_native_entry(&mut cpu, ret, 4);
    assert_eq!(
        dynamic.unresolved_reason,
        jit::direct::UnresolvedReason::DynamicMissOrUnbound
    );
    assert_eq!(dynamic.direct_link_refusal_census_id, 0);

    let target = install_fixture_block(&mut cpu, TARGET);
    arm_stack_fixture(&mut cpu, RET, 0x800);
    bus.memory[0x800..0x804].copy_from_slice(&TARGET.to_le_bytes());
    assert!(cpu.try_run_direct_block_for_test(&mut bus, ret).unwrap());
    assert!(cpu.jit_direct.hide_portal_for_test(target.id()));
    arm_stack_fixture(&mut cpu, RET, 0x800);
    bus.memory[0x800..0x804].copy_from_slice(&TARGET.to_le_bytes());
    let dynamic_hidden = invoke_native_entry(&mut cpu, ret, 4);
    assert_eq!(
        dynamic_hidden.unresolved_reason,
        jit::direct::UnresolvedReason::DynamicHidden
    );
    assert_eq!(dynamic_hidden.direct_link_refusal_census_id, 0);
}

#[cfg(feature = "direct-link-refusal-census")]
#[test]
fn link_refusal_census_routes_both_segment_write_jcc_arms() {
    const ENTRY: u32 = 0x300;
    const FALLTHROUGH: u32 = 0x307;
    const TAKEN: u32 = 0x310;
    let mut memory = vec![0; 0x1000];
    memory[ENTRY as usize..FALLTHROUGH as usize].copy_from_slice(&[
        0x66, 0x8e, 0xd8, // mov ds,ax
        0x39, 0xd1, // cmp ecx,edx
        0x75, 0x09, // jne TAKEN
    ]);
    let mut cpu = fresh();
    make_data_segments_flat(&mut cpu);
    let mut cs = cpu.registers.cs();
    cs.default_size_32 = true;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    cpu.set_eip(ENTRY);
    cpu.enable_direct_link_refusal_census(true);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    decode_fixture(&mut cpu, &mut bus, &[ENTRY, ENTRY + 3, ENTRY + 5]);
    let compilation = jit::direct::compile(&mut cpu, ENTRY, true).expect("segment Jcc compile");
    assert_eq!(compilation.successors, [None, None]);
    assert_eq!(
        compilation.emitted_static_targets,
        [
            Some(jit::links::LinkTarget {
                linear: FALLTHROUGH,
                mode_key: compilation.span.key.mode_key,
            }),
            Some(jit::links::LinkTarget {
                linear: TAKEN,
                mode_key: compilation.span.key.mode_key,
            }),
        ]
    );
    let key = compilation.span.key;
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("segment Jcc install");
    let block = cpu.jit_direct.block(id).expect("installed segment Jcc");
    assert!(block.is_segment_write_block());

    let snapshot = cpu
        .direct_link_refusal_census_snapshot()
        .expect("armed census");
    assert_eq!(snapshot.rows.len(), 2);
    assert_eq!(snapshot.rows[0].slot, 0);
    assert_eq!(snapshot.rows[0].target_linear, FALLTHROUGH);
    assert_eq!(snapshot.rows[0].state, "suppressed");
    assert_eq!(snapshot.rows[1].slot, 1);
    assert_eq!(snapshot.rows[1].target_linear, TAKEN);
    assert_eq!(snapshot.rows[1].state, "suppressed");

    arm_stack_fixture(&mut cpu, ENTRY, 0x800);
    cpu.registers.set_eax(0x10);
    cpu.registers.set_ecx(7);
    cpu.registers.set_edx(7);
    let fallthrough = invoke_native_entry(&mut cpu, block, 1);
    assert_eq!(
        fallthrough.unresolved_reason,
        jit::direct::UnresolvedReason::StaticUnbound
    );
    assert_eq!(
        fallthrough.direct_link_refusal_census_id,
        snapshot.rows[0].id
    );

    make_data_segments_flat(&mut cpu);
    arm_stack_fixture(&mut cpu, ENTRY, 0x800);
    cpu.registers.set_eax(0x10);
    cpu.registers.set_ecx(8);
    cpu.registers.set_edx(7);
    let taken = invoke_native_entry(&mut cpu, block, 1);
    assert_eq!(
        taken.unresolved_reason,
        jit::direct::UnresolvedReason::StaticUnbound
    );
    assert_eq!(taken.direct_link_refusal_census_id, snapshot.rows[1].id);
}

#[cfg(feature = "direct-link-refusal-census")]
#[test]
fn link_refusal_census_uses_segment_write_control_targets() {
    for (entry, target, opcode, label) in [
        (0x400u32, 0x450u32, 0xe9u8, "Jmp"),
        (0x500u32, 0x550u32, 0xe8u8, "Call"),
    ] {
        let mut memory = vec![0; 0x1000];
        memory[entry as usize..entry as usize + 8]
            .copy_from_slice(&[0x66, 0x8e, 0xd8, opcode, 0x48, 0x00, 0x00, 0x00]);
        let mut cpu = fresh();
        make_data_segments_flat(&mut cpu);
        let mut cs = cpu.registers.cs();
        cs.default_size_32 = true;
        cpu.registers.set_segment(SegmentIndex::Cs, cs);
        let mut ss = cpu.registers.segment(SegmentIndex::Ss);
        ss.default_size_32 = true;
        cpu.registers.set_segment(SegmentIndex::Ss, ss);
        cpu.set_eip(entry);
        cpu.enable_direct_link_refusal_census(true);
        let mut bus = TestBus::with_memory(memory);
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        decode_fixture(&mut cpu, &mut bus, &[entry, entry + 3]);
        map_direct_page(
            &mut cpu,
            &mut bus,
            0x800,
            0x800,
            jit::fast_map::PagePermissions::UNPAGED,
            false,
            true,
        );

        let compilation = jit::direct::compile(&mut cpu, entry, true)
            .unwrap_or_else(|| panic!("segment {label} compile"));
        assert_eq!(compilation.successors, [None, None], "{label}");
        let emitted = compilation.emitted_static_targets[0].expect("static cell target");
        assert_eq!(emitted.linear, target, "{label}");
        assert_ne!(
            emitted.linear,
            entry + 8,
            "{label} must not use fallthrough"
        );
        assert_eq!(compilation.emitted_static_targets[1], None, "{label}");
        let key = compilation.span.key;
        assert!(matches!(
            cpu.jit_direct.probe(key),
            jit::direct::BlockProbe::Interpret
        ));
        cpu.jit_direct
            .install(&compilation)
            .unwrap_or_else(|| panic!("segment {label} install"));
        let snapshot = cpu
            .direct_link_refusal_census_snapshot()
            .expect("armed census");
        assert_eq!(snapshot.rows.len(), 1, "{label}");
        assert_eq!(snapshot.rows[0].slot, 0, "{label}");
        assert_eq!(snapshot.rows[0].target_linear, target, "{label}");
        assert_eq!(snapshot.rows[0].state, "suppressed", "{label}");
    }
}

#[cfg(feature = "direct-link-refusal-census")]
#[test]
fn link_refusal_census_omits_dynamic_and_self_loop_cells() {
    const RET: u32 = 0x600;
    let mut ret_cpu = flat_stack_cpu(RET);
    ret_cpu.enable_direct_link_refusal_census(true);
    let mut ret_bus = TestBus::with_memory(vec![0; 0x1000]);
    ret_bus.memory[RET as usize] = 0xc3;
    ret_bus.direct_pages_enabled = true;
    ret_bus.direct_page_clocks = true;
    decode_fixture(&mut ret_cpu, &mut ret_bus, &[RET]);
    map_direct_page(
        &mut ret_cpu,
        &mut ret_bus,
        0x800,
        0x800,
        jit::fast_map::PagePermissions::UNPAGED,
        true,
        false,
    );
    let ret = jit::direct::compile(&mut ret_cpu, RET, true).expect("Ret compile");
    assert!(ret.dynamic_successor);
    assert_eq!(ret.emitted_static_targets, [None, None]);
    let ret_key = ret.span.key;
    assert!(matches!(
        ret_cpu.jit_direct.probe(ret_key),
        jit::direct::BlockProbe::Interpret
    ));
    ret_cpu.jit_direct.install(&ret).expect("Ret install");
    assert!(
        ret_cpu
            .direct_link_refusal_census_snapshot()
            .expect("armed Ret census")
            .rows
            .is_empty()
    );

    const LOOP: u32 = 0x700;
    let mut loop_cpu = flat_stack_cpu(LOOP);
    loop_cpu.enable_direct_link_refusal_census(true);
    let mut loop_bus = TestBus::with_memory(vec![0; 0x1000]);
    loop_bus.memory[LOOP as usize..LOOP as usize + 2].copy_from_slice(&[0x75, 0xfe]);
    loop_bus.direct_pages_enabled = true;
    loop_bus.direct_page_clocks = true;
    decode_fixture(&mut loop_cpu, &mut loop_bus, &[LOOP]);
    let loop_block = jit::direct::compile(&mut loop_cpu, LOOP, true).expect("self-loop compile");
    assert!(loop_block.self_loop);
    assert_eq!(
        loop_block.successors[0].map(|target| target.linear),
        Some(LOOP + 2)
    );
    assert_eq!(loop_block.successors[1], None);
    assert_eq!(loop_block.emitted_static_targets, [None, None]);
    let loop_key = loop_block.span.key;
    assert!(matches!(
        loop_cpu.jit_direct.probe(loop_key),
        jit::direct::BlockProbe::Interpret
    ));
    loop_cpu
        .jit_direct
        .install(&loop_block)
        .expect("self-loop install");
    assert!(
        loop_cpu
            .direct_link_refusal_census_snapshot()
            .expect("armed self-loop census")
            .rows
            .is_empty()
    );
}

#[cfg(feature = "direct-link-refusal-census")]
#[test]
fn link_refusal_census_registers_ordinary_fallthrough() {
    const ENTRY: u32 = 0x800;
    let mut cpu = flat_stack_cpu(ENTRY);
    cpu.enable_direct_link_refusal_census(true);
    let mut bus = TestBus::with_memory(vec![0; 0x1000]);
    bus.memory[ENTRY as usize..ENTRY as usize + 3].copy_from_slice(&[0x90; 3]);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    decode_fixture(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);
    let compilation =
        jit::direct::compile_with_instruction_limit_for_test(&mut cpu, ENTRY, true, 3)
            .expect("NOP compile");
    assert_eq!(
        compilation.emitted_static_targets[0].map(|target| target.linear),
        Some(ENTRY + 3)
    );
    assert_eq!(compilation.emitted_static_targets[1], None);
    let key = compilation.span.key;
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    cpu.jit_direct.install(&compilation).expect("NOP install");
    let snapshot = cpu
        .direct_link_refusal_census_snapshot()
        .expect("armed census");
    assert_eq!(snapshot.rows.len(), 1);
    assert_eq!(snapshot.rows[0].slot, 0);
    assert_eq!(snapshot.rows[0].target_linear, ENTRY + 3);
    assert_eq!(snapshot.rows[0].state, "not_attempted");
}

/// The dynamic-miss lane closes on its own counter the same way. Same classifier, separate
/// census array, and the same `key_for` hole -- so it needs its own witness rather than an
/// argument by symmetry.
#[test]
fn dynamic_miss_classes_sum_to_the_dynamic_miss_counter() {
    const ENTRY: u32 = 0x300;
    const COLD_TARGET: u32 = 0x400;
    const UNKEYABLE_TARGET: u32 = 0xff_100;
    const INITIAL_ESP: u32 = 0x3000;

    let mut memory = vec![0; 0x0010_0000];
    memory[ENTRY as usize] = 0xc3; // ret
    memory[COLD_TARGET as usize..COLD_TARGET as usize + 2].copy_from_slice(&[0xeb, 0xfe]);

    let mut cpu = flat_stack_cpu(ENTRY);
    cpu.jit_direct.enable_barrier_census_for_test();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    decode_fixture(&mut cpu, &mut bus, &[ENTRY, COLD_TARGET]);
    map_direct_page(
        &mut cpu,
        &mut bus,
        INITIAL_ESP,
        INITIAL_ESP,
        jit::fast_map::PagePermissions::UNPAGED,
        true,
        false,
    );
    let source = install_fixture_block(&mut cpu, ENTRY);

    let before = cpu.perf_counters().clone();
    let targets = [COLD_TARGET, COLD_TARGET, UNKEYABLE_TARGET];
    for target in targets {
        arm_stack_fixture(&mut cpu, ENTRY, INITIAL_ESP);
        bus.memory[INITIAL_ESP as usize..INITIAL_ESP as usize + 4]
            .copy_from_slice(&target.to_le_bytes());
        assert!(cpu.try_run_direct_block_for_test(&mut bus, source).unwrap());
        assert_eq!(cpu.registers.eip, target);
    }

    let exits = cpu
        .perf_counters()
        .jit_direct_unresolved_dynamic_miss_or_unbound
        - before.jit_direct_unresolved_dynamic_miss_or_unbound;
    assert_eq!(
        exits,
        targets.len() as u64,
        "every RET must miss the inline cache"
    );

    let snapshot = cpu
        .direct_barrier_census_snapshot()
        .expect("census must be allocated for this test");
    let classes: Vec<(&str, u64)> = snapshot.dynamic_miss_targets.clone();
    assert_eq!(
        classes.iter().map(|(_, n)| n).sum::<u64>(),
        exits,
        "dynamic class totals must close on the exit counter exactly, got {classes:?}"
    );
    assert_eq!(
        classes
            .iter()
            .find(|(l, _)| *l == "no_key")
            .expect("missing class no_key")
            .1,
        1,
        "a dynamic target `key_for` refuses must be classified, not dropped"
    );
}

/// A dynamic miss into a REJECTED block is attributed back to the barrier that refused it.
///
/// `note_dynamic_miss_target` used to take the class and discard the entry linear, so this whole
/// lane landed on no row at all — 2.86M exits per quake run, larger than its entire attributed
/// static row set, and the lane Slice 4 measured at 65% of the static one for the row it lowered.
/// The two columns are asserted apart: a row must not report a dynamic miss as a static unbound.
#[test]
fn a_dynamic_miss_into_a_rejected_block_is_attributed_to_its_barrier() {
    const ENTRY: u32 = 0x300;
    const REJECTED_TARGET: u32 = 0x400;
    const INITIAL_ESP: u32 = 0x3000;
    const BARRIER: u8 = 0xf4; // HLT — refused by the non-continuable arm, not by `classify`

    let mut memory = vec![0; 0x0010_0000];
    memory[ENTRY as usize] = 0xc3; // ret
    memory[REJECTED_TARGET as usize] = BARRIER;

    let mut cpu = flat_stack_cpu(ENTRY);
    cpu.jit_direct.enable_barrier_census_for_test();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    decode_fixture(&mut cpu, &mut bus, &[ENTRY, REJECTED_TARGET]);
    map_direct_page(
        &mut cpu,
        &mut bus,
        INITIAL_ESP,
        INITIAL_ESP,
        jit::fast_map::PagePermissions::UNPAGED,
        true,
        false,
    );
    let source = install_fixture_block(&mut cpu, ENTRY);

    // Refuse the target the way the dispatcher would, so it becomes `BlockState::Rejected` AND
    // leaves a census row behind. A one-instruction non-terminal block cannot survive the
    // three-slot minimum, so this is a structural rejection by construction. The probe first is
    // load-bearing: `reject` only rewrites a key already in `Seen`, exactly as `install` does.
    let target_key = jit::direct::key_for(&cpu, REJECTED_TARGET, true).expect("target key");
    assert!(matches!(
        cpu.jit_direct.probe(target_key),
        jit::direct::BlockProbe::Interpret
    ));
    let span = match jit::direct::compile(&mut cpu, REJECTED_TARGET, true) {
        jit::direct::CompileOutcome::StructuralReject(span) => span,
        _ => panic!("the barrier target must reject structurally"),
    };
    cpu.jit_direct.reject(span);

    for _ in 0..2 {
        arm_stack_fixture(&mut cpu, ENTRY, INITIAL_ESP);
        bus.memory[INITIAL_ESP as usize..INITIAL_ESP as usize + 4]
            .copy_from_slice(&REJECTED_TARGET.to_le_bytes());
        assert!(cpu.try_run_direct_block_for_test(&mut bus, source).unwrap());
        assert_eq!(cpu.registers.eip, REJECTED_TARGET);
    }

    let snapshot = cpu
        .direct_barrier_census_snapshot()
        .expect("census must be allocated for this test");
    assert_eq!(
        snapshot
            .dynamic_miss_targets
            .iter()
            .find(|(label, _)| *label == "rejected")
            .expect("missing class rejected")
            .1,
        2,
        "both RETs must classify as a miss into a rejected block"
    );
    let row = snapshot
        .rows
        .iter()
        .find(|row| row.opcode == u16::from(BARRIER))
        .expect("the barrier that refused the target must own a row");
    assert_eq!(
        row.dynamic_unbound_exits, 2,
        "both dynamic misses must land on the barrier's row"
    );
    assert_eq!(
        row.unbound_exits, 0,
        "a dynamic miss must not be reported in the static column"
    );
}

/// The chain-used link mask, at the machine rather than at `BlockCache`
/// (dev_docs/plans/2026-08-18-chain-used-link-mask.md). Two blocks compiled under DIFFERENT ES
/// descriptors, neither of which touches ES: whole-array snapshot equality refused this edge, the
/// source exited `StaticUnbound` on every iteration, and prince-586 paid a dispatcher round trip
/// per 1.58 guest instructions for it.
///
/// The edge must now form and the transfer must happen NATIVELY -- and the result must still be
/// the interpreter's, because a chained successor runs no entry check of its own.
#[test]
fn direct_chain_links_across_a_segment_descriptor_neither_block_uses() {
    const ENTRY: u32 = 0x200;
    const SECOND: u32 = 0x220;
    const DONE: u32 = 0x240;

    let mut memory = vec![0; 0x5000];
    memory[ENTRY as usize..ENTRY as usize + 10].copy_from_slice(&[
        0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax,1
        0xe9, 0x16, 0x00, 0x00, 0x00, // jmp 0x220
    ]);
    memory[SECOND as usize..SECOND as usize + 10].copy_from_slice(&[
        0xb9, 0x02, 0x00, 0x00, 0x00, // mov ecx,2
        0xe9, 0x16, 0x00, 0x00, 0x00, // jmp 0x240
    ]);
    memory[DONE as usize] = 0xf4; // hlt, never compiled: the chain must END here

    let mut native = flat_stack_cpu(ENTRY);
    let mut interp = flat_stack_cpu(ENTRY);
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [ENTRY, ENTRY + 5, SECOND, SECOND + 5];
    decode_fixture(&mut native, &mut native_bus, &starts);
    decode_fixture(&mut interp, &mut interp_bus, &starts);

    // The compile-time capture is whatever the segment registers hold AT INSTALL, so installing
    // the two blocks under different ES selectors is exactly the census's class B: a frozen
    // descriptor that differs and that neither block reads.
    let entry_es = native.registers.segment(SegmentIndex::Es);
    let entry_block = install_fixture_block(&mut native, ENTRY);
    native
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::flat(0x20, 0x93));
    install_fixture_block(&mut native, SECOND);
    assert_ne!(entry_es, native.registers.segment(SegmentIndex::Es));
    // Back to the root's own ES for the run: the root still proves all six of its descriptors.
    native.registers.set_segment(SegmentIndex::Es, entry_es);

    native.set_eip(ENTRY);
    interp.set_eip(ENTRY);
    let before = native.perf_counters().clone();
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, entry_block)
            .unwrap()
    );
    for _ in 0..4 {
        interp.cycle(&mut interp_bus).unwrap();
    }

    assert_eq!(native.registers.eip, DONE);
    assert_eq!(native.registers, interp.registers);
    assert_eq!(native_bus.memory, interp_bus.memory);
    let after = native.perf_counters().clone();
    assert_eq!(
        after.jit_direct_linked_transfers - before.jit_direct_linked_transfers,
        1,
        "the two blocks must chain natively, not through the dispatcher"
    );
    assert_eq!(
        after.jit_direct_entries - before.jit_direct_entries,
        1,
        "one host entry must cover both blocks"
    );
    assert_unresolved_reason_deltas(&before, &after, [1, 0, 0, 0]);
}

/// The other half of the chain mask's soundness, at the machine: the ROOT's entry check has to
/// prove the descriptors the CHAIN needs, not the ones the root itself happens to use.
///
/// The root reads no memory at all. Its successor reads through ES, so the chain requirement
/// carries ES even though the root's own pinned mask is empty. Enter with a live ES whose BASE
/// differs from the one the successor baked, and the entry must be REFUSED -- relaxing the linked
/// arm to the root's own mask would let the chain run and read the wrong address, silently.
#[test]
pub(super) fn direct_chain_entry_validates_a_segment_only_the_successor_uses() {
    const ENTRY: u32 = 0x200;
    const SECOND: u32 = 0x220;
    const DONE: u32 = 0x240;
    const BAKED: u32 = 0x3000;
    const SHIFT: u32 = 0x100;

    let mut memory = vec![0; 0x5000];
    memory[ENTRY as usize..ENTRY as usize + 10].copy_from_slice(&[
        0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax,1
        0xe9, 0x16, 0x00, 0x00, 0x00, // jmp 0x220
    ]);
    memory[SECOND as usize..SECOND as usize + 12].copy_from_slice(&[
        0x26, 0x8b, 0x15, 0x00, 0x30, 0x00, 0x00, // mov edx,[es:0x3000]
        0xe9, 0x14, 0x00, 0x00, 0x00, // jmp 0x240
    ]);
    memory[DONE as usize] = 0xf4;
    memory[BAKED as usize..BAKED as usize + 4].copy_from_slice(&0xdead_beefu32.to_le_bytes());
    memory[(BAKED + SHIFT) as usize..(BAKED + SHIFT) as usize + 4]
        .copy_from_slice(&0xcafe_babeu32.to_le_bytes());

    let mut native = flat_stack_cpu(ENTRY);
    let mut native_bus = TestBus::with_memory(memory);
    native_bus.direct_pages_enabled = true;
    native_bus.direct_page_clocks = true;
    decode_fixture(
        &mut native,
        &mut native_bus,
        &[ENTRY, ENTRY + 5, SECOND, SECOND + 7],
    );
    map_direct_page(
        &mut native,
        &mut native_bus,
        0x3000,
        0x3000,
        jit::fast_map::PagePermissions::UNPAGED,
        true,
        true,
    );

    let entry_block = install_fixture_block(&mut native, ENTRY);
    install_fixture_block(&mut native, SECOND);

    // Non-vacuity first: with the live ES the successor baked, the chain forms and runs.
    native.set_eip(ENTRY);
    let before = native.perf_counters().clone();
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, entry_block)
            .unwrap()
    );
    assert_eq!(native.registers.eip, DONE);
    assert_eq!(native.registers.edx(), 0xdead_beef);
    assert_eq!(
        native.perf_counters().jit_direct_linked_transfers - before.jit_direct_linked_transfers,
        1,
        "the successor must be reached natively, or this row proves nothing about chains"
    );

    // Now move ES's BASE. The root pins nothing, so only the strict six-descriptor check stands
    // between the chain and a read through the stale base.
    let baked_es = native.registers.segment(SegmentIndex::Es);
    let mut shifted_es = baked_es;
    shifted_es.base = SHIFT;
    native.registers.set_segment(SegmentIndex::Es, shifted_es);
    native.set_eip(ENTRY);
    native.registers.set_edx(0);
    let before = native.perf_counters().clone();
    assert!(
        !native
            .try_run_direct_block_for_test(&mut native_bus, entry_block)
            .unwrap(),
        "the root must refuse: the chain needs an ES the root itself never pinned"
    );
    assert_eq!(
        native.perf_counters().jit_direct_reject_data_segment
            - before.jit_direct_reject_data_segment,
        1
    );
    assert_eq!(native.registers.eip, ENTRY);
    assert_eq!(
        native.registers.edx(),
        0,
        "nothing may have run: a chained read here would have used the baked base"
    );
}

/// **M-PRE, and it is a statement about `main`, not about the chain entry check.**
///
/// Every `DirectKind` that bakes a stack access must report SS in `pinned_segments`, or the
/// MASKED arm -- which `main` has always taken for an unlinked root -- lets a block run against a
/// stack descriptor its snapshot does not match. The chain entry check inherits that dependency
/// and does not create it, which is why a failure here opens a shipped-defect report against
/// `main` rather than stopping this slice (review round 1, M4).
///
/// Stated end to end rather than by inspecting the mask: each row is compiled, installed with no
/// successor -- so its entry takes exactly the masked check -- and then entered with SS's BASE
/// moved. A row that runs is a row whose stack segment is not pinned.
///
/// `ENTER` is absent because there is no 32-bit `Enter` kind to test: `0xC8` in a 32-bit segment
/// is not admitted at all (the compile refuses it), and `Enter16` needs a 16-bit code segment,
/// which this harness does not build. `LEAVE` covers the other half of that pair here.
///
/// **RESULT ON THIS TREE: GREEN, all five rows.** No shipped-defect report is owed.
#[test]
fn entry_mask_pins_ss_for_every_stack_baking_kind() {
    const ROW: u32 = 0x200;
    const ROW_DONE: u32 = 0x260;
    for (name, body) in [
        ("push r32", vec![0x50]),
        ("pop r32", vec![0x58]),
        ("push imm32", vec![0x68, 0x11, 0x22, 0x33, 0x44]),
        ("leave", vec![0xc9]),
        (
            "ss: override",
            vec![0x36, 0x8b, 0x15, 0x00, 0x30, 0x00, 0x00],
        ),
    ] {
        let len = u32::try_from(body.len()).expect("a short row");
        let mut memory = vec![0; 0x5000];
        memory[ROW as usize..ROW as usize + body.len()].copy_from_slice(&body);
        let jump = ROW + len;
        memory[jump as usize] = 0xe9;
        memory[jump as usize + 1..jump as usize + 5]
            .copy_from_slice(&(ROW_DONE - (jump + 5)).to_le_bytes());
        memory[ROW_DONE as usize] = 0xf4;

        let mut cpu = flat_stack_cpu(ROW);
        cpu.registers.set_esp(0x2800);
        cpu.registers.set_ebp(0x2800);
        let mut bus = TestBus::with_memory(memory);
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        decode_fixture(&mut cpu, &mut bus, &[ROW, jump]);
        for page in [0x2000, 0x3000] {
            map_direct_page(
                &mut cpu,
                &mut bus,
                page,
                page,
                jit::fast_map::PagePermissions::UNPAGED,
                true,
                true,
            );
        }
        let block = install_fixture_block(&mut cpu, ROW);
        assert!(
            !cpu.jit_direct.has_linked_successor(block.id()),
            "{name}: the row must be UNLINKED, or it takes the strict arm and proves nothing \
             about the pinned set"
        );

        shift_segment_base(&mut cpu, SegmentIndex::Ss, 0x100);
        cpu.set_eip(ROW);
        let before = cpu.perf_counters().clone();
        assert!(
            !cpu.try_run_direct_block_for_test(&mut bus, block).unwrap(),
            "{name}: the masked entry check must refuse a moved SS, or this kind is missing SS \
             from `pinned_segments` and `main` runs it against a stale stack base"
        );
        assert_eq!(
            cpu.perf_counters().jit_direct_reject_data_segment_masked
                - before.jit_direct_reject_data_segment_masked,
            1,
            "{name}: and the refusal must be the MASKED arm's, over the block's own pinned set"
        );
    }
}

/// Restores the ambient `IZARRAVM_CHAIN_ENTRY_CHECK` arm when it drops. The arm is read once per
/// `BlockCache`, so it must be forced BEFORE the fixture builds its CPU.
struct ChainEntryCheckArm;

impl ChainEntryCheckArm {
    fn forced(armed: bool) -> Self {
        jit::direct::set_chain_entry_check_for_test(Some(armed));
        Self
    }
}

impl Drop for ChainEntryCheckArm {
    fn drop(&mut self) {
        jit::direct::set_chain_entry_check_for_test(None);
    }
}

const CHAIN_ENTRY_ROOT: u32 = 0x200;
const CHAIN_ENTRY_SECOND: u32 = 0x220;
const CHAIN_ENTRY_DONE: u32 = 0x240;
const CHAIN_ENTRY_BAKED: u32 = 0x3000;

/// The shape all three entry-check rows below run on, and the shape that discriminates the two
/// arms: a root that pins NOTHING at all, chained to a successor that reads through ES.
///
/// * The root's OWN mask is empty, so `data_matches` on it proves nothing.
/// * The root's CHAIN REQUIREMENT is `{ES}`, because the successor pins ES.
/// * `all_data_matches` on the root proves all six, ES included -- sound, and far stronger.
///
/// So moving ES must refuse on BOTH arms (the chain needs it) and moving a segment neither block
/// pins must refuse on the OFF arm and be ADMITTED on the armed one. Returned already run once,
/// with the chain formed and asserted live, because every row below is vacuous without that.
fn chain_entry_check_fixture() -> (CpuGsw, TestBus, jit::direct::CompiledBlock) {
    let mut memory = vec![0; 0x5000];
    memory[CHAIN_ENTRY_ROOT as usize..CHAIN_ENTRY_ROOT as usize + 10].copy_from_slice(&[
        0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax,1
        0xe9, 0x16, 0x00, 0x00, 0x00, // jmp 0x220
    ]);
    memory[CHAIN_ENTRY_SECOND as usize..CHAIN_ENTRY_SECOND as usize + 12].copy_from_slice(&[
        0x26, 0x8b, 0x15, 0x00, 0x30, 0x00, 0x00, // mov edx,[es:0x3000]
        0xe9, 0x14, 0x00, 0x00, 0x00, // jmp 0x240
    ]);
    memory[CHAIN_ENTRY_DONE as usize] = 0xf4;
    memory[CHAIN_ENTRY_BAKED as usize..CHAIN_ENTRY_BAKED as usize + 4]
        .copy_from_slice(&0xdead_beefu32.to_le_bytes());

    let mut cpu = flat_stack_cpu(CHAIN_ENTRY_ROOT);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    decode_fixture(
        &mut cpu,
        &mut bus,
        &[
            CHAIN_ENTRY_ROOT,
            CHAIN_ENTRY_ROOT + 5,
            CHAIN_ENTRY_SECOND,
            CHAIN_ENTRY_SECOND + 7,
        ],
    );
    map_direct_page(
        &mut cpu,
        &mut bus,
        CHAIN_ENTRY_BAKED,
        CHAIN_ENTRY_BAKED,
        jit::fast_map::PagePermissions::UNPAGED,
        true,
        true,
    );

    let root = install_fixture_block(&mut cpu, CHAIN_ENTRY_ROOT);
    install_fixture_block(&mut cpu, CHAIN_ENTRY_SECOND);

    cpu.set_eip(CHAIN_ENTRY_ROOT);
    let before = cpu.perf_counters().clone();
    assert!(cpu.try_run_direct_block_for_test(&mut bus, root).unwrap());
    assert_eq!(cpu.registers.eip, CHAIN_ENTRY_DONE);
    assert_eq!(cpu.registers.edx(), 0xdead_beef);
    assert_eq!(
        cpu.perf_counters().jit_direct_linked_transfers - before.jit_direct_linked_transfers,
        1,
        "the successor must be reached natively, or no row here proves anything about chains"
    );
    // THE PRECONDITION EVERY ROW BELOW NEEDS, asserted rather than assumed: with no live edge
    // both arms take `data_matches` and every row is green for the wrong reason.
    assert!(
        cpu.jit_direct.has_linked_successor(root.id()),
        "the root must still hold a LIVE outbound edge at the instant of the next entry"
    );
    (cpu, bus, root)
}

fn shift_segment_base(cpu: &mut CpuGsw, segment: SegmentIndex, delta: u32) {
    let mut record = cpu.registers.segment(segment);
    record.base = record.base.wrapping_add(delta);
    cpu.registers.set_segment(segment, record);
}

/// ARMED: the root pins nothing and the chain needs ES, so moving ES must REFUSE.
///
/// This is the soundness row for the armed arm. A mutant that reads `segment_layouts` here instead
/// of `chain_layouts` sees an EMPTY mask, admits the entry, and lets the successor's chained body
/// read through a base nobody validated -- the miscompile the strict arm existed to prevent.
#[test]
fn chain_entry_check_refuses_a_root_whose_successor_pins_a_moved_segment() {
    let _arm = ChainEntryCheckArm::forced(true);
    let (mut cpu, mut bus, root) = chain_entry_check_fixture();
    assert!(cpu.jit_direct.chain_entry_check_armed());

    shift_segment_base(&mut cpu, SegmentIndex::Es, 0x100);
    cpu.set_eip(CHAIN_ENTRY_ROOT);
    cpu.registers.set_edx(0);
    let before = cpu.perf_counters().clone();

    assert!(
        !cpu.try_run_direct_block_for_test(&mut bus, root).unwrap(),
        "the chain requirement names ES even though the root itself pins nothing"
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_reject_data_segment - before.jit_direct_reject_data_segment,
        1
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_reject_data_segment_strict
            - before.jit_direct_reject_data_segment_strict,
        1,
        "a linked root's reject stays on the STRICT arm; the name's MEANING moved, not the arm"
    );
    assert_eq!(cpu.registers.eip, CHAIN_ENTRY_ROOT);
    assert_eq!(
        cpu.registers.edx(),
        0,
        "nothing may have run: a chained read here would have used the baked base"
    );
}

/// ARMED: moving a segment NEITHER block pins must be ADMITTED, and the chain must still run.
///
/// This is the whole point of the slice, and it is the row `all_data_matches` cannot pass: on the
/// OFF arm the identical state refuses (see the row below). A mutant that calls
/// `all_data_matches` on the chain layout -- keeping the array swap and dropping the mask -- is
/// green everywhere else and red here.
#[test]
fn chain_entry_check_admits_a_root_whose_chain_pins_nothing_on_the_moved_segment() {
    let _arm = ChainEntryCheckArm::forced(true);
    let (mut cpu, mut bus, root) = chain_entry_check_fixture();

    shift_segment_base(&mut cpu, SegmentIndex::Gs, 0x100);
    cpu.set_eip(CHAIN_ENTRY_ROOT);
    cpu.registers.set_edx(0);
    let before = cpu.perf_counters().clone();

    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, root).unwrap(),
        "GS is in no block's pinned set, so the chain requirement says nothing about it"
    );
    assert_eq!(cpu.registers.eip, CHAIN_ENTRY_DONE);
    assert_eq!(
        cpu.registers.edx(),
        0xdead_beef,
        "and the chain must still read through the ES base it was validated against"
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_reject_data_segment - before.jit_direct_reject_data_segment,
        0
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_linked_transfers - before.jit_direct_linked_transfers,
        1,
        "the admitted entry must still CHAIN, not fall out to the dispatcher"
    );
}

/// OFF: the same state the row above admits must REFUSE, because the OFF arm is `main` verbatim
/// and a linked root there proves all six of its own descriptors.
///
/// Two mutants die here. Dropping the OFF arm's `has_link` branch leaves both arms on
/// `data_matches`, and the root's own mask is empty, so the entry would be admitted. Arming the
/// chain check unconditionally does the same. The `has_linked_successor` precondition inside the
/// fixture is what stops this row passing because the edge quietly failed to form.
#[test]
fn chain_entry_check_off_arm_reproduces_all_data_matches() {
    let _arm = ChainEntryCheckArm::forced(false);
    let (mut cpu, mut bus, root) = chain_entry_check_fixture();
    assert!(!cpu.jit_direct.chain_entry_check_armed());

    shift_segment_base(&mut cpu, SegmentIndex::Gs, 0x100);
    cpu.set_eip(CHAIN_ENTRY_ROOT);
    let before = cpu.perf_counters().clone();

    assert!(
        !cpu.try_run_direct_block_for_test(&mut bus, root).unwrap(),
        "main's strict arm compares all six descriptors, GS included"
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_reject_data_segment_strict
            - before.jit_direct_reject_data_segment_strict,
        1
    );
    assert_eq!(cpu.registers.eip, CHAIN_ENTRY_ROOT);
}

/// The compile walk's page budget: a block long enough to overflow one host page must come out of
/// the walk already inside it, without `compile_with_page_len`'s recovery search running at all.
///
/// The regression this pins, measured on the tombraid loader (2026-08-22): once the S3 call-out
/// rows joined blocks that used to end at those instructions, 85.5% of full-length walks emitted
/// past the 4 KiB page, and each one paid four MORE walks and four more emissions to rediscover
/// the length the size model can compute during the first one. That was 4.4 seconds of a 10.1
/// second phase, and `jit_direct_compile_ns` fell 81% when the walk learned to stop.
///
/// Written as three claims, because any one alone can pass while the mechanism is absent:
///
/// 1. the UNBUDGETED walk over the same bytes really does overflow, so the fixture is a case the
///    search would have had to handle (delete the budget and this still passes);
/// 2. the budgeted block fits and is shorter, so the budget bound it (delete the budget and this
///    fails only because of 3, since the search produces a fitting block too);
/// 3. `compile_page_overflows` and `compile_page_search_steps` are BOTH zero, which is the only
///    claim that separates "the walk stopped" from "the search cleaned up afterwards".
#[test]
fn the_page_budget_ends_the_walk_before_the_recovery_search_runs() {
    const TARGET: u32 = 0x3000;
    const COUNT: usize = 32;
    // `mov eax, [0x3000]`: a plain memory load, deliberately NOT a memory-ALU form, whose block
    // length is already governed by `MAX_MEMORY_ALU_BLOCK_INSTRUCTIONS` and would make the page
    // budget untestable through it.
    let instruction = [0x8b, 0x05, 0x00, 0x30, 0x00, 0x00];
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

    let page = jit::exec_mem::host_page_len();
    let unbudgeted = jit::direct::compile_with_instruction_limit_for_test(
        &mut native,
        ALU_MEM_ENTRY,
        true,
        jit::direct::MAX_BLOCK_INSTRUCTIONS,
    )
    .expect("the unbudgeted walk must compile something");
    assert!(
        unbudgeted.code.len() > page,
        "the fixture must be a case the recovery search would have had to handle; {} bytes fits \
         a {page}-byte page",
        unbudgeted.code.len()
    );

    let before = native.direct_stall_snapshot();
    let budgeted = jit::direct::compile(&mut native, ALU_MEM_ENTRY, true)
        .expect("the budgeted walk must compile a shorter block");
    assert!(
        budgeted.code.len() <= page,
        "the budgeted block emitted {} bytes into a {page}-byte page",
        budgeted.code.len()
    );
    assert!(
        budgeted.span.instructions < unbudgeted.span.instructions,
        "the budget must have stopped the walk short: {} of {} instructions",
        budgeted.span.instructions,
        unbudgeted.span.instructions
    );
    // 4. how far the model's answer sits from the longest prefix that actually fits, which is the
    //    claim that separates a working size model from a merely conservative one. An EQUALITY is
    //    what one would want here and it is not true, deliberately: the constants are calibrated
    //    on the tombraid loader, whose memory slots average 341 emitted bytes, and this fixture's
    //    `mov eax,[disp32]` is the cheapest memory shape there is at 250. The model therefore ends
    //    the block early on it, and this pins BY HOW MUCH -- three slots of a thirteen-slot
    //    maximum. Re-calibrating the constants moves this number; a broken budget moves it a lot.
    let n = usize::from(budgeted.span.instructions);
    let mut longest = 0usize;
    for k in 3..=usize::from(unbudgeted.span.instructions) {
        let candidate = jit::direct::compile_with_instruction_limit_for_test(
            &mut native,
            ALU_MEM_ENTRY,
            true,
            k,
        )
        .expect("every prefix of this fixture compiles unbudgeted");
        if candidate.code.len() <= page {
            longest = k;
        }
    }
    assert!(
        n <= longest,
        "the budget must never admit MORE than fits: {n} slots against a {longest}-slot maximum"
    );
    assert!(
        longest - n <= 3,
        "the model gave up {} slots of {longest}, which is past the slack this fixture measured          (3). Either the constants moved or the estimate stopped tracking the emitter",
        longest - n
    );

    let after = native.direct_stall_snapshot();
    assert_eq!(
        after.compile_page_overflows - before.compile_page_overflows,
        0,
        "the size model must have kept the full-length walk inside the page"
    );
    assert_eq!(
        after.compile_page_search_steps - before.compile_page_search_steps,
        0,
        "and so the recovery search must not have compiled a single candidate"
    );
}
