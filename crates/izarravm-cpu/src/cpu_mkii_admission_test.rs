// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[cfg(not(any(feature = "int-trace", feature = "timing-class-histogram")))]
#[test]
fn mkii_adjacent_regions_retire_legacy_pending_before_admission() {
    let code = [0x90, 0x90, 0x90, 0x90, 0x90, 0xb8, 0x34, 0x12];
    for adjacent in [false, true] {
        let (mut cpu, mut bus) = fixture(&code);
        enable_session(&mut bus, false);
        bus.mkii_native_session = true;
        cpu.check_mkii_adjacent_pending_for_test(&mut bus, adjacent);
    }
}

pub(super) fn enable_session(bus: &mut TestBus, indirect: bool) {
    enable_inert_regions(bus);
    bus.mkii_session_ledger = true;
    bus.mkii_session_indirect_isa = indirect;
    bus.batch_bus_scale = (33, 105);
}

pub(super) fn compare_session_run(
    cpu: &mut CpuGsw,
    bus: &mut TestBus,
    oracle: &mut CpuGsw,
    other: &mut TestBus,
    cap: u64,
) {
    let bus_start = other.in_batch_scaled_bus_clocks();
    let actual = cpu.run_budgeted(bus, cap);
    let mut core = 0;
    loop {
        let could_take = oracle.can_take_interrupt();
        match oracle.cycle_no_interrupt_check_at_prefix(other, None, core) {
            Ok(outcome) => {
                core += outcome.core_clocks;
                if outcome.halted
                    || other.requires_step_break()
                    || (!could_take && oracle.can_take_interrupt())
                    || core + other.in_batch_scaled_bus_clocks() - bus_start >= cap
                {
                    assert_eq!(actual.unwrap().consumed_core_clocks, core);
                    break;
                }
            }
            Err(mut error) => {
                error.consumed_core_clocks += core;
                assert_eq!(actual.unwrap_err(), error);
                break;
            }
        }
    }
    assert_pair_state(cpu, bus, oracle, other);
    assert_eq!(
        bus.mkii_session_isa_clocks(),
        other.mkii_session_isa_clocks()
    );
    assert_eq!(bus.io_reads, other.io_reads);
    assert_eq!(bus.io_writes, other.io_writes);
}

#[test]
fn mkii_native_admission_observes_helper_service_and_faults() {
    let code = [
        0x90, 0xbb, 0x22, 0x11, 0xec, 0xb8, 0x44, 0x33, 0x90, 0xe6, 0x60,
    ];
    for indirect in [false, true] {
        for fail in [false, true] {
            for remainder in 0..12 {
                let (mut cpu, mut bus) = fixture(&code);
                let (mut oracle, mut other) = fixture(&code);
                for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
                    enable_session(bus, indirect);
                    bus.io_read_fails = fail;
                    bus.io_read_value = Some(0xa5);
                    cpu.registers.set_edx(0x3da);
                    cpu.registers.set_eax(0xaabb0000);
                    warm_code(cpu, bus, code.len() as u32);
                    cpu.timing_rem = remainder;
                }
                bus.mkii_native_session = true;
                assert!(bus.mkii_bus_session().is_some());
                compare_session_run(&mut cpu, &mut bus, &mut oracle, &mut other, 100);
                assert_eq!(cpu.dynarec_mkii_stats().native_admissions, 1);
                assert_eq!(cpu.registers.ebx(), 0x1122);
                assert_eq!(
                    cpu.registers.eax(),
                    0xaabb0000 | if fail { 0 } else { 0xa5 }
                );
                assert_eq!(bus.mkii_session_isa_clocks(), if fail { 0 } else { 2 });
                assert_eq!(bus.io_reads.len(), usize::from(!fail));
                assert!(bus.io_writes.is_empty());
            }
        }
    }
}

#[test]
fn mkii_session_ledger_charges_the_selected_lane_once() {
    for indirect in [false, true] {
        let (_, mut bus) = fixture(&[]);
        enable_session(&mut bus, indirect);
        bus.mkii_exact_fetch_projection = false;
        bus.mkii_session_trace_origin = 11;
        bus.trace.add_elapsed_clocks(16);
        bus.mkii_session_isa = if indirect { 71 } else { 3 };
        *bus.mkii_session_boxed_isa = if indirect { 3 } else { 71 };
        bus.lazy_io_reads = true;
        bus.io_read_value = Some(0xa5);
        assert_eq!(bus.in_batch_raw_bus_clocks(), 8);
        assert_eq!(bus.in_batch_scaled_bus_clocks(), 3);
        assert_eq!(bus.jit_projected_batch_scaled_bus_clocks(2), Some(4));
        assert_eq!(bus.read_io(0x3da, BusWidth::Byte, 7, true), Ok(0xa5));
        assert_eq!(bus.in_batch_raw_bus_clocks(), 10);
        assert_eq!(bus.in_batch_scaled_bus_clocks(), 4);
        assert_eq!(bus.jit_projected_batch_scaled_bus_clocks(0), Some(4));
        assert_eq!(bus.trace.elapsed_clocks(), 16);
        assert_eq!(bus.mkii_session_isa_clocks(), 5);
        assert_eq!(
            if indirect {
                bus.mkii_session_isa
            } else {
                *bus.mkii_session_boxed_isa
            },
            71,
        );
        assert_eq!(bus.io_reads, [(0x3da, 7)]);
        assert!(!bus.requires_step_break());
    }
}

#[test]
fn mkii_native_admission_matches_deadlines_after_live_isa_charges() {
    let programs: [&[u8]; 2] = [
        &[0xec, 0x90, 0xb8, 1, 0, 0x05, 2, 0, 0x90, 0xe6, 0x60],
        &[
            0xec, 0x90, 0xb8, 1, 0, 0x03, 0x06, 0, 0x20, 0x90, 0xe6, 0x60,
        ],
    ];
    for indirect in [false, true] {
        for (program, code) in programs.into_iter().enumerate() {
            for charged_helper in [false, true] {
                let mut admissions = 0;
                for remainder in 0..12 {
                    for cap in (0..24).chain([100]) {
                        let (mut cpu, mut bus) = fixture(code);
                        let (mut oracle, mut other) = fixture(code);
                        for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
                            enable_session(bus, indirect);
                            bus.lazy_io_reads = true;
                            bus.io_read_value = Some(0xa5);
                            cpu.registers.set_edx(0x3da);
                            warm_read(cpu, bus, 0x2000);
                            warm_code(cpu, bus, code.len() as u32);
                            bus.trace.add_elapsed_clocks(177);
                            bus.mkii_session_trace_origin = bus.trace.elapsed_clocks() - 5;
                            bus.mkii_session_isa = 3;
                            *bus.mkii_session_boxed_isa = 3;
                            cpu.timing_rem = remainder;
                            cpu.set_eip(u32::from(!charged_helper));
                        }
                        bus.mkii_native_session = true;
                        assert!(bus.mkii_bus_session().is_some());
                        assert!(other.mkii_bus_session().is_none());
                        compare_session_run(&mut cpu, &mut bus, &mut oracle, &mut other, cap);
                        let count = cpu.dynarec_mkii_stats().native_admissions;
                        admissions += count;
                        if cap == 100 {
                            assert_eq!(
                                count, 1,
                                "indirect={indirect} program={program} helper={charged_helper} rem={remainder}"
                            );
                        }
                    }
                }
                assert!(admissions > 0);
            }
        }
    }
}
