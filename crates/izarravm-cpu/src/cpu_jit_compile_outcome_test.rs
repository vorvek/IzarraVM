// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const ENTRY: u32 = 0x100;

fn fresh() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    for segment in [SegmentIndex::Cs, SegmentIndex::Ds, SegmentIndex::Ss] {
        cpu.load_segment_real(segment, 0);
    }
    for segment in [SegmentIndex::Cs, SegmentIndex::Ss] {
        let mut descriptor = cpu.registers.segment(segment);
        descriptor.default_size_32 = true;
        cpu.registers.set_segment(segment, descriptor);
    }
    cpu.registers.eip = ENTRY;
    cpu
}

fn fixture(code: &[u8]) -> (CpuGsw, TestBus) {
    let mut memory = vec![0; 0x2000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(code);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    let mut cpu = fresh();
    cpu.jit_direct.set_fast_map_enabled_for_test(true);
    cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0, BusAccessKind::DataRead)
        .expect("initialize direct map");
    (cpu, bus)
}

fn warm(cpu: &mut CpuGsw, bus: &mut TestBus, addresses: &[u32]) {
    for &linear in addresses {
        cpu.registers.eip = linear;
        cpu.begin_instruction();
        cpu.fetch_decoded(bus, linear).expect("fixture decode");
    }
}

fn structural(outcome: jit::direct::CompileOutcome) -> jit::direct::RejectedSpan {
    match outcome {
        jit::direct::CompileOutcome::StructuralReject(span) => span,
        jit::direct::CompileOutcome::Compiled(_) => panic!("fixture unexpectedly compiled"),
        jit::direct::CompileOutcome::Retry => panic!("fixture unexpectedly requested a retry"),
    }
}

fn compiled(outcome: jit::direct::CompileOutcome) -> jit::direct::Compilation {
    match outcome {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("fixture unexpectedly became a structural rejection")
        }
        jit::direct::CompileOutcome::Retry => panic!("fixture unexpectedly requested a retry"),
    }
}

#[test]
fn structural_rejection_includes_the_complete_short_prefix_and_barrier() {
    let cases: &[(&[u8], &[u32], u16)] = &[
        (&[0x90], &[ENTRY], 1),
        (&[0x40, 0x90], &[ENTRY, ENTRY + 1], 2),
        (&[0x40, 0x41, 0x90], &[ENTRY, ENTRY + 1, ENTRY + 2], 3),
    ];

    for &(code, addresses, expected_len) in cases {
        let (mut cpu, mut bus) = fixture(code);
        warm(&mut cpu, &mut bus, addresses);

        let span = structural(jit::direct::compile(&mut cpu, ENTRY, true));
        assert_eq!(span.key().linear, ENTRY);
        assert_eq!(span.key().physical, ENTRY);
        assert_eq!(span.guest_len(), expected_len);
    }
}

#[test]
fn three_supported_slots_compile_before_an_unsupported_barrier() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42, 0x90]);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(compilation.span.instructions, 3);
    assert_eq!(compilation.span.guest_len, 3);
}

#[test]
fn supported_terminals_compile_even_when_the_block_is_short() {
    let (mut single_cpu, mut single_bus) = fixture(&[0xc3]);
    warm(&mut single_cpu, &mut single_bus, &[ENTRY]);
    let single = compiled(jit::direct::compile(&mut single_cpu, ENTRY, true));
    assert_eq!(single.span.instructions, 1);
    assert_eq!(single.span.guest_len, 1);

    let (mut pair_cpu, mut pair_bus) = fixture(&[0x40, 0xc3]);
    warm(&mut pair_cpu, &mut pair_bus, &[ENTRY, ENTRY + 1]);
    let pair = compiled(jit::direct::compile(&mut pair_cpu, ENTRY, true));
    assert_eq!(pair.span.instructions, 2);
    assert_eq!(pair.span.guest_len, 2);
}

#[test]
fn missing_third_decode_retries_then_compiles_without_a_guest_write() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1]);
    assert!(matches!(
        jit::direct::compile(&mut cpu, ENTRY, true),
        jit::direct::CompileOutcome::Retry
    ));

    warm(&mut cpu, &mut bus, &[ENTRY + 2]);
    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(compilation.span.instructions, 3);
    assert_eq!(compilation.span.guest_len, 3);
}

