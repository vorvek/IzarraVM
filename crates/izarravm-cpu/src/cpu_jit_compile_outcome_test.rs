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

fn reject(cache: &mut jit::direct::BlockCache, key: jit::direct::BlockKey, len: usize) {
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
        let mut cache = jit::direct::BlockCache::default();
        let key = jit::direct::BlockKey::new(0x200, 0x320, 7);
        reject(&mut cache, key, 4);
        assert_eq!(cache.invalidate_physical_range(0x320 + offset, 1), 1);
        assert!(!cache.range_hits_compiled_code(0x320, 4));
    }

    let mut cache = jit::direct::BlockCache::default();
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
    let mut cache = jit::direct::BlockCache::default();
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
    let mut cache = jit::direct::BlockCache::default();
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
