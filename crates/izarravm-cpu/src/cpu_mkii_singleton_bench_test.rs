// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
#[ignore = "release-only paired transition benchmark"]
fn mkii_singleton_mov_paired_native_loop_timing() {
    assert!(
        !std::hint::black_box(cfg!(debug_assertions)),
        "run with --release"
    );
    const CAP: u64 = 1_000_000;
    let code = [0xb8, 0x34, 0x12, 0xeb, 0xfb];
    let setup = || {
        let (mut cpu, mut bus) = fixture(&code);
        admission::enable_session(&mut bus, false);
        bus.mkii_native_session = true;
        cpu.registers.set_eax(0xabcd_5678);
        cpu.registers.eflags = 2;
        warm_code(&mut cpu, &mut bus, code.len() as u32);
        (cpu, bus)
    };
    let (mut baseline, mut old_bus) = setup();
    let (mut candidate, mut new_bus) = setup();
    let (mut oracle, mut other) = setup();
    let mut old_bench = baseline.prepare_mkii_singleton_loop_for_test(&mut old_bus, true);
    let mut new_bench = candidate.prepare_mkii_singleton_loop_for_test(&mut new_bus, false);
    let oracle_core = oracle.elapsed_clocks;
    let oracle_retired = oracle.perf.instructions;
    let oracle_monitor = oracle.perf.monitor_resident_core_clocks;
    let raw = other.in_batch_raw_bus_clocks();
    let scaled = other.in_batch_scaled_bus_clocks();
    let isa = other.mkii_session_isa_clocks();
    let mut core = 0;
    loop {
        let outcome = oracle
            .cycle_no_interrupt_check_at_prefix(&mut other, None, core)
            .unwrap();
        assert!(!outcome.halted);
        core += outcome.core_clocks;
        if core + other.in_batch_scaled_bus_clocks() - scaled >= CAP {
            break;
        }
    }
    assert_eq!(other.in_batch_raw_bus_clocks(), raw);
    assert_eq!(other.in_batch_scaled_bus_clocks(), scaled);
    assert_eq!(other.mkii_session_isa_clocks(), isa);
    assert!(matches!(oracle.registers.eip, 0 | 3));
    let expected_core = oracle.elapsed_clocks - oracle_core;
    let expected_retired = oracle.perf.instructions - oracle_retired;
    let expected_monitor = oracle.perf.monitor_resident_core_clocks - oracle_monitor;
    assert_eq!(core, expected_core);
    let mut sample = |new: bool, pair: usize, warmup: bool| {
        let (cpu, bus, bench) = if new {
            (&mut candidate, &mut new_bus, &mut new_bench)
        } else {
            (&mut baseline, &mut old_bus, &mut old_bench)
        };
        let elapsed_core = cpu.elapsed_clocks;
        let retired = cpu.perf.instructions;
        let monitor = cpu.perf.monitor_resident_core_clocks;
        let raw = bus.in_batch_raw_bus_clocks();
        let scaled = bus.in_batch_scaled_bus_clocks();
        let isa = bus.mkii_session_isa_clocks();
        let (elapsed, stats) = bench.run(cpu, bus, CAP);
        assert_eq!(cpu.registers, oracle.registers);
        assert_eq!(cpu.pending_flags, oracle.pending_flags);
        assert_eq!(cpu.timing_rem, oracle.timing_rem);
        assert_eq!(cpu.elapsed_clocks - elapsed_core, expected_core);
        assert_eq!(cpu.perf.instructions - retired, expected_retired);
        assert_eq!(
            cpu.perf.monitor_resident_core_clocks - monitor,
            expected_monitor
        );
        assert_eq!(bus.in_batch_raw_bus_clocks(), raw);
        assert_eq!(bus.in_batch_scaled_bus_clocks(), scaled);
        assert_eq!(bus.mkii_session_isa_clocks(), isa);
        assert_eq!(bus.memory, other.memory);
        assert_eq!(stats.helpers, expected_retired / 2);
        assert_eq!(stats.native, expected_retired.div_ceil(2));
        assert_eq!(stats.native_admissions > 0, new);
        println!(
            "{}",
            serde_json::json!({
                "role": if new { "candidate" } else { "legacy" },
                "pair": pair,
                "warmup": warmup,
                "elapsed_ns": u64::try_from(elapsed.as_nanos()).unwrap(),
                "retired": expected_retired,
                "core_clocks": expected_core,
                "end_eip": cpu.registers.eip,
                "native": stats.native,
                "helpers": stats.helpers,
                "native_admissions": stats.native_admissions,
                "regions": stats.regions,
                "probe_hits": stats.probe_hits,
                "probe_misses": stats.probe_misses,
                "cold": stats.cold,
                "mismatches": stats.mismatches,
                "trace_bytes": bench.bytes,
                "callbacks": "real_unwrapped"
            })
        );
    };
    sample(false, 0, true);
    sample(true, 0, true);
    for pair in 0..10 {
        for new in if pair % 2 == 0 {
            [false, true]
        } else {
            [true, false]
        } {
            sample(new, pair, false);
        }
    }
}
