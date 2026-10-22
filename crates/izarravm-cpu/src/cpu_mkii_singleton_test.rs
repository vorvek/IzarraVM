// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[path = "cpu_mkii_singleton_bench_test.rs"]
mod bench;

fn singleton_fixture() -> (CpuGsw, TestBus) {
    singleton_fixture_code(&[0xb8, 0x34, 0x12, 0xf4])
}

fn singleton_fixture_code(code: &[u8]) -> (CpuGsw, TestBus) {
    let (mut cpu, mut bus) = fixture(code);
    admission::enable_session(&mut bus, false);
    bus.mkii_native_session = true;
    cpu.registers.set_eax(0xabcd_5678);
    warm_code(&mut cpu, &mut bus, (code.len() - 1) as u32);
    (cpu, bus)
}

#[test]
fn mkii_singleton_mov_matches_instruction_at_caps_and_remainders() {
    for remainder in [0, 1, 5, 11] {
        for pending in [false, true] {
            for carry in [false, true] {
                for cap in [0, 1, 2, 3, 4, 10, 100] {
                    let (mut candidate, mut bus) = singleton_fixture();
                    let (mut oracle, mut other) = singleton_fixture();
                    for cpu in [&mut candidate, &mut oracle] {
                        cpu.timing_rem = remainder;
                        cpu.registers.eflags = 0x8d6 | u32::from(carry);
                        if pending {
                            cpu.pending_flags = PendingFlags {
                                tag: (1 << 31) | (1 << 8),
                                a: if carry { 0xffff } else { 0x7fff },
                                b: 1,
                                result: if carry { 0 } else { 0x8000 },
                            };
                        }
                    }
                    let projected = candidate.preview_scale_clocks(u64::from(
                        candidate
                            .class_table()
                            .raw(crate::timing_class::TimingClass::MovImmReg),
                    ));
                    let (_, stats, noops, _) =
                        candidate.check_mkii_singleton_callbacks_for_test(&mut bus, cap, false);
                    oracle.cycle_no_interrupt_check(&mut other).unwrap();
                    assert_pair_state(&candidate, &bus, &oracle, &other);
                    assert_eq!(
                        bus.mkii_session_isa_clocks(),
                        other.mkii_session_isa_clocks()
                    );
                    assert_eq!(stats.native_admissions, u64::from(projected < cap));
                    assert_eq!(noops, u64::from(projected < cap));
                }
            }
        }
    }
}

#[test]
fn mkii_singleton_mov_settles_real_legacy_prefix_before_completion() {
    let code = [0x90, 0xb8, 0x34, 0x12, 0xf4];
    let (mut baseline, mut old_bus) = singleton_fixture_code(&code);
    let (mut candidate, mut bus) = singleton_fixture_code(&code);
    let (mut oracle, mut other) = singleton_fixture_code(&code);
    let (old_calls, _, _, _) = baseline.check_mkii_singleton_prefix_for_test(&mut old_bus, true);
    let (calls, stats, noops, _) = candidate.check_mkii_singleton_prefix_for_test(&mut bus, false);
    oracle.cycle_no_interrupt_check(&mut other).unwrap();
    oracle.cycle_no_interrupt_check(&mut other).unwrap();
    assert_eq!(old_calls, [2, 1, 0, 0]);
    assert_eq!(calls, [1, 1, 1, 0]);
    assert_eq!(stats.native_admissions, 0);
    assert_eq!(stats.regions, 1);
    assert_eq!(noops, 1);
    assert_pair_state(&candidate, &bus, &baseline, &old_bus);
    assert_pair_state(&candidate, &bus, &oracle, &other);
}

#[test]
fn mkii_singleton_mov_between_helpers_matches_budgeted_oracle() {
    let code = [0xf8, 0xb8, 0x34, 0x12, 0x40, 0xe6, 0x60];
    let mut admissions = 0;
    for remainder in [0, 5, 11] {
        for cap in (0..16).chain([100]) {
            let (mut candidate, mut bus) = fixture(&code);
            let (mut oracle, mut other) = fixture(&code);
            for (cpu, bus) in [(&mut candidate, &mut bus), (&mut oracle, &mut other)] {
                admission::enable_session(bus, false);
                cpu.registers.set_eax(0xabcd_5678);
                warm_code(cpu, bus, code.len() as u32);
                cpu.timing_rem = remainder;
            }
            bus.mkii_native_session = true;
            admission::compare_session_run(&mut candidate, &mut bus, &mut oracle, &mut other, cap);
            let count = candidate.dynarec_mkii_stats().native_admissions;
            admissions += count;
            if cap == 100 {
                assert_eq!(count, 1);
                assert_eq!(candidate.registers.eax(), 0xabcd_1235);
                assert!(!bus.io_writes.is_empty());
            }
        }
    }
    assert!(admissions > 0);
}