#[test]
fn retry_state_stays_dormant_after_decode_recovery_until_cache_clear() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1]);
    cpu.set_jit_auto_admit(true);
    let key = jit::direct::key_for(&cpu, ENTRY, true).expect("fixture key");
    let attempts = cpu.perf_counters().jit_direct_compile_attempts;
    let installed = cpu.perf_counters().jit_direct_blocks_installed;

    cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
        .expect("first observation");
    cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
        .expect("missing decode retry");
    assert_eq!(
        cpu.perf_counters().jit_direct_compile_attempts,
        attempts + 1
    );
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Rejected
    ));

    warm(&mut cpu, &mut bus, &[ENTRY + 2]);
    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(compilation.span.instructions, 3);
    for _ in 0..1_000 {
        cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
            .expect("dormant probe");
    }
    assert_eq!(
        cpu.perf_counters().jit_direct_compile_attempts,
        attempts + 1
    );
    assert_eq!(cpu.perf_counters().jit_direct_blocks_installed, installed);

    cpu.jit_direct.clear();
    cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
        .expect("first observation after clear");
    cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
        .expect("compile after clear");
    assert_eq!(
        cpu.perf_counters().jit_direct_compile_attempts,
        attempts + 2
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_blocks_installed,
        installed + 1
    );
}

#[test]
fn direct_admission_waits_for_the_configured_heat() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);
    cpu.set_jit_auto_admit(true);
    cpu.jit_direct.set_admission_heat_for_test(8);
    let attempts = cpu.perf_counters().jit_direct_compile_attempts;

    for _ in 0..8 {
        cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
            .expect("heat observation");
    }
    assert_eq!(cpu.perf_counters().jit_direct_compile_attempts, attempts);

    cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
        .expect("compile after heat threshold");
    assert_eq!(
        cpu.perf_counters().jit_direct_compile_attempts,
        attempts + 1
    );
}

#[test]
fn disabled_native_continuations_leave_direct_counters_cold() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42, 0xf4]);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );
    cpu.jit_direct.set_fast_map_enabled_for_test(false);
    cpu.set_jit_auto_admit(false);
    cpu.registers.eip = ENTRY;

    for _ in 0..4 {
        let outcome = cpu.run_budgeted(&mut bus, u64::MAX).expect("JIT-off run");
        if outcome.halted {
            break;
        }
    }

    assert!(cpu.halted);
    let perf = cpu.perf_counters();
    assert_eq!(perf.jit_direct_entries, 0);
    assert_eq!(perf.jit_direct_compile_attempts, 0);
    assert_eq!(perf.jit_direct_hot_hits, 0);
    assert_eq!(perf.jit_direct_hash_hits, 0);
    assert_eq!(perf.jit_direct_lookup_misses, 0);
}

#[test]
fn page_straddling_barrier_retries_instead_of_becoming_persistent() {
    let mut memory = vec![0; 0x2000];
    memory[0xfff..0x1001].copy_from_slice(&[0xcd, 0x80]);
    let mut bus = TestBus::with_memory(memory);
    let mut cpu = fresh();
    cpu.registers.eip = 0xfff;
    cpu.begin_instruction();
    cpu.fetch_decoded(&mut bus, 0xfff)
        .expect("straddling INT decode");

    assert!(matches!(
        jit::direct::compile(&mut cpu, 0xfff, true),
        jit::direct::CompileOutcome::Retry
    ));
    assert!(cpu.decode_cache.get(0xfff, true).is_none());
}

#[test]
fn live_cs_limit_failure_retries_and_recovers_without_a_write() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);
    let mut cs = cpu.registers.cs();
    let old_limit = cs.limit;
    cs.limit = ENTRY;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);

    assert!(matches!(
        jit::direct::compile(&mut cpu, ENTRY, true),
        jit::direct::CompileOutcome::Retry
    ));

    cs.limit = old_limit;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(compilation.span.instructions, 3);
}

#[test]
fn live_segment_state_failure_retries_and_recovers_without_a_write() {
    let (mut cpu, mut bus) = fixture(&[0x8b, 0x00, 0xc3]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 2]);
    cpu.control.cr0 |= CR0_PE;
    let mut ds = cpu.registers.segment(SegmentIndex::Ds);
    let old_access = ds.access;
    ds.access = 0;
    cpu.registers.set_segment(SegmentIndex::Ds, ds);

    assert!(matches!(
        jit::direct::compile(&mut cpu, ENTRY, true),
        jit::direct::CompileOutcome::Retry
    ));

    ds.access = old_access;
    cpu.registers.set_segment(SegmentIndex::Ds, ds);
    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(compilation.span.instructions, 2);
}

#[test]
fn emitted_size_exhaustion_is_retryable_for_short_and_long_blocks() {
    let (mut terminal_cpu, mut terminal_bus) = fixture(&[0xc3]);
    warm(&mut terminal_cpu, &mut terminal_bus, &[ENTRY]);
    assert!(matches!(
        jit::direct::compile_with_page_len_for_test(&mut terminal_cpu, ENTRY, true, 1),
        jit::direct::CompileOutcome::Retry
    ));

    let (mut prefix_cpu, mut prefix_bus) = fixture(&[0x40, 0x41, 0x42, 0x43]);
    warm(
        &mut prefix_cpu,
        &mut prefix_bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );
    assert!(matches!(
        jit::direct::compile_with_page_len_for_test(&mut prefix_cpu, ENTRY, true, 1),
        jit::direct::CompileOutcome::Retry
    ));
}

fn reject(cache: &mut jit::JitState, key: jit::direct::BlockKey, len: usize) {
    assert!(matches!(
        cache.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    assert!(matches!(cache.probe(key), jit::direct::BlockProbe::Compile));
    cache.reject(jit::direct::RejectedSpan::new(key, len).expect("rejected fixture span"));
    assert!(matches!(
        cache.probe(key),
        jit::direct::BlockProbe::Rejected
    ));
}

#[test]
fn rejected_span_invalidates_on_every_owned_byte_but_not_adjacent_bytes() {
    for offset in 0..4 {
        let mut cache = jit::JitState::new(jit::direct::BlockCache::default());
        let key = jit::direct::BlockKey::new(0x200, 0x320, 7);
        reject(&mut cache, key, 4);
        assert_eq!(cache.invalidate_physical_range(0x320 + offset, 1), 1);
        assert!(!cache.range_hits_compiled_code(0x320, 4));
    }

    let mut cache = jit::JitState::new(jit::direct::BlockCache::default());
    let key = jit::direct::BlockKey::new(0x200, 0x320, 7);
    reject(&mut cache, key, 4);
    assert_eq!(cache.invalidate_physical_range(0x31f, 1), 0);
    assert_eq!(cache.invalidate_physical_range(0x324, 1), 0);
    assert!(matches!(
        cache.probe(key),
        jit::direct::BlockProbe::Rejected
    ));
}

#[test]
fn repeated_reject_does_not_acquire_a_second_watch_owner() {
    let mut cache = jit::JitState::new(jit::direct::BlockCache::default());
    let key = jit::direct::BlockKey::new(0x200, 0x320, 7);
    reject(&mut cache, key, 4);
    cache.reject(jit::direct::RejectedSpan::new(key, 4).expect("rejected fixture span"));

    assert_eq!(cache.invalidate_physical_range(0x322, 1), 1);
    assert!(!cache.range_hits_compiled_code(0x320, 4));
}

fn mixed_compiled_and_rejected_cache() -> (CpuGsw, jit::direct::BlockKey, jit::direct::BlockKey) {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);
    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    let compiled_key = compilation.span.key;
    assert!(matches!(
        cpu.jit_direct.probe(compiled_key),
        jit::direct::BlockProbe::Interpret
    ));
    assert!(matches!(
        cpu.jit_direct.probe(compiled_key),
        jit::direct::BlockProbe::Compile
    ));
    cpu.jit_direct
        .install(&compilation)
        .expect("compiled fixture install");

    let rejected_key = jit::direct::BlockKey::new(ENTRY + 8, ENTRY + 8, cpu.jit_mode_key());
    reject(&mut cpu.jit_direct, rejected_key, 1);
    (cpu, compiled_key, rejected_key)
}

#[test]
fn compiled_and_rejected_owners_share_a_chunk_until_both_retire() {
    for reject_first in [false, true] {
        let (mut cpu, compiled_key, rejected_key) = mixed_compiled_and_rejected_cache();
        assert!(
            cpu.jit_direct
                .range_hits_compiled_code(compiled_key.physical, 16)
        );

        let first = if reject_first {
            rejected_key.physical
        } else {
            compiled_key.physical
        };
        let second = if reject_first {
            compiled_key.physical
        } else {
            rejected_key.physical
        };
        assert_eq!(cpu.jit_direct.invalidate_physical_range(first, 1), 1);
        assert!(
            cpu.jit_direct
                .range_hits_compiled_code(compiled_key.physical, 16)
        );
        assert_eq!(cpu.jit_direct.invalidate_physical_range(second, 1), 1);
        assert!(
            !cpu.jit_direct
                .range_hits_compiled_code(compiled_key.physical, 16)
        );
    }
}

#[test]
fn cache_clear_unpublishes_rejected_watch_and_keeps_the_table_base() {
    let mut cache = jit::JitState::new(jit::direct::BlockCache::default());
    let key = jit::direct::BlockKey::new(0x200, 0x12_320, 7);
    let base = cache.native_code_watch_table();
    let entry = unsafe { (base as *const usize).add((key.physical >> 12) as usize) };
    reject(&mut cache, key, 4);
    assert_ne!(unsafe { *entry }, 0);

    cache.clear();
    assert_eq!(unsafe { *entry }, 0);
    assert_eq!(cache.native_code_watch_table(), base);
    assert!(!cache.range_hits_compiled_code(key.physical, 4));

    reject(&mut cache, key, 4);
    assert_ne!(unsafe { *entry }, 0);
    assert_eq!(cache.invalidate_physical_range(key.physical, 1), 1);
    assert_eq!(unsafe { *entry }, 0);
}

#[test]
fn failed_install_consumes_seen_without_a_code_watch() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);
    let mut compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    let key = compilation.span.key;
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Compile
    ));
    compilation.code.clear();

    assert!(cpu.jit_direct.install(&compilation).is_none());
    cpu.jit_direct.dormant(key);
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Rejected
    ));
    assert!(
        !cpu.jit_direct
            .range_hits_compiled_code(key.physical, u32::from(compilation.span.guest_len))
    );
}