#[test]
fn mkii_singleton_mov_rechecks_session_source_and_stop_guards() {
    for guard in 1..=8 {
        let (mut candidate, mut bus) = singleton_fixture();
        let (mut oracle, mut other) = singleton_fixture();
        let (calls, stats, noops, _) =
            candidate.check_mkii_singleton_guard_for_test(&mut bus, guard);
        assert_eq!(stats.native_admissions, 0, "guard={guard}");
        assert_eq!(calls[2], 1, "guard={guard}");
        if matches!(guard, 5 | 6) {
            assert_eq!(calls, [0, 0, 1, 0]);
            assert_eq!(noops, 0);
        } else {
            if guard == 7 {
                oracle.interrupt_shadow = true;
            } else if guard == 8 {
                oracle.profile.enabled = true;
            }
            oracle.cycle_no_interrupt_check(&mut other).unwrap();
        }
        assert_pair_state(&candidate, &bus, &oracle, &other);
    }
}

#[test]
fn mkii_singleton_mov_refuses_stale_debug_latches_without_losing_effects() {
    for (armed, repeat) in [(true, false), (false, true), (true, true)] {
        let (mut candidate, mut bus) = singleton_fixture();
        let (mut oracle, mut other) = singleton_fixture();
        for cpu in [&mut candidate, &mut oracle] {
            cpu.registers.eflags &= !FLAG_TF;
            cpu.single_step_armed = armed;
            cpu.single_step_repeat = repeat;
        }
        let (calls, stats, noops, _) =
            candidate.check_mkii_singleton_callbacks_for_test(&mut bus, 100, false);
        oracle.cycle_no_interrupt_check(&mut other).unwrap();
        assert_eq!(calls, [1, 1, 1, 0]);
        assert_eq!(stats.native_admissions, 0);
        assert_eq!(noops, 0);
        assert!(!candidate.single_step_armed);
        assert!(!candidate.single_step_repeat);
        assert_pair_state(&candidate, &bus, &oracle, &other);
    }
}

#[test]
fn mkii_singleton_mov_does_not_admit_other_pure_forms() {
    for code in [
        &[0x90, 0xf4][..],
        &[0xbb, 0x34, 0x12, 0xf4],
        &[0x66, 0xb8, 0x34, 0x12, 0x78, 0x56, 0xf4],
        &[0x89, 0xc8, 0xf4],
        &[0xb0, 0x12, 0xf4],
        &[0xb4, 0x12, 0xf4],
        &[0x05, 1, 0, 0xf4],
    ] {
        let (mut candidate, mut bus) = singleton_fixture_code(code);
        let (mut oracle, mut other) = singleton_fixture_code(code);
        let (calls, stats, _, _) =
            candidate.check_mkii_singleton_callbacks_for_test(&mut bus, 100, false);
        oracle.cycle_no_interrupt_check(&mut other).unwrap();
        assert_eq!(calls, [1, 1, 0, 0]);
        assert_eq!(stats.native_admissions, 0);
        assert_pair_state(&candidate, &bus, &oracle, &other);
    }
}

#[test]
fn mkii_singleton_mov_keeps_sixteen_bit_sequential_wrap_canonical() {
    let (mut candidate, mut bus) = singleton_fixture();
    let (mut oracle, mut other) = singleton_fixture();
    for (cpu, bus) in [(&mut candidate, &mut bus), (&mut oracle, &mut other)] {
        bus.memory[0xfffd..0x10000].copy_from_slice(&[0xb8, 0x34, 0x12]);
        cpu.set_eip(0xfffd);
        cpu.fetch_decoded(bus, 0xfffd).unwrap();
        cpu.set_eip(0xfffd);
    }
    let (calls, stats, _, _) =
        candidate.check_mkii_singleton_callbacks_for_test(&mut bus, 100, false);
    oracle.cycle_no_interrupt_check(&mut other).unwrap();
    assert_eq!(calls, [1, 1, 0, 0]);
    assert_eq!(stats.native_admissions, 0);
    assert_eq!(candidate.registers.eip, 0);
    assert_pair_state(&candidate, &bus, &oracle, &other);
}

#[cfg(not(any(feature = "int-trace", feature = "timing-class-histogram")))]
#[test]
fn mkii_singleton_mov_removes_real_preparation_callbacks() {
    let (mut baseline, mut old_bus) = singleton_fixture();
    let (mut candidate, mut new_bus) = singleton_fixture();
    let (old_calls, _, old_noops, old_bytes) =
        baseline.check_mkii_singleton_callbacks_for_test(&mut old_bus, 100, true);
    let (new_calls, stats, new_noops, new_bytes) =
        candidate.check_mkii_singleton_callbacks_for_test(&mut new_bus, 100, false);
    assert_eq!(old_calls, [1, 1, 0, 0]);
    assert_eq!(new_calls, [0, 1, 0, 0]);
    assert_eq!(stats.native_admissions, 1);
    assert_eq!((old_noops, new_noops), (0, 1));
    eprintln!("singleton MOV trace bytes: legacy={old_bytes}, candidate={new_bytes}");
    assert_pair_state(&candidate, &new_bus, &baseline, &old_bus);
    assert_eq!(candidate.registers.eax(), 0xabcd_1234);
    assert_eq!(candidate.registers.eip, 3);
    assert_eq!(
        new_bus.mkii_session_isa_clocks(),
        old_bus.mkii_session_isa_clocks()
    );
}