#[test]
fn direct_page_coverage_failure_consumes_seen_without_a_watch() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);
    let key = jit::direct::key_for(&cpu, ENTRY, true).expect("fixture key");
    cpu.set_jit_auto_admit(true);
    bus.direct_pages_enabled = false;
    let attempts = cpu.perf_counters().jit_direct_compile_attempts;

    cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
        .expect("first observation");
    cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
        .expect("coverage failure");
    for _ in 0..100_000 {
        cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
            .expect("dormant probe");
    }

    assert_eq!(
        cpu.perf_counters().jit_direct_compile_attempts,
        attempts + 1
    );
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Rejected
    ));
    assert!(!cpu.jit_direct.range_hits_compiled_code(key.physical, 3));
    cpu.jit_direct.clear();
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
}

fn rejected_after_decode_eviction() -> (CpuGsw, TestBus, jit::direct::BlockKey) {
    let (mut cpu, mut bus) = fixture(&[0x90]);
    warm(&mut cpu, &mut bus, &[ENTRY]);
    let span = structural(jit::direct::compile(&mut cpu, ENTRY, true));
    let key = span.key();
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Compile
    ));
    cpu.jit_direct.reject(span);

    let insn = cpu
        .decode_cache
        .get(ENTRY, true)
        .expect("rejected instruction decode");
    let collision = ENTRY + cpu.decode_cache.lines.len() as u32;
    assert!(cpu.decode_cache.put(collision, insn, true, 0x800).inserted);
    assert!(cpu.decode_cache.get(ENTRY, true).is_none());
    assert!(cpu.jit_direct.range_hits_compiled_code(ENTRY, 1));
    (cpu, bus, key)
}

#[test]
fn interpreter_write_recovers_rejection_after_decode_eviction() {
    let (mut cpu, mut bus, key) = rejected_after_decode_eviction();
    cpu.write_memory_u8(
        &mut bus,
        SegmentIndex::Ds,
        ENTRY,
        0x91,
        BusAccessKind::DataWrite,
    )
    .expect("interpreter write");

    assert_eq!(bus.memory[ENTRY as usize], 0x91);
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
}

#[test]
fn device_write_recovers_rejection_after_decode_eviction() {
    let (mut cpu, _bus, key) = rejected_after_decode_eviction();
    cpu.note_device_memory_write_range(ENTRY, 1);

    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
}

// ---- G1 SMC heat demotion gate ----

#[test]
fn smc_heat_pre_compile_gate_demotes_a_hot_entry_chunk() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42, 0xf4]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);
    cpu.set_jit_auto_admit(true);
    let key = jit::direct::key_for(&cpu, ENTRY, true).expect("fixture key");
    // Heat the entry 16-byte chunk past the churn threshold within epoch 0 (sync first so a
    // reset pending from setup is observed before the seed, not after).
    cpu.sync_smc_heat();
    for _ in 0..jit::direct::SMC_HEAT_THRESHOLD {
        cpu.jit_direct.smc_heat.bump(ENTRY, 1, 0);
    }
    let attempts = cpu.perf_counters().jit_direct_compile_attempts;
    let installed = cpu.perf_counters().jit_direct_blocks_installed;
    let demotions = cpu.perf_counters().smc_heat_demotions;
    for _ in 0..4 {
        cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
            .expect("gate");
    }
    // The cheap entry-chunk gate refuses admission before a compile is even attempted.
    assert_eq!(
        cpu.perf_counters().jit_direct_compile_attempts,
        attempts,
        "no compile should be paid"
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_blocks_installed,
        installed,
        "the block must not install"
    );
    assert!(cpu.perf_counters().smc_heat_demotions > demotions);
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Rejected
    ));
}

#[test]
fn smc_heat_pre_install_gate_demotes_a_hot_span_after_compiling() {
    let code: Vec<u8> = std::iter::repeat_n(0x40u8, 20).chain([0xf4]).collect();
    let addresses: Vec<u32> = (ENTRY..ENTRY + code.len() as u32).collect();
    // Learn the compiled span so the assertion below is self-checking.
    let (mut probe_cpu, mut probe_bus) = fixture(&code);
    warm(&mut probe_cpu, &mut probe_bus, &addresses);
    let span_len = compiled(jit::direct::compile(&mut probe_cpu, ENTRY, true))
        .span
        .guest_len;
    assert!(
        span_len > 16,
        "block must cross into the second 16-byte chunk (len={span_len})"
    );

    let (mut cpu, mut bus) = fixture(&code);
    warm(&mut cpu, &mut bus, &addresses);
    cpu.set_jit_auto_admit(true);
    let key = jit::direct::key_for(&cpu, ENTRY, true).expect("fixture key");
    // Heat a chunk inside the span but NOT the entry chunk: the cheap pre-compile gate passes and
    // the full-span gate after compilation must catch it before install.
    let far = ENTRY + 16;
    cpu.sync_smc_heat();
    for _ in 0..jit::direct::SMC_HEAT_THRESHOLD {
        cpu.jit_direct.smc_heat.bump(far, 1, 0);
    }
    assert!(
        !cpu.jit_direct.smc_heat.chunk_hot(ENTRY, 0),
        "entry chunk stays cold"
    );
    let attempts = cpu.perf_counters().jit_direct_compile_attempts;
    let installed = cpu.perf_counters().jit_direct_blocks_installed;
    let demotions = cpu.perf_counters().smc_heat_demotions;
    for _ in 0..4 {
        cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
            .expect("gate");
    }
    assert!(
        cpu.perf_counters().jit_direct_compile_attempts > attempts,
        "the block compiles (this is the post-compile gate)"
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_blocks_installed,
        installed,
        "the block must not install"
    );
    assert!(cpu.perf_counters().smc_heat_demotions > demotions);
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Rejected
    ));
}

// ---- G4 non-RAM code admission ----

#[test]
fn g4_admission_refuses_a_page_without_instruction_prefetch_cover() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42, 0xf4]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);
    cpu.set_jit_auto_admit(true);
    let key = jit::direct::key_for(&cpu, ENTRY, true).expect("fixture key");
    // Model a non-RAM code page: the bus serves Data kinds (decode still works) but yields no
    // direct page under InstructionPrefetch. The install-time cover check MUST use
    // InstructionPrefetch, so admission is refused and the block parks Dormant with no install.
    // Switching that check to a Data kind would let this install and fail the assertion below.
    bus.deny_instruction_prefetch_direct_page = true;
    let installed = cpu.perf_counters().jit_direct_blocks_installed;
    for _ in 0..4 {
        cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
            .expect("gate");
    }
    assert_eq!(
        cpu.perf_counters().jit_direct_blocks_installed,
        installed,
        "no install without InstructionPrefetch cover"
    );
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Rejected
    ));

    // A cover-failure Dormant carries no heat stamp, so G1's epoch-aging recovery never lifts it:
    // still parked after a full epoch advance.
    cpu.perf.instructions = 1u64 << jit::direct::SMC_HEAT_EPOCH_SHIFT;
    cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
        .expect("gate");
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Rejected
    ));
    assert_eq!(cpu.perf_counters().jit_direct_blocks_installed, installed);

    // Positive control: restoring the cover installs the identical block, proving the missing
    // InstructionPrefetch page was the only thing that refused admission.
    bus.deny_instruction_prefetch_direct_page = false;
    cpu.jit_direct.clear();
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);
    for _ in 0..4 {
        cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
            .expect("gate");
    }
    assert_eq!(
        cpu.perf_counters().jit_direct_blocks_installed,
        installed + 1,
        "installs once RAM covers the page under InstructionPrefetch"
    );
}

#[test]
fn smc_heat_demoted_key_recovers_when_its_chunk_cools() {
    let (mut cpu, mut bus) = fixture(&[0x40, 0x41, 0x42, 0xf4]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);
    cpu.set_jit_auto_admit(true);
    let key = jit::direct::key_for(&cpu, ENTRY, true).expect("fixture key");
    cpu.sync_smc_heat();
    for _ in 0..jit::direct::SMC_HEAT_THRESHOLD {
        cpu.jit_direct.smc_heat.bump(ENTRY, 1, 0);
    }
    // Demote through the gate: parked Dormant, no install, and probing within the same epoch does
    // NOT lift it (the stamp still reads current).
    let demotions = cpu.perf_counters().smc_heat_demotions;
    let installed = cpu.perf_counters().jit_direct_blocks_installed;
    for _ in 0..4 {
        cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
            .expect("gate");
    }
    assert!(cpu.perf_counters().smc_heat_demotions > demotions);
    assert_eq!(cpu.perf_counters().jit_direct_blocks_installed, installed);
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Rejected
    ));

    // Advance one heat epoch: the next probe path lifts the Dormant (stale stamp), and the normal
    // admission path re-compiles and installs the block.
    cpu.perf.instructions = 1u64 << jit::direct::SMC_HEAT_EPOCH_SHIFT;
    for _ in 0..4 {
        cpu.try_direct_continuation_for_test(&mut bus, ENTRY, true)
            .expect("gate");
    }
    assert_eq!(
        cpu.perf_counters().jit_direct_blocks_installed,
        installed + 1,
        "a cooled chunk re-admits and compiles"
    );
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Ready(_)
    ));
}

// G2+G1 composition: a same-value store churn loop into watched code accrues NO heat (elision
// keeps it out of the invalidation choke entirely), so it can never demote anything.
#[test]
fn same_value_store_churn_accrues_no_heat() {
    const TARGET: u32 = 0x1800;
    const VALUE: u32 = 0xdead_beef;
    let (mut cpu, mut bus) = fixture(&[0x40, 0xf4]);
    bus.memory[TARGET as usize..TARGET as usize + 4].copy_from_slice(&VALUE.to_le_bytes());
    cpu.decode_cache.mark_code_range(TARGET, 4);
    // Baseline after fixture setup (set_mode counts one translation-cache invalidation).
    let invalidations = cpu.perf_counters().code_invalidations;
    for _ in 0..16 {
        cpu.write_memory_sized(
            &mut bus,
            SegmentIndex::Ds,
            TARGET,
            OperandSize::Dword,
            VALUE,
            BusAccessKind::DataWrite,
        )
        .expect("watched same-value store");
    }
    assert_eq!(cpu.perf_counters().smc_heat_chunks_hot, 0);
    assert_eq!(cpu.perf_counters().smc_heat_demotions, 0);
    assert_eq!(cpu.perf_counters().code_invalidations, invalidations);
}

/// A fixture at an arbitrary entry with a FLAT code-segment limit.
///
/// `fresh()` loads CS in real mode, so its limit is 0xFFFF, and the per-slot fetch-limit check
/// in the compile loop refuses any slot whose last byte is above `cs.limit` BEFORE `classify`
/// runs. A high-entry case built on the ordinary `fixture()` would therefore be refused for a
/// reason that has nothing to do with the branch, and every assertion about the branch would
/// pass vacuously. That is the shape a design review caught before these were written.
fn flat_fixture(entry: u32, code: &[u8]) -> (CpuGsw, TestBus) {
    let mut memory = vec![0; 0x1_1000];
    memory[entry as usize..entry as usize + code.len()].copy_from_slice(code);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    let mut cpu = fresh();
    for segment in [SegmentIndex::Cs, SegmentIndex::Ss] {
        let mut descriptor = cpu.registers.segment(segment);
        descriptor.limit = u32::MAX;
        cpu.registers.set_segment(segment, descriptor);
    }
    cpu.registers.eip = entry;
    cpu.jit_direct.set_fast_map_enabled_for_test(true);
    cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0, BusAccessKind::DataRead)
        .expect("initialize direct map");
    (cpu, bus)
}

/// `66 0F 8x` in 32-bit code decodes at Word operand size, and the Word allowlist admits it.
///
/// Nothing else in the tree has a 66-prefixed Jcc, and nothing pins the Word allowlist as a
/// closed set, so this test and the short-form one below carry the whole allowlist argument: if
/// either range is dropped the entry instruction stops classifying, the block has no slots, and
/// `compiled()` panics on a `Retry`.
#[test]
fn a_word_operand_size_near_jcc_is_lowered() {
    // 66 0f 85 10 00: jnz +0x10 at Word operand size, five bytes.
    let (mut cpu, mut bus) = flat_fixture(ENTRY, &[0x66, 0x0f, 0x85, 0x10, 0x00]);
    warm(&mut cpu, &mut bus, &[ENTRY]);

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(compilation.span.instructions, 1);
    assert_eq!(compilation.span.guest_len, 5);
}

/// The short form, `66 7x`, is the other half of the allowlist edit. The two-byte range and the
/// one-byte range are separate `matches!` arms and separate classifier arms, so dropping either
/// leaves the other passing.
#[test]
fn a_word_operand_size_short_jcc_is_lowered() {
    // 66 75 10: jnz +0x10 at Word operand size, three bytes.
    let (mut cpu, mut bus) = flat_fixture(ENTRY, &[0x66, 0x75, 0x10]);
    warm(&mut cpu, &mut bus, &[ENTRY]);

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(compilation.span.instructions, 1);
    assert_eq!(compilation.span.guest_len, 3);
}

/// A Word-size branch whose target crosses the 16-bit wrap must NOT be lowered, because
/// `relative_jump` masks it to 16 bits while the emitted form bakes an unmasked delta.
///
/// The two halves run the SAME instruction stream and differ only in the entry address, which is
/// what makes the negative attributable: without the low-entry control the high-entry assertion
/// would also hold if the block simply failed to form for an unrelated reason.
///
/// The three `inc` fillers are load-bearing. The compile loop returns early on
/// `slots.len() < 3 && !last.is_terminal()`, so with fewer of them the refused case would come
/// back as `Retry` rather than as a shorter block and there would be nothing to assert.
#[test]
fn a_word_jcc_above_the_wrap_is_refused_while_the_same_block_below_it_compiles() {
    // inc eax; inc ecx; inc edx; 66 0f 85 10 00 (jnz +0x10 at Word operand size).
    const CODE: [u8; 8] = [0x40, 0x41, 0x42, 0x66, 0x0f, 0x85, 0x10, 0x00];
    const HIGH: u32 = 0x1_0100;

    // Below the wrap: the branch is lowered and the block is all four instructions.
    let (mut cpu, mut bus) = flat_fixture(ENTRY, &CODE);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );
    let low = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(low.span.instructions, 4, "control: the branch must lower");
    assert_eq!(low.span.guest_len, 8);
    // The two mechanism counters are the slice's PRIMARY gate on the pinned corpus, so pin them
    // here rather than shipping an instrument nothing checks.
    let low_perf = cpu.perf_counters();
    assert_eq!(low_perf.jit_direct_word_control_admitted, 1);
    assert_eq!(low_perf.jit_direct_word_control_refused, 0);

    // Above the wrap: the target is 0x1_0118, the architectural target is 0x0118, and the block
    // stops at the three fillers.
    let (mut cpu, mut bus) = flat_fixture(HIGH, &CODE);
    warm(&mut cpu, &mut bus, &[HIGH, HIGH + 1, HIGH + 2, HIGH + 3]);
    let high = compiled(jit::direct::compile(&mut cpu, HIGH, true));
    assert_eq!(
        high.span.instructions, 3,
        "the Word branch must not be lowered above the 16-bit wrap"
    );
    assert_eq!(high.span.guest_len, 3);
    let high_perf = cpu.perf_counters();
    assert_eq!(high_perf.jit_direct_word_control_admitted, 0);
    assert_eq!(high_perf.jit_direct_word_control_refused, 1);
}

/// The same block at Dword operand size MUST still compile above 0xFFFF. Every other Jcc
/// fixture in this crate entries at 0x100, 0x101 or 0x500, so a clamp wrongly applied at Dword
/// would pass all of them and reach the pinned corpus undetected.
///
/// This is an end-to-end confirmation, not the primary catcher: the predicate itself is already
/// pinned by `the_word_control_clamp_is_a_no_op_at_a_real_mode_limit`.
#[test]
fn a_dword_jcc_above_the_wrap_still_compiles() {
    // inc eax; inc ecx; inc edx; 0f 85 10 00 00 00 (jnz +0x10 at Dword operand size).
    const CODE: [u8; 9] = [0x40, 0x41, 0x42, 0x0f, 0x85, 0x10, 0x00, 0x00, 0x00];
    const HIGH: u32 = 0x1_0100;

    let (mut cpu, mut bus) = flat_fixture(HIGH, &CODE);
    warm(&mut cpu, &mut bus, &[HIGH, HIGH + 1, HIGH + 2, HIGH + 3]);
    let compilation = compiled(jit::direct::compile(&mut cpu, HIGH, true));
    assert_eq!(compilation.span.instructions, 4);
    assert_eq!(compilation.span.guest_len, 9);
}

/// A 16-bit stack under a 32-bit code segment, which is a reachable configuration TODAY and is
/// what makes this slice testable before 16-bit admission is flipped.
///
/// `fresh()` already leaves SS at `default_size_32 == false`, so the only thing owed is a stack
/// pointer inside the fixture's memory. The high half of ESP is deliberately non-zero: a
/// 16-bit stack must preserve ESP[31:16], and it must also mask the effective address, so both
/// mechanisms are load-bearing in every test built on this.
fn sixteen_bit_stack_fixture(entry: u32, code: &[u8]) -> (CpuGsw, TestBus) {
    let mut memory = vec![0; 0x2000];
    memory[entry as usize..entry as usize + code.len()].copy_from_slice(code);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    let mut cpu = fresh();
    let mut ss = cpu.registers.segment(SegmentIndex::Ss);
    ss.default_size_32 = false;
    cpu.registers.set_segment(SegmentIndex::Ss, ss);
    cpu.registers.set_esp(0x1234_0800);
    cpu.registers.eip = entry;
    cpu.jit_direct.set_fast_map_enabled_for_test(true);
    cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0, BusAccessKind::DataRead)
        .expect("initialize direct map");
    (cpu, bus)
}

/// THE ANTI-VACUITY GATE for the 16-bit stack slice.
///
/// A design review found that leaving the old `uses_stack() && !stack_is_32bit()` stop in place
/// alongside the new mapping would refuse every `Push16`, so the slice would do nothing while
/// every counter stayed identical and the pre-registered gate passed. Counter identity cannot
/// tell a correct inert slice from a broken one. This test can: it fails if the push is not
/// admitted, for any reason.
#[test]
fn a_word_push_on_a_sixteen_bit_stack_enters_the_block() {
    // inc eax; inc ecx; 66 50 (push ax at Word operand size).
    let (mut cpu, mut bus) = sixteen_bit_stack_fixture(ENTRY, &[0x40, 0x41, 0x66, 0x50]);
    warm(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        compilation.span.instructions, 3,
        "the Word push must be admitted on a 16-bit stack"
    );
    assert_eq!(compilation.span.guest_len, 4);
    // It is a WORD store, not a dword one. A width mix-up here would be invisible to the
    // instruction count and would misreport the bus split.
    assert_eq!(compilation.word_stores, 1);
    assert_eq!(compilation.dword_stores, 0);
}

/// Matrix row 2: a Word push on a THIRTY-TWO bit stack must not be admitted, because the
/// shipped `Push` kind would write four bytes and decrement four where the guest moves two.
/// Reachable today through a 66-prefixed push in 32-bit code.
///
/// The 16-bit-stack control beside it is what makes the negative attributable: without it the
/// assertion would also hold if the push simply failed to classify.
#[test]
fn a_word_push_on_a_thirty_two_bit_stack_is_refused_but_admitted_on_a_sixteen_bit_one() {
    // inc eax; inc ecx; inc edx; 66 50 (push ax at Word operand size).
    const CODE: [u8; 5] = [0x40, 0x41, 0x42, 0x66, 0x50];
    const WARM: [u32; 4] = [ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3];

    // 32-bit stack: the three fillers compile, the push does not.
    let (mut cpu, mut bus) = sixteen_bit_stack_fixture(ENTRY, &CODE);
    let mut ss = cpu.registers.segment(SegmentIndex::Ss);
    ss.default_size_32 = true;
    cpu.registers.set_segment(SegmentIndex::Ss, ss);
    warm(&mut cpu, &mut bus, &WARM);
    let wide = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(
        wide.span.instructions, 3,
        "a two-byte push must not be lowered by the four-byte kind"
    );
    assert_eq!(wide.span.guest_len, 3);

    // 16-bit stack, same bytes: the push IS admitted.
    let (mut cpu, mut bus) = sixteen_bit_stack_fixture(ENTRY, &CODE);
    warm(&mut cpu, &mut bus, &WARM);
    let narrow = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(narrow.span.instructions, 4, "control: the push must lower");
    assert_eq!(narrow.span.guest_len, 5);
}

/// Matrix row 4: a Dword push on a 16-bit stack stays refused. Four bytes on a 16-bit stack
/// pointer is a form this slice does not build, and it must not fall through to either kind.
#[test]
fn a_dword_push_on_a_sixteen_bit_stack_is_still_refused() {
    // inc eax; inc ecx; inc edx; 50 (push eax at Dword operand size).
    let (mut cpu, mut bus) = sixteen_bit_stack_fixture(ENTRY, &[0x40, 0x41, 0x42, 0x50]);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );

    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));
    assert_eq!(compilation.span.instructions, 3);
    assert_eq!(compilation.span.guest_len, 3);
}
