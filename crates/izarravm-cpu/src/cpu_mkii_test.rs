// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[path = "cpu_mkii_admission_test.rs"]
mod admission;

fn fixture(code: &[u8]) -> (CpuGsw, TestBus) {
    let (mut cpu, mut memory) = real_mode_cpu(code, 65536);
    cpu.set_mode(GswMode::Gsw586);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.set_esp(0x9000);
    memory[0x2000..0x2004].copy_from_slice(&0x12345678u32.to_le_bytes());
    cpu.set_jit_auto_admit(true);
    cpu.set_dynarec_mkii_enabled(true);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    (cpu, bus)
}

#[test]
fn mkii_native_and_helpers_match_instruction_oracle() {
    compare_oracle(false);
    compare_oracle(true);
}

fn compare_oracle(uniform: bool) {
    let code = [
        0xb8, 0xfe, 0xff, 0xb4, 0x7f, 0xb3, 0xe1, 0x00, 0xd8, 0x30, 0xe0, 0x66, 0x05, 0x42, 0x12,
        0x00, 0x80, 0x50, 0x5a, 0x60, 0x61, 0xd1, 0xda, 0x29, 0x16, 0x00, 0x20, 0x85, 0xc0, 0x9c,
        0x59, 0x90, 0xeb, 0xde,
    ];
    let (mut cpu, mut bus) = fixture(&code);
    let (mut oracle, mut other) = fixture(&code);
    for bus in [&mut bus, &mut other] {
        bus.uniform_native_fetches = uniform;
        bus.project_additional_bus_clocks = uniform;
        bus.mkii_exact_fetch_projection = uniform;
        bus.report_batch_clocks = uniform;
        bus.batch_bus_scale = (16, 105);
    }
    oracle.set_dynarec_mkii_enabled(false);
    oracle.set_native_backend_enabled(false);
    for cap in [1, 3, 9, 13, 31, 79, 137].into_iter().cycle().take(120) {
        let before = cpu.perf.instructions;
        let result = cpu.run_budgeted(&mut bus, cap).unwrap();
        let retired = cpu.perf.instructions - before;
        let mut core = 0;
        for _ in 0..retired {
            core += oracle
                .cycle_no_interrupt_check(&mut other)
                .unwrap()
                .core_clocks;
        }
        cpu.materialize_flags();
        oracle.materialize_flags();
        assert_eq!(cpu.registers, oracle.registers);
        assert_eq!(cpu.elapsed_clocks, oracle.elapsed_clocks);
        assert_eq!(result.consumed_core_clocks, core);
        assert_eq!(cpu.timing_rem, oracle.timing_rem);
        assert_eq!(&bus.memory[..], &other.memory[..]);
        assert_eq!(bus.trace.cycles(), other.trace.cycles());
    }
    let stats = cpu.dynarec_mkii_stats();
    assert!(stats.native > 100, "{stats:?}");
    assert!(stats.helpers > 100, "{stats:?}");
    assert_eq!(stats.mismatches, 0);
    if uniform {
        assert!(stats.spans > 0, "{stats:?}");
    }
}

#[test]
fn mkii_cap_matches_an_independently_stopping_oracle() {
    let code = [0xb8, 1, 0, 0xb9, 3, 0, 0x01, 0xc8, 0x90, 0xeb, 0xf5];
    for cap in 0..80 {
        for remainder in [0, 1, 7, 11] {
            let (mut cpu, mut bus) = fixture(&code);
            let (mut oracle, mut other) = fixture(&code);
            for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
                bus.uniform_native_fetches = true;
                bus.mkii_exact_fetch_projection = true;
                bus.report_batch_clocks = true;
                bus.batch_bus_scale = (16, 105);
                warm_code(cpu, bus, code.len() as u32);
                cpu.timing_rem = remainder;
            }
            let bus_start = other.in_batch_scaled_bus_clocks();
            let actual = cpu.run_budgeted(&mut bus, cap).unwrap();
            let mut total = 0;
            loop {
                total += oracle
                    .cycle_no_interrupt_check(&mut other)
                    .unwrap()
                    .core_clocks;
                if total + other.in_batch_scaled_bus_clocks() - bus_start >= cap {
                    break;
                }
            }
            assert_eq!(
                actual.consumed_core_clocks, total,
                "cap={cap} rem={remainder}"
            );
            assert_eq!(
                cpu.perf.instructions, oracle.perf.instructions,
                "cap={cap} rem={remainder}"
            );
            assert_eq!(cpu.registers, oracle.registers);
            assert_eq!(cpu.pending_flags, oracle.pending_flags);
            assert_eq!(bus.trace.cycles(), other.trace.cycles());
        }
    }
}

fn warm_code(cpu: &mut CpuGsw, bus: &mut TestBus, end: u32) {
    while cpu.registers.eip < end {
        let lin = cpu.linear_eip();
        cpu.fetch_decoded(bus, lin).unwrap();
    }
    cpu.set_eip(0);
}

fn enable_read_regions(bus: &mut TestBus) {
    bus.mkii_read_regions = true;
    bus.uniform_native_fetches = true;
    bus.mkii_exact_fetch_projection = true;
    bus.report_batch_clocks = true;
    bus.direct_page_clocks = true;
    bus.batch_bus_scale = (16, 105);
    bus.trace.set_tracing_mode(izarravm_bus::TracingMode::Off);
}

#[test]
fn mkii_trailing_store_matches_oracle() {
    let code = [0x90, 0x90, 0xa3, 0, 0x20, 0xe6, 0x60];
    for inert in [false, true] {
        let (mut cpu, mut bus) = fixture(&code);
        let (mut oracle, mut other) = fixture(&code);
        for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
            if inert {
                enable_inert_regions(bus);
            } else {
                enable_read_regions(bus);
            }
            admission::enable_session(bus, false);
            bus.mkii_native_session = true;
            cpu.registers.set_eax(0x5678);
            cpu.write_memory_bus_width(
                bus,
                SegmentIndex::Ds,
                0x2000,
                BusWidth::Word,
                0,
                BusAccessKind::DataWrite,
            )
            .unwrap();
            cpu.settle_write_record();
            warm_code(cpu, bus, code.len() as u32);
        }
        let a = cpu.run_budgeted(&mut bus, 200).unwrap();
        let b = oracle.run_budgeted(&mut other, 200).unwrap();
        assert_eq!(a.consumed_core_clocks, b.consumed_core_clocks);
        assert_pair_state(&cpu, &bus, &oracle, &other);
        assert_eq!(&bus.memory[0x2000..0x2002], &[0x78, 0x56]);
        assert!(
            cpu.dynarec_mkii_stats().native >= 3 && cpu.dynarec_mkii_stats().native_admissions > 0,
            "{:?}",
            cpu.dynarec_mkii_stats()
        );
    }
}

fn lea_oracle(code: &[u8], setup: impl Fn(&mut CpuGsw, &mut TestBus)) {
    let (mut cpu, mut bus) = fixture(code);
    let (mut oracle, mut other) = fixture(code);
    oracle.set_dynarec_mkii_enabled(false);
    oracle.set_native_backend_enabled(false);
    for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
        admission::enable_session(bus, false);
        bus.mkii_native_session = true;
        setup(cpu, bus);
        warm_code(cpu, bus, code.len() as u32);
    }
    admission::compare_session_run(&mut cpu, &mut bus, &mut oracle, &mut other, 200);
    assert_eq!(cpu.pending_flags, oracle.pending_flags);
}

#[test]
fn mkii_native_lea_matches_interpreter() {
    lea_oracle(&[0x90, 0x8d, 0x07, 0xe6, 0x60], |cpu, _| {
        cpu.registers.set_ebx(0x1234);
    });
    let (mut cpu, mut bus) = fixture(&[0x90, 0x8d, 0x07, 0xe6, 0x60]);
    admission::enable_session(&mut bus, false);
    bus.mkii_native_session = true;
    cpu.registers.set_ebx(0x1234);
    warm_code(&mut cpu, &mut bus, 5);
    cpu.run_budgeted(&mut bus, 200).unwrap();
    let stats = cpu.dynarec_mkii_stats();
    assert!(stats.native >= 2 && stats.native_admissions > 0 && stats.region_guard_misses == 0);
    assert_eq!(cpu.registers.eax() & 0xffff, 0x1234);

    lea_oracle(&[0x8d, 0x07, 0x8d, 0x04, 0xe6, 0x60], |cpu, _| {
        cpu.registers.set_ebx(0x10);
        cpu.registers.set_esi(0x20);
    });
}

#[test]
fn mkii_native_lea_offset_not_linear_and_mod3_faults() {
    lea_oracle(&[0x90, 0x8d, 0x07, 0xe6, 0x60], |cpu, _| {
        cpu.registers.set_ebx(0x5);
        cpu.registers.segments[SegmentIndex::Ds.index()].base = 0x12340000;
        cpu.registers.segments[SegmentIndex::Ds.index()].limit = 0;
    });
    let (mut cpu, mut bus) = fixture(&[0x90, 0x8d, 0x07, 0xe6, 0x60]);
    admission::enable_session(&mut bus, false);
    bus.mkii_native_session = true;
    cpu.registers.set_ebx(0x5);
    cpu.registers.segments[SegmentIndex::Ds.index()].base = 0x12340000;
    cpu.registers.segments[SegmentIndex::Ds.index()].limit = 0;
    warm_code(&mut cpu, &mut bus, 5);
    cpu.run_budgeted(&mut bus, 200).unwrap();
    assert_eq!(cpu.registers.eax() & 0xffff, 0x5);
    assert_eq!(cpu.dynarec_mkii_stats().region_guard_misses, 0);

    let (mut cpu, mut bus) = fixture(&[0x8d, 0xc0, 0xe6, 0x60]);
    let (mut oracle, mut other) = fixture(&[0x8d, 0xc0, 0xe6, 0x60]);
    oracle.set_dynarec_mkii_enabled(false);
    oracle.set_native_backend_enabled(false);
    for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
        enable_read_regions(bus);
        warm_code(cpu, bus, 4);
    }
    compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 50);
    assert_eq!(cpu.dynarec_mkii_stats().regions, 0);
    assert_eq!(cpu.dynarec_mkii_stats().native, 0);
}

#[test]
fn mkii_native_lea_widths_wrap_and_old_base() {
    lea_oracle(&[0x90, 0x66, 0x8d, 0x07, 0xe6, 0x60], |cpu, _| {
        cpu.registers.set_ebx(0x0000_ffff);
        cpu.registers.set_eax(0xabcd_0000);
    });
    lea_oracle(&[0x90, 0x67, 0x8d, 0x03, 0xe6, 0x60], |cpu, _| {
        cpu.registers.set_ebx(0x10001);
    });
    lea_oracle(&[0x90, 0x8d, 0x1f, 0xe6, 0x60], |cpu, _| {
        cpu.registers.set_ebx(0x00aa);
    });
    let (mut cpu, mut bus) = fixture(&[0x90, 0x8d, 0x1f, 0xe6, 0x60]);
    admission::enable_session(&mut bus, false);
    bus.mkii_native_session = true;
    cpu.registers.set_ebx(0x00aa);
    warm_code(&mut cpu, &mut bus, 5);
    cpu.run_budgeted(&mut bus, 200).unwrap();
    assert_eq!(cpu.registers.ebx() & 0xffff, 0x00aa);
}

#[test]
fn mkii_owned_source_replay_survives_decode_eviction_but_rechecks_epochs() {
    let code = [0x90, 0xb8, 0x34, 0x12, 0x90, 0xe4, 0x60];
    for change in 0..5 {
        let (mut cpu, mut bus) = fixture(&code);
        let (mut oracle, mut other) = fixture(&code);
        for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
            enable_read_regions(bus);
            bus.mkii_folded_fetches = true;
            warm_code(cpu, bus, code.len() as u32);
        }
        compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1);
        for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
            cpu.set_eip(0);
            cpu.decode_cache.kill_line_at(1);
            match change {
                0 => {}
                1 => bus.direct_mapping_epoch += 1,
                2 => bus.mkii_folded_fetches = false,
                3 => {
                    bus.memory[2] = 0x78;
                    cpu.note_code_write(2, 1);
                }
                4 => bus.mkii_owned_replay_disabled = true,
                _ => unreachable!(),
            }
        }
        compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1000);
        if change == 0 {
            assert_eq!(cpu.dynarec_mkii_stats().regions, 1);
            assert!(cpu.decode_cache.get_packed(1, false).is_none());
        } else {
            assert_eq!(cpu.dynarec_mkii_stats().regions, 0, "change={change}");
            assert!(cpu.decode_cache.get_packed(1, false).is_some());
        }
    }
}

#[test]
fn mkii_region_pricing_refreshes_after_a_persona_change() {
    let code = [0xb8, 1, 0, 0x03, 0x06, 0, 0x20, 0x90, 0xeb, 0xf6];
    let (mut cpu, mut bus) = fixture(&code);
    let (mut oracle, mut other) = fixture(&code);
    for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
        enable_read_regions(bus);
        warm_read(cpu, bus, 0x2000);
        warm_code(cpu, bus, code.len() as u32);
    }
    for mode in [GswMode::Gsw586, GswMode::Gsw486, GswMode::Gsw586] {
        cpu.set_mode(mode);
        oracle.set_mode(mode);
        compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 120);
    }
    assert!(cpu.dynarec_mkii_stats().regions > 0);
}

fn warm_read(cpu: &mut CpuGsw, bus: &mut TestBus, address: u32) {
    cpu.read_memory_bus_width(
        bus,
        SegmentIndex::Ds,
        address,
        BusWidth::Word,
        BusAccessKind::DataRead,
    )
    .unwrap();
}

#[test]
fn mkii_carry_zero_guard_matches_pending_and_live_flags() {
    let mut flags = vec![
        PendingFlags::default(),
        PendingFlags {
            tag: 3 << 16,
            ..PendingFlags::default()
        },
    ];
    for width in 0..4 {
        let mask = match width {
            0 => 255,
            1 => 65535,
            _ => u32::MAX,
        };
        for operation in [0, 1, 2, 7] {
            for (a, b, result) in [
                (0, 0, 0),
                (mask, 1, 0),
                (0, 1, mask),
                (u32::MAX, u32::MAX, 9),
            ] {
                let pending = PendingFlags {
                    tag: (1 << 31) | (width << 8) | operation,
                    a,
                    b,
                    result,
                };
                flags.extend([
                    pending,
                    pending.with_cf_override(false),
                    pending.with_cf_override(true),
                ]);
            }
        }
    }
    for width in [BusWidth::Byte, BusWidth::Word, BusWidth::Dword] {
        for alu in [2u8, 3] {
            for memory in [false, true] {
                let mut code = Vec::new();
                if width == BusWidth::Dword {
                    code.push(0x66);
                }
                code.extend_from_slice(&[
                    alu * 8 + if width == BusWidth::Byte { 2 } else { 3 },
                    if memory {
                        if width == BusWidth::Byte { 0x26 } else { 0x06 }
                    } else if width == BusWidth::Byte {
                        0xe1
                    } else {
                        0xc1
                    },
                ]);
                if memory {
                    code.extend_from_slice(&[0, 0x20]);
                }
                code.extend_from_slice(&[0x75, 1, 0x90, 0xe4, 0x60]);
                for (index, pending) in flags.iter().enumerate() {
                    // Exhaust the shared guard once; other forms cover both outcomes.
                    if index >= 14 && (width != BusWidth::Word || alu != 2 || !memory) {
                        continue;
                    }
                    let (mut cpu, mut bus) = fixture(&code);
                    let (mut oracle, mut other) = fixture(&code);
                    for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
                        enable_read_regions(bus);
                        warm_read(cpu, bus, 0x2000);
                        warm_code(cpu, bus, code.len() as u32);
                        cpu.registers.set_eax(0x7fff_ff00);
                        cpu.registers.set_ecx(0x8000_0081);
                        cpu.registers.eflags |= FLAG_CF;
                        if index & 1 == 0 {
                            cpu.registers.eflags &= !FLAG_CF;
                        }
                        cpu.pending_flags = *pending;
                    }
                    let carry = cpu.flag(FLAG_CF);
                    compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1000);
                    let stats = cpu.dynarec_mkii_stats();
                    assert_eq!(
                        stats.carry_misses,
                        u64::from(carry),
                        "{pending:?} {width:?} alu={alu}"
                    );
                    assert_eq!(stats.carry_native, u64::from(!carry));
                }
            }
        }
    }
}

#[test]
fn mkii_carry_guard_uses_flags_written_inside_the_region() {
    for initial in [0u16, 0xffff] {
        let [low, high] = initial.to_le_bytes();
        let code = [
            0xb8, low, high, 0x03, 0x06, 0, 0x20, 0x13, 0x06, 0, 0x20, 0x75, 0, 0xe4, 0x60,
        ];
        let (mut cpu, mut bus) = fixture(&code);
        let (mut oracle, mut other) = fixture(&code);
        for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
            enable_read_regions(bus);
            bus.memory[0x2000..0x2002].copy_from_slice(&1u16.to_le_bytes());
            warm_read(cpu, bus, 0x2000);
            warm_code(cpu, bus, code.len() as u32);
        }
        compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1000);
        assert_eq!(
            cpu.dynarec_mkii_stats().carry_misses,
            u64::from(initial == 0xffff)
        );
        assert_eq!(
            cpu.dynarec_mkii_stats().carry_native,
            u64::from(initial == 0)
        );
    }
}

#[test]
fn mkii_carry_regions_match_caps_with_both_carry_values() {
    let code = [0x13, 0x06, 0, 0x20, 0x75, 1, 0x90, 0xe4, 0x60];
    for cap in 0..24 {
        for remainder in [0, 1, 7, 11] {
            for carry in [false, true] {
                let (mut cpu, mut bus) = fixture(&code);
                let (mut oracle, mut other) = fixture(&code);
                for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
                    enable_read_regions(bus);
                    warm_read(cpu, bus, 0x2000);
                    warm_code(cpu, bus, code.len() as u32);
                    cpu.timing_rem = remainder;
                    cpu.set_flag(FLAG_CF, carry);
                }
                compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, cap);
            }
        }
    }
}

#[test]
fn mkii_carry_prefix_survives_a_later_carry_miss_and_fault() {
    for fault in [false, true] {
        let code = [
            0xb8,
            0xff,
            0xff,
            0x13,
            0x06,
            0,
            0x20,
            0x13,
            0x06,
            0,
            if fault { 0x30 } else { 0x20 },
            0x75,
            0,
            0xe4,
            0x60,
        ];
        let (mut cpu, mut bus) = fixture(&code);
        let (mut oracle, mut other) = fixture(&code);
        for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
            enable_read_regions(bus);
            bus.memory[0x2000..0x2002].copy_from_slice(&1u16.to_le_bytes());
            warm_read(cpu, bus, 0x2000);
            warm_code(cpu, bus, code.len() as u32);
            if fault {
                let mut ds = cpu.registers.segment(SegmentIndex::Ds);
                ds.limit = 0x2fff;
                cpu.registers.set_segment(SegmentIndex::Ds, ds);
            }
        }
        compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1000);
        assert_eq!(cpu.dynarec_mkii_stats().carry_native, 1);
        assert_eq!(cpu.dynarec_mkii_stats().carry_misses, 1);
    }
}

#[test]
fn mkii_read_region_uses_live_addresses_and_commits_guard_prefix_once() {
    // MOV BX,2000h; MOV AX,[BX]; MOV BX,AX; MOV DX,[BX]; ADD BX,DX; CMP CX,[BX]; JA; IN.
    let code = [
        0xbb, 0, 0x20, 0x8b, 7, 0x89, 0xc3, 0x8b, 0x17, 0x01, 0xd3, 0x3b, 0x0f, 0x77, 0, 0xe4, 0x60,
    ];
    for cold in [None, Some(0x2000), Some(0x3000), Some(0x4000)] {
        let (mut cpu, mut bus) = fixture(&code);
        let (mut oracle, mut other) = fixture(&code);
        for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
            enable_read_regions(bus);
            bus.memory[0x2000..0x2002].copy_from_slice(&0x3000u16.to_le_bytes());
            bus.memory[0x3000..0x3002].copy_from_slice(&0x1000u16.to_le_bytes());
            for address in [0x2000, 0x3000, 0x4000] {
                warm_read(cpu, bus, address);
            }
            warm_code(cpu, bus, code.len() as u32);
            if let Some(address) = cold {
                cpu.jit_fast_map.invalidate_page(address);
            }
        }
        compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1000);
        assert_eq!(cpu.registers.ebx(), 0x4000);
        let stats = cpu.dynarec_mkii_stats();
        assert!(stats.regions > 0, "{stats:?}");
        assert_eq!(
            stats.region_guard_misses,
            u64::from(cold.is_some()),
            "{cold:?}"
        );
        assert_eq!(stats.cold, 1 + u64::from(cold.is_some()), "{stats:?}");
    }
}

#[test]
fn mkii_read_region_zero_prefix_and_faulting_load_preserve_canonical_effects() {
    for prefix in [false, true] {
        let code: &[u8] = if prefix {
            &[0x8b, 0x06, 0, 0x20, 0x8b, 0x16, 0, 0x30, 0x90, 0xe4, 0x60]
        } else {
            &[0x8b, 0x16, 0, 0x30, 0x90, 0xe4, 0x60]
        };
        for fault in [false, true] {
            let (mut cpu, mut bus) = fixture(code);
            let (mut oracle, mut other) = fixture(code);
            for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
                enable_read_regions(bus);
                warm_read(cpu, bus, 0x2000);
                warm_code(cpu, bus, code.len() as u32);
                if fault {
                    let mut ds = cpu.registers.segment(SegmentIndex::Ds);
                    ds.limit = 0x2fff;
                    cpu.registers.set_segment(SegmentIndex::Ds, ds);
                }
            }
            compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1000);
            let stats = cpu.dynarec_mkii_stats();
            assert_eq!(
                stats.region_guard_misses, 1,
                "prefix={prefix} fault={fault}"
            );
            if fault {
                assert_eq!(stats.native, u64::from(prefix));
                assert_eq!(stats.cold, 1);
            }
        }
    }
}

#[test]
fn mkii_read_region_cap_matches_independent_instruction_boundaries() {
    let code = [
        0xb8, 1, 0, 0x03, 0x06, 0, 0x20, 0x2b, 0x06, 0, 0x20, 0x3b, 0x06, 0, 0x20, 0x77, 1, 0x90,
        0xe4, 0x60,
    ];
    let mut regions = 0;
    for cap in 0..60 {
        for remainder in [0, 1, 7, 11] {
            let (mut cpu, mut bus) = fixture(&code);
            let (mut oracle, mut other) = fixture(&code);
            for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
                enable_read_regions(bus);
                warm_read(cpu, bus, 0x2000);
                warm_code(cpu, bus, code.len() as u32);
                cpu.timing_rem = remainder;
            }
            compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, cap);
            regions += cpu.dynarec_mkii_stats().regions;
        }
    }
    assert!(regions > 0);
}

#[test]
fn mkii_read_region_moffs_high_bytes_and_scaled_addresses_match_oracle() {
    let code = [
        0xa0, 0, 0x20, 0x66, 0xa1, 0, 0x20, 0x8a, 0x26, 1, 0x20, 0x02, 0x26, 2, 0x20, 0x2a, 0x26,
        3, 0x20, 0x67, 0x66, 0x8b, 0x54, 0xb3, 0x10, 0x90, 0xe4, 0x60,
    ];
    let (mut cpu, mut bus) = fixture(&code);
    let (mut oracle, mut other) = fixture(&code);
    for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
        enable_read_regions(bus);
        warm_read(cpu, bus, 0x2000);
        warm_code(cpu, bus, code.len() as u32);
        cpu.registers.set_ebx(0x1fe0);
        cpu.registers.set_esi(4);
    }
    compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1000);
    assert_eq!(cpu.registers.edx(), 0x12345678);
    assert_eq!(cpu.dynarec_mkii_stats().regions, 1);
    assert_eq!(cpu.dynarec_mkii_stats().region_guard_misses, 0);
}

#[test]
fn mkii_memory_compare_branch_matches_all_conditions_and_widths() {
    compare_all_conditions(false, 7);
}

#[test]
fn mkii_read_region_compare_branch_matches_all_conditions_and_widths() {
    compare_all_conditions(true, 7);
}

#[test]
fn mkii_read_region_add_sub_branches_match_all_conditions_and_widths() {
    compare_all_conditions(true, 0);
    compare_all_conditions(true, 5);
}

fn compare_all_conditions(regions: bool, arithmetic: u8) {
    for width in [BusWidth::Byte, BusWidth::Word, BusWidth::Dword] {
        let mask = match width {
            BusWidth::Byte => 255,
            BusWidth::Word => 65535,
            BusWidth::Dword => u32::MAX,
        };
        let sign = (mask >> 1) + 1;
        for condition in 0..16 {
            for (a, b) in [
                (0, 0),
                (0, 1),
                (1, 0),
                (sign, 1),
                (sign - 1, mask),
                (7, 3),
                (sign - 1, 1),
                (sign, mask),
            ] {
                let mut code = Vec::new();
                if width == BusWidth::Dword {
                    code.push(0x66);
                }
                code.extend_from_slice(&[
                    arithmetic * 8 + if width == BusWidth::Byte { 2 } else { 3 },
                    if width == BusWidth::Byte { 0x26 } else { 0x06 },
                    0,
                    0x20,
                    0x70 + condition,
                    1,
                    0x90,
                    0xe4,
                    0x60,
                ]);
                let (mut cpu, mut bus) = fixture(&code);
                let (mut oracle, mut other) = fixture(&code);
                for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
                    bus.uniform_native_fetches = true;
                    bus.mkii_exact_fetch_projection = true;
                    bus.report_batch_clocks = true;
                    bus.direct_page_clocks = true;
                    bus.batch_bus_scale = (16, 105);
                    if regions {
                        enable_read_regions(bus);
                    }
                    bus.memory[0x2000..0x2004].copy_from_slice(&b.to_le_bytes());
                    cpu.read_memory_bus_width(
                        bus,
                        SegmentIndex::Ds,
                        0x2000,
                        width,
                        BusAccessKind::DataRead,
                    )
                    .unwrap();
                    warm_code(cpu, bus, code.len() as u32);
                    cpu.registers
                        .set_eax(if width == BusWidth::Byte { a << 8 } else { a });
                    cpu.prefetch.len = 1;
                    cpu.prefetch.linear_base = 0x8000;
                    cpu.timing_rem = 7;
                }
                let actual = cpu.run_budgeted(&mut bus, 100).unwrap();
                let mut core = 0;
                while !other.requires_step_break() {
                    core += oracle
                        .cycle_no_interrupt_check(&mut other)
                        .unwrap()
                        .core_clocks;
                }
                assert_eq!(
                    cpu.registers, oracle.registers,
                    "{width:?} cc={condition} {a:x} {b:x}"
                );
                assert_eq!(cpu.pending_flags, oracle.pending_flags);
                assert_eq!(cpu.prefetch.len, oracle.prefetch.len);
                assert_eq!(actual.consumed_core_clocks, core);
                assert_eq!(cpu.perf.instructions, oracle.perf.instructions);
                assert_eq!(cpu.timing_rem, oracle.timing_rem);
                assert_eq!(bus.trace.cycles(), other.trace.cycles());
                assert_eq!(bus.trace.elapsed_clocks(), other.trace.elapsed_clocks());
                let stats = cpu.dynarec_mkii_stats();
                assert_eq!(
                    if regions {
                        stats.regions
                    } else {
                        stats.memory_spans
                    },
                    1
                );
            }
        }
    }
}

#[test]
fn mkii_final_helper_settles_pure_work_and_preserves_earlier_taken_pairs() {
    for grant in [false, true] {
        for taken in [false, true] {
            let code = [0x3b, 0x06, 0, 0x20, 0x74, 1, 0x50, 0xee];
            let (mut cpu, mut bus) = fixture(&code);
            let (mut oracle, mut other) = fixture(&code);
            for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
                enable_read_regions(bus);
                bus.mkii_read_regions = grant;
                warm_read(cpu, bus, 0x2000);
                warm_code(cpu, bus, code.len() as u32);
                cpu.registers.set_eax(if taken { 0x5678 } else { 1 });
            }
            compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1000);
            let stats = cpu.dynarec_mkii_stats();
            assert_eq!(stats.entries, 1);
            assert_eq!(stats.native, 2);
            assert_eq!(stats.regions, u64::from(grant));
            assert_eq!(stats.memory_spans, u64::from(!grant));
            assert_eq!(cpu.registers.esp(), if taken { 0x9000 } else { 0x8ffe });
        }
        for helper in [false, true] {
            let mut code = vec![0xb8, 0x34, 0x12, 0x90];
            if helper {
                code.push(0x50);
            }
            code.push(0xee);
            let (mut cpu, mut bus) = fixture(&code);
            let (mut oracle, mut other) = fixture(&code);
            for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
                enable_read_regions(bus);
                bus.mkii_read_regions = grant;
                warm_code(cpu, bus, code.len() as u32);
            }
            compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1000);
            let stats = cpu.dynarec_mkii_stats();
            assert_eq!(stats.entries, 1);
            assert_eq!(stats.native, 2);
            assert_eq!(stats.regions, u64::from(grant));
            assert_eq!(stats.spans, u64::from(!grant));
            assert_eq!(cpu.registers.eax(), 0x1234);
            assert_eq!(cpu.registers.esp(), if helper { 0x8ffe } else { 0x9000 });
        }
    }
}

#[test]
fn mkii_region_continues_through_untaken_branches_and_settles_taken_prefixes() {
    let code = [
        0x3b, 0x06, 0, 0x20, 0x74, 12, 0xbb, 0x11, 0x11, 0x3b, 0x06, 2, 0x20, 0x74, 3, 0xbb, 0x22,
        0x22, 0xe4, 0x60,
    ];
    for (value, native, bx) in [(0, 2, 0x3333), (1, 5, 0x1111), (2, 6, 0x2222)] {
        let (mut cpu, mut bus) = fixture(&code);
        let (mut oracle, mut other) = fixture(&code);
        for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
            enable_read_regions(bus);
            bus.memory[0x2000..0x2004].copy_from_slice(&[0, 0, 1, 0]);
            warm_read(cpu, bus, 0x2000);
            warm_code(cpu, bus, code.len() as u32);
            cpu.registers.set_eax(value);
            cpu.registers.set_ebx(0x3333);
        }
        compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1000);
        let stats = cpu.dynarec_mkii_stats();
        assert_eq!(cpu.registers.ebx(), bx);
        assert_eq!(stats.entries, 1);
        assert_eq!(stats.regions, 1);
        assert_eq!(stats.native, native);
        assert_eq!(stats.region_guard_misses, 0);
        assert_eq!(stats.cold, 1);
    }
}

#[test]
fn mkii_interior_branch_caps_preserve_the_exact_executed_prefix() {
    let code = [
        0x3b, 0x06, 0, 0x20, 0x74, 12, 0xbb, 0x11, 0x11, 0x3b, 0x06, 2, 0x20, 0x74, 3, 0xbb, 0x22,
        0x22, 0xe4, 0x60,
    ];
    let mut taken_legacy_prefixes = 0;
    for cap in 0..40 {
        for remainder in [0, 1, 7, 11] {
            for value in 0..3 {
                let (mut cpu, mut bus) = fixture(&code);
                let (mut oracle, mut other) = fixture(&code);
                for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
                    enable_read_regions(bus);
                    bus.memory[0x2000..0x2004].copy_from_slice(&[0, 0, 1, 0]);
                    warm_read(cpu, bus, 0x2000);
                    warm_code(cpu, bus, code.len() as u32);
                    cpu.registers.set_eax(value);
                    cpu.timing_rem = remainder;
                }
                compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, cap);
                let stats = cpu.dynarec_mkii_stats();
                if value == 0 && stats.regions == 0 && stats.memory_spans == 1 {
                    taken_legacy_prefixes += 1;
                    assert_eq!(stats.native, 2);
                    assert_eq!(cpu.registers.ebx(), 0);
                }
            }
        }
    }
    assert!(taken_legacy_prefixes > 0);
}

#[test]
fn mkii_interior_branch_skips_or_commits_a_later_guard_and_fault() {
    let code = [
        0x3b, 0x06, 0, 0x20, 0x74, 7, 0xbb, 0x11, 0x11, 0x8b, 0x16, 0, 0x30, 0xe4, 0x60,
    ];
    for taken in [false, true] {
        for fault in [false, true] {
            let (mut cpu, mut bus) = fixture(&code);
            let (mut oracle, mut other) = fixture(&code);
            for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
                enable_read_regions(bus);
                warm_read(cpu, bus, 0x2000);
                warm_code(cpu, bus, code.len() as u32);
                cpu.registers.set_eax(if taken { 0x5678 } else { 1 });
                if fault {
                    let mut ds = cpu.registers.segment(SegmentIndex::Ds);
                    ds.limit = 0x2fff;
                    cpu.registers.set_segment(SegmentIndex::Ds, ds);
                }
            }
            compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1000);
            let stats = cpu.dynarec_mkii_stats();
            assert_eq!(stats.regions, 1);
            assert_eq!(stats.region_guard_misses, u64::from(!taken));
            assert_eq!(stats.native, if taken { 2 } else { 3 });
            assert_eq!(cpu.registers.ebx(), if taken { 0 } else { 0x1111 });
            if taken {
                assert_eq!(stats.cold, 1);
                assert_eq!(cpu.perf.instructions, 3);
            }
        }
    }
}

#[test]
fn mkii_taken_backedge_reuses_the_trace_until_fallthrough() {
    let code = [
        0x05, 1, 0, 0x3d, 3, 0, 0x72, 0xf8, 0xbb, 0x11, 0x11, 0xe4, 0x60,
    ];
    let (mut cpu, mut bus) = fixture(&code);
    let (mut oracle, mut other) = fixture(&code);
    for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
        enable_read_regions(bus);
        warm_code(cpu, bus, code.len() as u32);
    }
    compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1000);
    let stats = cpu.dynarec_mkii_stats();
    assert_eq!(cpu.registers.ebx(), 0x1111);
    assert_eq!(stats.entries, 3);
    assert_eq!(stats.regions, 3);
    assert_eq!(stats.native, 10);
    assert_eq!(stats.compiled, 1);
    assert!(stats.dispatch_hits > 0);
}

#[test]
fn mkii_branch_region_refusal_does_not_fault_an_unexecuted_segment_access() {
    let code = [
        0x3b, 0x06, 0, 0x20, 0x74, 5, 0x26, 0x8b, 0x16, 0, 0x30, 0xe4, 0x60,
    ];
    for taken in [false, true] {
        let (mut cpu, mut bus) = fixture(&code);
        let (mut oracle, mut other) = fixture(&code);
        for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
            enable_read_regions(bus);
            warm_read(cpu, bus, 0x2000);
            warm_code(cpu, bus, code.len() as u32);
            cpu.registers.set_eax(if taken { 0x5678 } else { 1 });
            let mut es = cpu.registers.segment(SegmentIndex::Es);
            es.access = 0x98;
            cpu.registers.set_segment(SegmentIndex::Es, es);
        }
        compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1000);
        let stats = cpu.dynarec_mkii_stats();
        assert_eq!(stats.regions, 0);
        assert_eq!(stats.memory_spans, 1);
        assert_eq!(stats.native, 2);
        assert_eq!(cpu.perf.instructions, if taken { 3 } else { 2 });
    }
}

#[test]
fn mkii_zero_displacement_branch_exits_only_when_taken() {
    let cases: &[&[u8]] = &[
        &[
            0x3b, 0x06, 0, 0x20, 0x74, 0, 0xbb, 0x11, 0x11, 0x53, 0x5a, 0xe4, 0x60,
        ],
        &[
            0x74, 0, 0xb8, 0x11, 0x11, 0x50, 0x5b, 0x74, 0, 0xb9, 0x22, 0x22, 0xe4, 0x60,
        ],
    ];
    for (case, code) in cases.iter().enumerate() {
        for regions in [false, true] {
            for taken in [false, true] {
                let (mut cpu, mut bus) = fixture(code);
                let (mut oracle, mut other) = fixture(code);
                for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
                    bus.uniform_native_fetches = true;
                    bus.mkii_exact_fetch_projection = true;
                    bus.report_batch_clocks = true;
                    bus.direct_page_clocks = true;
                    bus.batch_bus_scale = (16, 105);
                    if regions {
                        enable_read_regions(bus);
                    }
                    warm_read(cpu, bus, 0x2000);
                    warm_code(cpu, bus, code.len() as u32);
                    cpu.registers.set_eax(if taken { 0x5678 } else { 1 });
                    cpu.set_flag(FLAG_ZF, taken);
                    cpu.prefetch.len = 1;
                    cpu.prefetch.linear_base = 0x8000;
                }
                compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1000);
                let stats = cpu.dynarec_mkii_stats();
                assert_eq!(stats.entries, if taken { 2 + case as u64 } else { 1 });
                assert_eq!(cpu.prefetch.len, oracle.prefetch.len);
                assert_eq!(cpu.registers.ebx(), 0x1111);
            }
        }
    }
}

#[test]
fn mkii_helper_write_invalidates_a_previously_skipped_fallthrough_tail() {
    let code = [
        0x3b, 0x06, 0, 0x20, 0x74, 8, 0xc6, 0x06, 12, 0, 0x77, 0xbb, 0x11, 0x11, 0xe4, 0x60,
    ];
    let (mut cpu, mut bus) = fixture(&code);
    let (mut oracle, mut other) = fixture(&code);
    for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
        enable_read_regions(bus);
        warm_read(cpu, bus, 0x2000);
        warm_code(cpu, bus, code.len() as u32);
        cpu.registers.set_eax(0x5678);
    }
    compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1000);
    assert_eq!(cpu.registers.ebx(), 0);
    assert_eq!(bus.memory[12], 0x11);
    assert_eq!(cpu.jit_direct.code_watch.refcount(12), 1);
    let before = cpu.dynarec_mkii_stats();
    for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
        bus.io_touched = false;
        cpu.set_eip(0);
        cpu.registers.set_eax(1);
    }
    compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1000);
    assert_eq!(cpu.registers.ebx(), 0x1177);
    let after = cpu.dynarec_mkii_stats();
    assert!(after.invalidations > before.invalidations);
    assert!(after.retired_artifacts > before.retired_artifacts);
    assert!(after.cold > before.cold);
    for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
        bus.io_touched = false;
        cpu.set_eip(11);
        cpu.registers.set_ebx(0);
    }
    compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1000);
    assert_eq!(cpu.registers.ebx(), 0x1177);
    assert!(cpu.dynarec_mkii_stats().compiled > after.compiled);
}

#[test]
fn mkii_memory_pair_cap_matches_independent_instruction_boundaries() {
    let code = [0x3b, 0x06, 0, 0x20, 0x77, 1, 0x90, 0xe4, 0x60];
    for cap in 0..16 {
        for remainder in [0, 1, 7, 11] {
            let (mut cpu, mut bus) = fixture(&code);
            let (mut oracle, mut other) = fixture(&code);
            for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
                bus.uniform_native_fetches = true;
                bus.mkii_exact_fetch_projection = true;
                bus.report_batch_clocks = true;
                bus.direct_page_clocks = true;
                bus.batch_bus_scale = (16, 105);
                cpu.read_memory_bus_width(
                    bus,
                    SegmentIndex::Ds,
                    0x2000,
                    BusWidth::Word,
                    BusAccessKind::DataRead,
                )
                .unwrap();
                warm_code(cpu, bus, code.len() as u32);
                cpu.timing_rem = remainder;
            }
            let bus_start = other.in_batch_scaled_bus_clocks();
            let actual = cpu.run_budgeted(&mut bus, cap).unwrap();
            let mut core = 0;
            loop {
                core += oracle
                    .cycle_no_interrupt_check(&mut other)
                    .unwrap()
                    .core_clocks;
                if other.requires_step_break()
                    || core + other.in_batch_scaled_bus_clocks() - bus_start >= cap
                {
                    break;
                }
            }
            assert_eq!(
                actual.consumed_core_clocks, core,
                "cap={cap} rem={remainder}"
            );
            assert_eq!(cpu.perf.instructions, oracle.perf.instructions);
            assert_eq!(cpu.registers, oracle.registers);
            assert_eq!(cpu.pending_flags, oracle.pending_flags);
            assert_eq!(bus.trace.cycles(), other.trace.cycles());
        }
    }
}

#[test]
fn mkii_helper_patch_replaces_the_next_native_operand() {
    let (mut cpu, mut bus) = fixture(&[
        0xc6, 0x06, 0x06, 0x00, 0x77, 0xb0, 0x11, 0x50, 0x5b, 0xe4, 0x60,
    ]);
    warm_code(&mut cpu, &mut bus, 11);
    cpu.run_budgeted(&mut bus, 100).unwrap();
    assert_eq!(cpu.registers.ebx(), 0x77);
    assert_eq!(cpu.perf.instructions, 5);
    assert!(cpu.dynarec_mkii_stats().invalidations >= 1);
}

#[test]
fn mkii_cold_mismatch_executes_the_fetched_instruction_once_after_a_native_prefix() {
    let code = [0x90, 0xb8, 0x34, 0x12, 0x93, 0xe4, 0x60];
    for replacement in [[0x83, 0xc0, 0x01], [0x8e, 0xd0, 0x90]] {
        let (mut cpu, mut bus) = fixture(&code);
        let (mut oracle, mut other) = fixture(&code);
        for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
            warm_code(cpu, bus, code.len() as u32);
        }
        compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 0);
        assert_eq!(cpu.dynarec_mkii_stats().compiled, 1);
        for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
            // Keep the owned trace to exercise its cold-decode mismatch exit.
            bus.memory[1..4].copy_from_slice(&replacement);
            cpu.decode_cache.kill_line_at(1);
            cpu.set_eip(0);
        }
        compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1000);
        assert_eq!(cpu.dynarec_mkii_stats().mismatches, 1);
        if replacement[0] == 0x83 {
            assert_eq!(cpu.registers.ebx(), 1);
            assert_eq!(cpu.perf.instructions, 5);
        } else {
            assert_eq!(cpu.perf.instructions, 2);
        }
    }
}

#[test]
fn mkii_dispatcher_failure_and_cold_selection_preserve_the_stop_boundary() {
    let code = [0xb8, 0x34, 0x12, 0x90, 0xe4, 0x60];
    for available in [false, true] {
        for warm in [false, true] {
            for cap in [0, 1, 7, 1000] {
                let (mut cpu, mut bus) = fixture(&code);
                let (mut oracle, mut other) = fixture(&code);
                if warm {
                    warm_code(&mut cpu, &mut bus, code.len() as u32);
                    warm_code(&mut oracle, &mut other, code.len() as u32);
                }
                let actual = if available {
                    cpu.fail_mkii_compile_for_test();
                    cpu.run_mkii(&mut bus, cap)
                } else {
                    cpu.run_mkii_without_dispatcher_for_test(&mut bus, cap)
                }
                .unwrap();
                let mut core = 0;
                loop {
                    core += oracle
                        .cycle_no_interrupt_check(&mut other)
                        .unwrap()
                        .core_clocks;
                    if core >= cap || other.requires_step_break() {
                        break;
                    }
                }
                assert_eq!(actual.consumed_core_clocks, core);
                assert_eq!(cpu.perf.instructions, oracle.perf.instructions);
                assert_eq!(cpu.registers, oracle.registers);
                assert_eq!(cpu.elapsed_clocks, oracle.elapsed_clocks);
                assert_eq!(cpu.timing_rem, oracle.timing_rem);
                assert_eq!(bus.trace.cycles(), other.trace.cycles());
                if !available || cap == 0 {
                    assert_eq!(cpu.dynarec_mkii_stats().entries, 0);
                }
            }
        }
    }
}

fn compare_pair_run(
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
        match oracle.cycle_no_interrupt_check(other) {
            Ok(outcome) => {
                core += outcome.core_clocks;
                if outcome.halted
                    || other.requires_step_break()
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
}

fn assert_pair_state(cpu: &CpuGsw, bus: &TestBus, oracle: &CpuGsw, other: &TestBus) {
    assert_eq!(cpu.registers, oracle.registers);
    assert_eq!(cpu.pending_flags, oracle.pending_flags);
    assert_eq!(cpu.perf.instructions, oracle.perf.instructions);
    assert_eq!(cpu.timing_rem, oracle.timing_rem);
    assert_eq!(cpu.elapsed_clocks, oracle.elapsed_clocks);
    assert_eq!(
        cpu.perf.monitor_resident_core_clocks,
        oracle.perf.monitor_resident_core_clocks
    );
    assert_eq!(bus.trace.cycles(), other.trace.cycles());
    assert_eq!(bus.trace.elapsed_clocks(), other.trace.elapsed_clocks());
    assert_eq!(&bus.memory[..], &other.memory[..]);
}

#[test]
fn mkii_memory_pair_revalidates_live_segments_maps_and_observers() {
    compare_live_memory_guards(false, false);
}

#[test]
fn mkii_read_region_revalidates_live_segments_maps_and_observers() {
    compare_live_memory_guards(true, false);
}

#[test]
fn mkii_native_admission_revalidates_live_segments_maps_and_observers() {
    compare_live_memory_guards(true, true);
}

fn compare_live_memory_guards(regions: bool, session: bool) {
    // CMP AX,[BX]; JA +1; NOP; IN AL,60h.
    let code = [0x3b, 0x07, 0x77, 1, 0x90, 0xe4, 0x60];
    for remainder in 0..if session { 12 } else { 1 } {
        for case in 0..14 {
            let (mut cpu, mut bus) = fixture(&code);
            let (mut oracle, mut other) = fixture(&code);
            for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
                bus.uniform_native_fetches = true;
                bus.mkii_exact_fetch_projection = true;
                bus.report_batch_clocks = true;
                bus.direct_page_clocks = true;
                bus.batch_bus_scale = (16, 105);
                if regions {
                    enable_read_regions(bus);
                }
                if session {
                    admission::enable_session(bus, false);
                }
                cpu.timing_rem = remainder;
                cpu.registers.set_ebx(0x2000);
                cpu.read_memory_bus_width(
                    bus,
                    SegmentIndex::Ds,
                    0x2000,
                    BusWidth::Word,
                    BusAccessKind::DataRead,
                )
                .unwrap();
                warm_code(cpu, bus, code.len() as u32);
                let mut ds = cpu.registers.segment(SegmentIndex::Ds);
                match case {
                    0 => {
                        cpu.jit_fast_map.invalidate_page(0x2000);
                    }
                    1 => {
                        cpu.registers.set_ebx(0x2001);
                    }
                    2 => {
                        cpu.registers.set_ebx(0x2fff);
                    }
                    3 => {
                        ds.base = 0x1000;
                        cpu.registers.set_ebx(0x1000);
                    }
                    4 => {
                        ds.limit = 0x1fff;
                    }
                    5 => {
                        ds.access = 0x98;
                    }
                    6 => {
                        ds.access = 0x96;
                        ds.limit = 0x1fff;
                    }
                    7 => {
                        cpu.rmw_census_enabled = true;
                    }
                    8 => {
                        cpu.slot_census_enabled = true;
                    }
                    9 => {
                        cpu.cpl = 3;
                        cpu.control.cr0 |= CR0_AM;
                        cpu.registers.eflags |= FLAG_AC;
                        cpu.recompute_alignment_armed();
                        ds.base = 1;
                        cpu.registers.set_ebx(0x1fff);
                    }
                    10 => {
                        cpu.set_jit_auto_admit(false);
                    }
                    11 | 13 => {
                        let mut page = bus
                            .direct_page(0x2000, BusAccessKind::DataRead)
                            .unwrap()
                            .unwrap();
                        if case == 11 {
                            page.mapping_epoch += 1;
                        } else {
                            cpu.cpl = 3;
                            cpu.registers.eflags |= FLAG_IOPL;
                        }
                        cpu.jit_fast_map.invalidate_page(0x2000);
                        assert!(cpu.jit_fast_map.populate_read(
                            0x2000,
                            0x2000,
                            page,
                            jit::fast_map::PagePermissions {
                                writable: true,
                                user: case != 13
                            },
                            false
                        ));
                    }
                    12 => {
                        bus.direct_mapping_epoch += 1;
                    }
                    _ => unreachable!(),
                }
                cpu.registers.set_segment(SegmentIndex::Ds, ds);
            }
            if session {
                bus.mkii_native_session = true;
                assert!(bus.mkii_bus_session().is_some());
                admission::compare_session_run(&mut cpu, &mut bus, &mut oracle, &mut other, 100);
            } else {
                compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 100);
            }
            let stats = cpu.dynarec_mkii_stats();
            if session {
                assert_eq!(
                    stats.native_admissions,
                    u64::from(!matches!(case, 5 | 7..=10)),
                    "case={case} rem={remainder}"
                );
            }
            if regions {
                assert_eq!(
                    stats.regions,
                    u64::from(!matches!(case, 5 | 7..=10)),
                    "case={case}"
                );
                assert_eq!(
                    stats.region_guard_misses,
                    u64::from(matches!(case, 0..=2 | 4 | 11..=13)),
                    "case={case}"
                );
            } else {
                assert_eq!(
                    stats.memory_spans,
                    u64::from(matches!(case, 3 | 6)),
                    "case={case}"
                );
            }
        }
    }
}

#[test]
fn mkii_memory_pair_address_overrides_and_wrapping_branch_targets() {
    compare_address_overrides(false);
}

#[test]
fn mkii_read_region_address_overrides_and_wrapping_branch_targets() {
    compare_address_overrides(true);
}

fn compare_address_overrides(regions: bool) {
    let cases: &[&[u8]] = &[
        &[0x3b, 0x80, 0x10, 0, 0x77, 1, 0x90, 0xe4, 0x60],
        &[0x67, 0x3b, 0x83, 0x10, 0, 0, 0, 0x77, 1, 0x90, 0xe4, 0x60],
        &[0x26, 0x3b, 0x07, 0x66, 0x77, 1, 0x90, 0xe4, 0x60],
        &[0x3b, 0x07, 0x77, 0xfc],
    ];
    for (case, code) in cases.iter().enumerate() {
        let (mut cpu, mut bus) = fixture(code);
        let (mut oracle, mut other) = fixture(code);
        for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
            bus.uniform_native_fetches = true;
            bus.mkii_exact_fetch_projection = true;
            bus.report_batch_clocks = true;
            bus.direct_page_clocks = true;
            bus.batch_bus_scale = (16, 105);
            if regions {
                enable_read_regions(bus);
            }
            cpu.read_memory_bus_width(
                bus,
                SegmentIndex::Ds,
                0x2000,
                BusWidth::Word,
                BusAccessKind::DataRead,
            )
            .unwrap();
            warm_code(cpu, bus, code.len() as u32);
            cpu.registers.set_eax(0xffff);
            match case {
                0 => {
                    cpu.registers.set_ebx(0xfff0);
                    cpu.registers.set_esi(0x2000);
                }
                1 => {
                    cpu.registers.set_ebx(0x1ff0);
                }
                2 => {
                    let mut es = cpu.registers.segment(SegmentIndex::Es);
                    es.base = 0x1000;
                    cpu.registers.set_segment(SegmentIndex::Es, es);
                    cpu.registers.set_ebx(0x1000);
                }
                3 => {
                    cpu.registers.set_ebx(0x2000);
                }
                _ => unreachable!(),
            }
        }
        compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 100);
        let stats = cpu.dynarec_mkii_stats();
        assert!(
            if regions {
                stats.regions
            } else {
                stats.memory_spans
            } > 0,
            "case={case}"
        );
    }
}

#[test]
fn mkii_memory_pair_preserves_end_of_segment_and_target_faults() {
    compare_segment_end(false);
}

#[test]
fn mkii_read_region_preserves_end_of_segment_and_target_faults() {
    compare_segment_end(true);
}

fn compare_segment_end(regions: bool) {
    for case in 0..4 {
        let code: &[u8] = if case == 3 {
            &[0x3b, 0x06, 0, 0x20, 0x66, 0x77, 2]
        } else {
            &[0x3b, 0x06, 0, 0x20, 0x77, if case == 2 { 0x20 } else { 0 }]
        };
        let (mut cpu, mut bus) = fixture(code);
        let (mut oracle, mut other) = fixture(code);
        for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
            bus.uniform_native_fetches = true;
            bus.mkii_exact_fetch_projection = true;
            bus.report_batch_clocks = true;
            bus.direct_page_clocks = true;
            bus.batch_bus_scale = (16, 105);
            if regions {
                enable_read_regions(bus);
            }
            cpu.read_memory_bus_width(
                bus,
                SegmentIndex::Ds,
                0x2000,
                BusWidth::Word,
                BusAccessKind::DataRead,
            )
            .unwrap();
            let start = if case == 2 {
                0
            } else {
                65536 - code.len() as u32
            };
            bus.memory[start as usize..start as usize + code.len()].copy_from_slice(code);
            cpu.registers.eip = start;
            while cpu.registers.eip < start + code.len() as u32 {
                let lin = cpu.linear_eip();
                cpu.fetch_decoded(bus, lin).unwrap();
            }
            cpu.set_eip(start);
            cpu.registers.set_eax(if case == 0 { 0 } else { 0xffff });
            if case == 2 {
                let mut cs = cpu.registers.cs();
                cs.limit = code.len() as u32 - 1;
                cpu.registers.set_segment(SegmentIndex::Cs, cs);
            }
        }
        compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 8);
        let stats = cpu.dynarec_mkii_stats();
        assert!(
            if regions {
                stats.regions
            } else {
                stats.memory_spans
            } > 0,
            "case={case}"
        );
    }
}

#[test]
fn mkii_trace_grows_across_an_evicted_prefix_without_losing_source_watches() {
    let code = [0x90, 0x3b, 0x06, 0, 0x20, 0x77, 0xf9];
    let (mut cpu, mut bus) = fixture(&code);
    let (mut oracle, mut other) = fixture(&code);
    for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
        bus.uniform_native_fetches = true;
        bus.mkii_exact_fetch_projection = true;
        bus.report_batch_clocks = true;
        bus.direct_page_clocks = true;
        bus.batch_bus_scale = (16, 105);
        cpu.read_memory_bus_width(
            bus,
            SegmentIndex::Ds,
            0x2000,
            BusWidth::Word,
            BusAccessKind::DataRead,
        )
        .unwrap();
        warm_code(cpu, bus, 5);
        cpu.registers.set_eax(0xffff);
    }
    compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1);
    assert_eq!(cpu.dynarec_mkii_stats().compiled, 1);
    assert_eq!(cpu.jit_direct.code_watch.refcount(1), 1);
    assert_eq!(cpu.jit_direct.code_watch.refcount(5), 0);
    for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
        cpu.set_eip(5);
        cpu.fetch_decoded(bus, 5).unwrap();
        cpu.set_eip(0);
        cpu.decode_cache.kill_line_at(1);
    }
    cpu.fail_mkii_compile_for_test();
    compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1);
    assert_eq!(cpu.dynarec_mkii_stats().compiled, 1);
    assert_eq!(cpu.dynarec_mkii_stats().expansions, 0);
    assert_eq!(cpu.jit_direct.code_watch.refcount(1), 1);
    assert_eq!(cpu.jit_direct.code_watch.refcount(5), 0);
    cpu.set_eip(0);
    oracle.set_eip(0);
    compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 100);
    assert_eq!(cpu.dynarec_mkii_stats().expansions, 1);
    assert!(cpu.dynarec_mkii_stats().memory_spans > 0);
    assert_eq!(cpu.jit_direct.code_watch.refcount(1), 1);
    assert_eq!(cpu.jit_direct.code_watch.refcount(5), 1);
    let before = cpu.dynarec_mkii_stats().invalidations;
    for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
        bus.memory[5] = 0x75;
        cpu.note_code_write(5, 1);
        cpu.set_eip(0);
    }
    compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 100);
    assert!(cpu.dynarec_mkii_stats().invalidations > before);
}

#[cfg(feature = "reflected-call-memo")]
#[test]
fn mkii_memory_pair_refuses_live_read_journal() {
    let code = [0x3b, 0x06, 0, 0x20, 0x77, 1, 0x90, 0xe4, 0x60];
    let (mut cpu, mut bus) = fixture(&code);
    let (mut oracle, mut other) = fixture(&code);
    for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
        bus.uniform_native_fetches = true;
        bus.mkii_exact_fetch_projection = true;
        bus.report_batch_clocks = true;
        cpu.read_memory_bus_width(
            bus,
            SegmentIndex::Ds,
            0x2000,
            BusWidth::Word,
            BusAccessKind::DataRead,
        )
        .unwrap();
        warm_code(cpu, bus, code.len() as u32);
        cpu.reflected_call_journal = true;
    }
    compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 100);
    assert_eq!(cpu.dynarec_mkii_stats().memory_spans, 0);
}

#[test]
fn mkii_live_cs_access_change_preserves_progress() {
    let (mut cpu, mut bus) = fixture(&[0x90, 0xeb, 0xfd]);
    warm_code(&mut cpu, &mut bus, 3);
    cpu.run_budgeted(&mut bus, 20).unwrap();
    let before = cpu.perf.instructions;
    let mut cs = cpu.registers.cs();
    cs.access ^= 1;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    cpu.run_budgeted(&mut bus, 20).unwrap();
    assert!(cpu.perf.instructions > before);
}

#[test]
fn mkii_fault_keeps_native_prefix_and_partial_helper_work() {
    let (mut cpu, mut bus) = fixture(&[0xb8, 0x01, 0x00, 0x90, 0xf7, 0xf3]);
    let (mut oracle, mut other) = fixture(&[0xb8, 0x01, 0x00, 0x90, 0xf7, 0xf3]);
    warm_code(&mut cpu, &mut bus, 6);
    warm_code(&mut oracle, &mut other, 6);
    let error = cpu.run_budgeted(&mut bus, 100).unwrap_err();
    let mut prefix = 0;
    let expected = loop {
        match oracle.cycle_no_interrupt_check(&mut other) {
            Ok(outcome) => prefix += outcome.core_clocks,
            Err(mut error) => {
                error.consumed_core_clocks += prefix;
                break error;
            }
        }
    };
    assert_eq!(error, expected);
    assert_eq!(cpu.registers, oracle.registers);
    assert_eq!(cpu.elapsed_clocks, oracle.elapsed_clocks);
    assert_eq!(bus.trace.cycles(), other.trace.cycles());
    assert_eq!(cpu.dynarec_mkii_stats().native, 2);
}

#[test]
fn mkii_shadow_stops_after_exactly_one_successor() {
    for code in [
        &[0xfb, 0x90, 0x90][..],
        &[0x8e, 0xd0, 0x90, 0x90],
        &[0x17, 0x90, 0x90],
    ] {
        let (mut cpu, mut bus) = fixture(code);
        let (mut oracle, mut other) = fixture(code);
        for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
            if code[0] != 0xfb {
                cpu.gdtr = DescriptorTable {
                    base: 0x3000,
                    limit: 15,
                };
                bus.memory[0x3008..0x3010].copy_from_slice(&descriptor(0, 0xffff, 0x93, 0));
                cpu.registers.set_eax(8);
                bus.memory[0x9000..0x9002].copy_from_slice(&8u16.to_le_bytes());
                cpu.registers.eflags |= FLAG_IF;
            }
            warm_code(cpu, bus, code.len() as u32);
        }
        let actual = cpu.run_budgeted(&mut bus, 100).unwrap();
        let core: u64 = (0..2)
            .map(|_| {
                oracle
                    .cycle_no_interrupt_check(&mut other)
                    .unwrap()
                    .core_clocks
            })
            .sum();
        assert_eq!(actual.consumed_core_clocks, core);
        assert_pair_state(&cpu, &bus, &oracle, &other);
        assert_eq!(cpu.registers.eip, code.len() as u32 - 1);
        assert_eq!(cpu.perf.instructions, 2);
        assert!(!cpu.interrupt_shadow);
        assert_eq!(cpu.perf.brk_interrupt, 1);
        assert_eq!(cpu.dynarec_mkii_stats().helpers, 1);
        assert_eq!(cpu.dynarec_mkii_stats().native, 1);
        assert_eq!(cpu.dynarec_mkii_stats().cold, 0);
    }
}

#[test]
fn mkii_warm_push_retirement_preserves_wrap_and_live_accounting() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for limit in [0xffff, 0x1ffff] {
            for cpl in [0, 3] {
                for remainder in [0, 7, 11] {
                    let (mut cpu, mut bus) = fixture(&[]);
                    let (mut oracle, mut other) = fixture(&[]);
                    for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
                        cpu.set_mode(mode);
                        cpu.cpl = cpl;
                        let mut cs = cpu.registers.cs();
                        cs.limit = limit;
                        cs.selector = u16::from(cpl);
                        cs.access = (cs.access & !0x60) | (cpl << 5);
                        cpu.registers.set_segment(SegmentIndex::Cs, cs);
                        let mut ss = cpu.registers.segment(SegmentIndex::Ss);
                        ss.selector = u16::from(cpl);
                        ss.access = (ss.access & !0x60) | (cpl << 5);
                        cpu.registers.set_segment(SegmentIndex::Ss, ss);
                        cpu.registers.set_eax(0x1234);
                        bus.memory[0xffff] = 0x50;
                        cpu.set_eip(0xffff);
                        let lin = cpu.linear_eip();
                        cpu.fetch_decoded(bus, lin).unwrap();
                        cpu.set_eip(0xffff);
                        cpu.timing_rem = remainder;
                    }
                    compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 0);
                    assert_eq!(cpu.registers.eip, if limit == 0xffff { 0 } else { 0x10000 });
                    assert_eq!(cpu.registers.esp(), 0x8ffe);
                    assert_eq!(cpu.perf.instructions, 1);
                    assert_eq!(cpu.dynarec_mkii_stats().helpers, 1);
                    assert_eq!(cpu.dynarec_mkii_stats().cold, 0);
                    assert_eq!(
                        cpu.perf.monitor_resident_core_clocks,
                        if cpl == 0 { cpu.elapsed_clocks } else { 0 }
                    );
                }
            }
        }
    }
}

#[test]
fn mkii_warm_popf_stops_before_the_suffix_when_if_or_tf_changes() {
    let code = [0x9d, 0x43, 0xe4, 0x60];
    for flags in [FLAG_IF, FLAG_TF] {
        let (mut cpu, mut bus) = fixture(&code);
        let (mut oracle, mut other) = fixture(&code);
        for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
            bus.memory[0x9000..0x9002]
                .copy_from_slice(&((2 | flags | FLAG_CF) as u16).to_le_bytes());
            warm_code(cpu, bus, code.len() as u32);
            cpu.timing_rem = 7;
        }
        let actual = cpu.run_budgeted(&mut bus, 100).unwrap();
        let expected = oracle.cycle_no_interrupt_check(&mut other).unwrap();
        assert_eq!(actual.consumed_core_clocks, expected.core_clocks);
        assert_pair_state(&cpu, &bus, &oracle, &other);
        assert_eq!(cpu.registers.eip, 1);
        assert_eq!(cpu.registers.esp(), 0x9002);
        assert_eq!(cpu.registers.ebx(), 0);
        assert_eq!(cpu.perf.instructions, 1);
        assert_eq!(cpu.dynarec_mkii_stats().helpers, 1);
        assert_eq!(cpu.dynarec_mkii_stats().cold, 0);
        assert_eq!(cpu.perf.brk_interrupt, u64::from(flags == FLAG_IF));
    }
}

#[test]
fn mkii_cold_port_observes_prior_native_core() {
    let (mut cpu, mut bus) = fixture(&[0x90, 0x90, 0xe4, 0x60]);
    let mut warm = cpu.clone();
    for _ in 0..2 {
        let lin = warm.linear_eip();
        let insn = warm.fetch_decoded(&mut bus, lin).unwrap();
        let _ = cpu.decode_cache.put(lin, insn, false, lin);
    }
    cpu.run_budgeted(&mut bus, 100).unwrap();
    assert_eq!(cpu.perf.instructions, 3);
    assert_eq!(bus.last_read_io_core_clocks_so_far, Some(2));
    assert_eq!(cpu.dynarec_mkii_stats().native, 2);
}

struct PollCommitBus {
    inner: TestBus,
    requests: Vec<izarravm_bus::CalloutPollSkipRequest>,
}

impl CpuBus for PollCommitBus {
    fn read_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<u32, BusError> {
        self.inner.read_memory(address, width, kind)
    }
    fn write_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        value: u32,
        kind: BusAccessKind,
    ) -> Result<(), BusError> {
        self.inner.write_memory(address, width, value, kind)
    }
    fn prefetch_memory(&mut self, address: u32, out: &mut [u8]) -> Result<usize, BusError> {
        self.inner.prefetch_memory(address, out)
    }
    fn charge_instruction_fetch(&mut self, address: u32) -> Result<(), BusError> {
        self.inner.charge_instruction_fetch(address)
    }
    fn jit_preflight_cached_fetch(&self, linear: u32, physical: u32, len: u8) -> Option<u64> {
        self.inner.jit_preflight_cached_fetch(linear, physical, len)
    }
    fn read_io(
        &mut self,
        port: u16,
        width: BusWidth,
        core: u64,
        ring0: bool,
    ) -> Result<u32, BusError> {
        self.inner.read_io(port, width, core, ring0)
    }
    fn write_io(
        &mut self,
        port: u16,
        width: BusWidth,
        value: u32,
        core: u64,
        ring0: bool,
    ) -> Result<(), BusError> {
        self.inner.write_io(port, width, value, core, ring0)
    }
    fn interrupt_acknowledge(&mut self, vector: u8, ax: u16) -> Result<(), BusError> {
        self.inner.interrupt_acknowledge(vector, ax)
    }
    fn callout_poll_skip(
        &mut self,
        request: &izarravm_bus::CalloutPollSkipRequest,
    ) -> Result<izarravm_bus::CalloutPollSkipOutcome, izarravm_bus::CalloutPollDecline> {
        self.requests.push(*request);
        let raw = request.raw_core_clocks * 3;
        let now = request.core_clocks_at_block_entry
            + ((request.prefix_raw + raw) * u64::from(request.core_num) + request.timing_rem)
                / u64::from(request.core_den);
        assert!(now + 100 < request.cap);
        self.inner.trace.add_elapsed_clocks(9);
        Ok(izarravm_bus::CalloutPollSkipOutcome {
            iterations: 3,
            skipped_raw_core_clocks: raw,
            committed_raw_bus_clocks: 9,
            now_after: now,
        })
    }
}

#[test]
fn mkii_poll_committed_time_and_pending_flags_survive_a_real_in_fault() {
    for fail in [false, true] {
        for remainder in [0, 1, 7, 11] {
            let (mut cpu, mut inner) = fixture(&[0x90, 0xec, 0xa8, 8, 0x75, 0xfb]);
            warm_code(&mut cpu, &mut inner, 6);
            cpu.registers.set_edx(0x3da);
            cpu.registers.set_eax(0xaabb_ccdd);
            cpu.pending_flags = PendingFlags {
                tag: (1 << 31) | (1 << 16) | (1 << 17),
                a: 0x7f,
                b: 1,
                result: 0x80,
            };
            cpu.timing_rem = remainder;
            cpu.set_direct_poll_skip_override(Some(true));
            cpu.set_direct_poll_skip_16_override(Some(true));
            inner.io_read_fails = fail;
            inner.io_read_value = Some(0x18);
            let mut bus = PollCommitBus {
                inner,
                requests: Vec::new(),
            };
            let pending = cpu.pending_flags;
            let flags = cpu.registers.eflags;
            let before = cpu.elapsed_clocks;
            let raw_before = bus.inner.trace.elapsed_clocks();
            let result = cpu.run_budgeted(&mut bus, 10_000);
            assert_eq!(bus.requests.len(), 1);
            let request = bus.requests[0];
            assert_eq!(request.prefix_raw, 0);
            assert_eq!(request.core_clocks_at_block_entry, 1);
            let skipped_raw = request.raw_core_clocks * 3;
            let in_raw = if fail {
                0
            } else {
                u64::from(cpu.port_io_core_clocks(false))
            };
            let expected = (12 + skipped_raw + in_raw + remainder) / 12;
            assert_eq!(cpu.pending_flags, pending);
            assert_eq!(cpu.registers.eflags, flags);
            assert_eq!(cpu.elapsed_clocks - before, expected);
            assert_eq!(cpu.timing_rem, (skipped_raw + in_raw + remainder) % 12);
            assert_eq!(cpu.perf.instructions, if fail { 1 } else { 2 });
            assert_eq!(
                bus.inner.trace.elapsed_clocks() - raw_before,
                13 + 2 * u64::from(!fail)
            );
            if fail {
                assert_eq!(result.unwrap_err().consumed_core_clocks, expected);
                assert_eq!(cpu.registers.eax(), 0xaabb_ccdd);
            } else {
                assert_eq!(result.unwrap().consumed_core_clocks, expected);
                assert_eq!(cpu.registers.eax(), 0xaabb_cc18);
                assert_eq!(cpu.registers.eip, 2);
                assert_eq!(cpu.perf.brk_step, 1);
            }
        }
    }
}

fn enable_inert_regions(bus: &mut TestBus) {
    enable_read_regions(bus);
    bus.mkii_inert_regions = true;
    bus.mkii_folded_fetches = true;
    bus.direct_page_clocks = false;
}

#[test]
fn mkii_logical_regions_feed_native_carry_guards() {
    let code = [
        0x0b, 0xc1, 0x11, 0xd3, 0x85, 0xc0, 0x19, 0xca, 0x90, 0xe6, 0x60,
    ];
    for inert in [false, true] {
        for carry in [false, true] {
            for remainder in 0..12 {
                let (mut cpu, mut bus) = fixture(&code);
                let (mut oracle, mut other) = fixture(&code);
                for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
                    if inert {
                        enable_inert_regions(bus);
                    } else {
                        enable_read_regions(bus);
                    }
                    warm_code(cpu, bus, code.len() as u32);
                    cpu.registers.set_eax(0xaabb8000);
                    cpu.registers.set_ebx(0xccdd0004);
                    cpu.registers.set_ecx(0x11220001);
                    cpu.registers.set_edx(0x33440002);
                    cpu.registers.eflags = FLAG_AF | (u32::from(carry) * FLAG_CF);
                    cpu.timing_rem = remainder;
                }
                compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1000);
                let stats = cpu.dynarec_mkii_stats();
                assert_eq!(stats.regions, 1);
                assert_eq!(stats.native, 5);
                assert_eq!(stats.carry_native, 2);
                assert_eq!(stats.carry_misses, 0);
                assert_eq!(cpu.registers.eax(), 0xaabb8001);
                assert_eq!(cpu.registers.ebx(), 0xccdd0006);
                assert_eq!(cpu.registers.edx(), 0x33440001);
            }
        }
    }
}

#[test]
fn mkii_logical_poll_tests_refill_cold_tails() {
    use crate::jit::block::{PollScanOutcome, build_poll_loop_from};

    for test in [[0xa8, 8], [0x84, 0xe0]] {
        let code = [0xec, test[0], test[1], 0x75, 0xfb, 0xe6, 0x60];
        let (mut cpu, mut bus) = fixture(&code);
        enable_inert_regions(&mut bus);
        bus.lazy_io_reads = true;
        bus.io_read_value = Some(0);
        cpu.registers.set_eax(0x800);
        cpu.registers.set_edx(0x3da);
        cpu.set_direct_poll_skip_override(Some(true));
        cpu.set_direct_poll_skip_16_override(Some(true));
        warm_code(&mut cpu, &mut bus, code.len() as u32);
        assert!(matches!(
            build_poll_loop_from(&cpu, 0, true),
            PollScanOutcome::Found(_)
        ));
        cpu.run_budgeted(&mut bus, 0).unwrap();
        assert_eq!(cpu.registers.eip, 1);
        let compiled = cpu.dynarec_mkii_stats().compiled;
        let retired = cpu.dynarec_mkii_stats().retired_artifacts;
        assert_eq!(compiled, 1);
        cpu.decode_cache.kill_line_at(1);
        cpu.decode_cache.kill_line_at(3);
        cpu.set_eip(0);
        cpu.run_budgeted(&mut bus, 0).unwrap();
        assert_eq!(cpu.registers.eip, 1);
        assert!(cpu.decode_cache.poll_negative_live(0, false));
        assert!(cpu.decode_cache.get_packed(1, false).is_none());
        assert!(cpu.decode_cache.get_packed(3, false).is_none());
        assert!(cpu.decode_cache.get_packed(5, false).is_some());
        let before = cpu.dynarec_mkii_stats();
        let instructions = cpu.perf.instructions;
        cpu.set_eip(0);
        cpu.run_budgeted(&mut bus, 1000).unwrap();
        assert!(cpu.decode_cache.get_packed(1, false).is_some(), "{test:x?}");
        assert!(cpu.decode_cache.get_packed(3, false).is_some(), "{test:x?}");
        assert!(!cpu.decode_cache.poll_negative_live(0, false), "{test:x?}");
        assert!(matches!(
            build_poll_loop_from(&cpu, 0, true),
            PollScanOutcome::Found(_)
        ));
        let after = cpu.dynarec_mkii_stats();
        assert_eq!(after.compiled, compiled);
        assert_eq!(after.retired_artifacts, retired);
        assert_eq!(
            after.helpers - before.helpers,
            if test[0] == 0xa8 { 1 } else { 3 },
            "test={test:x?} eip={} instructions={} before={before:?} after={after:?}",
            cpu.registers.eip,
            cpu.perf.instructions - instructions
        );
        assert_eq!(
            after.cold - before.cold,
            if test[0] == 0xa8 { 3 } else { 1 }
        );
        assert_eq!(after.native - before.native, 0);
        assert_eq!(cpu.perf.instructions - instructions, 4);
        assert_eq!(cpu.registers.eip, 7);
    }
}

#[test]
fn mkii_inert_completion_matches_all_remainders_caps_and_privilege_levels() {
    let code = [0x90, 0xb8, 1, 0, 0x03, 0x06, 0, 0x20, 0x90, 0xe4, 0x60];
    for cpl in 0..4 {
        for remainder in 0..12 {
            for cap in [0, 1, 2, 3, 4, 5, 6, 7, 8, 12, 40] {
                let (mut cpu, mut bus) = fixture(&code);
                let (mut oracle, mut other) = fixture(&code);
                for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
                    enable_inert_regions(bus);
                    cpu.cpl = cpl;
                    cpu.registers.eflags |= 3 << 12;
                    cpu.registers.segments[SegmentIndex::Ds.index()].access = 0x93 | (cpl << 5);
                    warm_read(cpu, bus, 0x2000);
                    warm_code(cpu, bus, code.len() as u32);
                    bus.trace.add_elapsed_clocks(177);
                    cpu.elapsed_clocks = 71;
                    cpu.timing_rem = remainder;
                    assert!(bus.certify_inert_read_region().is_some());
                }
                compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, cap);
                if cap == 40 {
                    assert!(cpu.dynarec_mkii_stats().regions > 0);
                    assert!(cpu.dynarec_mkii_stats().native >= 4);
                    assert_eq!(cpu.perf.monitor_resident_core_clocks != 0, cpl == 0);
                }
            }
        }
    }
}

#[test]
fn mkii_inert_completion_preserves_taken_and_faulting_prefixes() {
    for taken in [false, true] {
        for valid_read in [false, true] {
            for carry in [false, true] {
                for remainder in 0..12 {
                    let code = [
                        0x90, 0x13, 0x06, 0, 0x20, 0x75, 4, 0x8b, 0x06, 0, 0x30, 0xe4, 0x60,
                    ];
                    let (mut cpu, mut bus) = fixture(&code);
                    let (mut oracle, mut other) = fixture(&code);
                    for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
                        enable_inert_regions(bus);
                        bus.memory[0x2000..0x2002].copy_from_slice(&u16::from(taken).to_le_bytes());
                        cpu.registers.set_eax(0);
                        warm_read(cpu, bus, 0x2000);
                        if valid_read {
                            warm_read(cpu, bus, 0x3000);
                        } else {
                            cpu.registers.segments[SegmentIndex::Ds.index()].limit = 0x2fff;
                        }
                        warm_code(cpu, bus, code.len() as u32);
                        cpu.set_flag(FLAG_CF, carry);
                        cpu.timing_rem = remainder;
                    }
                    compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1000);
                    assert!(cpu.dynarec_mkii_stats().regions > 0);
                    if carry {
                        assert!(cpu.dynarec_mkii_stats().carry_misses > 0);
                    }
                }
            }
        }
    }
}

#[test]
fn mkii_inert_grants_refuse_observers_faults_and_inexact_prior_bus_totals() {
    for refusal in 0..10 {
        let (cpu, mut bus) = fixture(&[0x90, 0x90]);
        enable_inert_regions(&mut bus);
        bus.trace.add_elapsed_clocks(177);
        let grant = bus.certify_inert_read_region().unwrap();
        assert_eq!(grant.scaled_bus_clocks(), bus.in_batch_scaled_bus_clocks());
        assert_eq!(
            grant.epochs(),
            (bus.direct_mapping_epoch, bus.jit_cost_dial_epoch())
        );
        match refusal {
            0 => bus.code_fetch_observations = Some(Vec::new()),
            1 => bus.core_events = Some(Vec::new()),
            2 => bus.fail_fetch_charge_at = Some(0),
            3 => bus.fail_instruction_prefetch_direct_page = true,
            4 => bus.direct_page_clocks = true,
            5 => bus.mkii_folded_fetches = false,
            6 => bus.native_aggregate_accounting_disabled = true,
            7 => bus.trace.set_tracing_mode(TracingMode::Full),
            8 => bus.mkii_owned_replay_disabled = true,
            9 => bus.mkii_exact_fetch_projection = false,
            _ => unreachable!(),
        }
        assert!(
            bus.certify_inert_read_region().is_none(),
            "refusal={refusal}"
        );
        if refusal < 4 {
            assert!(bus.begin_read_region().is_none());
            assert!(bus.owned_code_replay_epochs().is_none());
            assert!(bus.certify_owned_code_span(0, 0, 2).is_none());
            assert!(
                bus.jit_preflight_cached_fetch(cpu.linear_eip(), 0, 1)
                    .is_none()
            );
        }
    }
}

#[test]
fn mkii_inert_admission_includes_a_prior_helper_bus_charge() {
    let code = [
        0x50, 0x90, 0xb8, 1, 0, 0x03, 0x06, 0, 0x20, 0x90, 0xe4, 0x60,
    ];
    for remainder in 0..12 {
        for cap in 0..24 {
            let (mut cpu, mut bus) = fixture(&code);
            let (mut oracle, mut other) = fixture(&code);
            for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
                enable_inert_regions(bus);
                bus.direct_write_denied_page = Some(0x8000);
                warm_read(cpu, bus, 0x2000);
                warm_code(cpu, bus, code.len() as u32);
                bus.trace.add_elapsed_clocks(177);
                cpu.timing_rem = remainder;
            }
            let before = bus.trace.elapsed_clocks();
            compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, cap);
            assert!(bus.trace.elapsed_clocks() > before);
            if cap == 23 {
                assert_eq!(cpu.dynarec_mkii_stats().regions, 1);
                assert_eq!(cpu.dynarec_mkii_stats().native, 4);
                assert_eq!(cpu.registers.esp(), 0x8ffe);
            }
        }
    }
}

#[test]
fn mkii_inert_zero_prefix_guard_replays_the_unexecuted_instruction() {
    let code = [0x13, 0x06, 0, 0x20, 0x75, 0, 0xe4, 0x60];
    let (mut cpu, mut bus) = fixture(&code);
    let (mut oracle, mut other) = fixture(&code);
    for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
        enable_inert_regions(bus);
        warm_read(cpu, bus, 0x2000);
        warm_code(cpu, bus, code.len() as u32);
        cpu.set_flag(FLAG_CF, true);
        cpu.timing_rem = 11;
    }
    compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1000);
    assert_eq!(cpu.dynarec_mkii_stats().regions, 1);
    assert_eq!(cpu.dynarec_mkii_stats().carry_misses, 1);
    assert_eq!(cpu.dynarec_mkii_stats().native, 0);
}

#[test]
fn mkii_inert_endpoint_at_10000_uses_canonical_wrap_settlement() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for limit in [0xffff, 0x1ffff] {
            for remainder in 0..12 {
                let (mut cpu, mut bus) = fixture(&[0xe4, 0x60]);
                let (mut oracle, mut other) = fixture(&[0xe4, 0x60]);
                for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
                    cpu.set_mode(mode);
                    enable_inert_regions(bus);
                    cpu.registers.segments[SegmentIndex::Cs.index()].limit = limit;
                    bus.memory[0xfffc..0x10000].copy_from_slice(&[0xb8, 0x34, 0x12, 0x90]);
                    for eip in [0xfffc, 0xffff] {
                        cpu.set_eip(eip);
                        cpu.fetch_decoded(bus, eip).unwrap();
                    }
                    cpu.set_eip(0xfffc);
                    cpu.prefetch.len = 1;
                    cpu.prefetch.linear_base = 0x8000;
                    cpu.timing_rem = remainder;
                }
                compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1000);
                assert_eq!(cpu.dynarec_mkii_stats().regions, 1);
                assert_eq!(cpu.dynarec_mkii_stats().native, 2);
                assert_eq!(cpu.registers.eip, if limit == 0xffff { 2 } else { 0x10000 });
                assert_eq!(cpu.prefetch.len, oracle.prefetch.len);
                assert_eq!(cpu.prefetch.linear_base, oracle.prefetch.linear_base);
            }
        }
    }
}

#[test]
fn mkii_logical_registers_preserve_pending_auxiliary_and_raw_flags() {
    for inert in [false, true] {
        let mut prior = vec![
            PendingFlags::default(),
            PendingFlags {
                tag: 3 << 16,
                a: 9,
                b: 7,
                result: 0x10,
            },
        ];
        for width in 0..3 {
            for op in [0, 1, 2, 7] {
                for af in [false, true] {
                    let (a, b, result) = match (op, af) {
                        (0, true) => (15, 1, 16),
                        (1, true) => (16, 1, 15),
                        (0, false) => (1, 1, 2),
                        (1, false) => (2, 1, 1),
                        (_, true) => (1, 2, 16),
                        (_, false) => (1, 2, 0),
                    };
                    let pending = PendingFlags {
                        tag: (1 << 31) | (width << 8) | op,
                        a,
                        b,
                        result,
                    };
                    prior.extend([
                        pending,
                        pending.with_cf_override(false),
                        pending.with_cf_override(true),
                    ]);
                }
            }
        }
        for width in [BusWidth::Byte, BusWidth::Word, BusWidth::Dword] {
            for op in [1u8, 4, 6] {
                let mut code = Vec::new();
                if width == BusWidth::Dword {
                    code.push(0x66);
                }
                code.extend_from_slice(&[
                    op * 8 + if width == BusWidth::Byte { 2 } else { 3 },
                    if width == BusWidth::Byte { 0xfd } else { 0xd9 },
                    0x90,
                    0xe6,
                    0x60,
                ]);
                for (index, pending) in prior.iter().enumerate() {
                    let (mut cpu, mut bus) = fixture(&code);
                    let (mut oracle, mut other) = fixture(&code);
                    for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
                        if inert {
                            enable_inert_regions(bus);
                        } else {
                            enable_read_regions(bus);
                        }
                        warm_code(cpu, bus, code.len() as u32);
                        cpu.registers.set_ebx(0xa55a00f0);
                        cpu.registers.set_ecx(0xdeadb50f);
                        cpu.registers.eflags =
                            FLAG_CF | FLAG_OF | FLAG_ZF | FLAG_SF | FLAG_PF | FLAG_DF;
                        if index & 1 != 0 {
                            cpu.registers.eflags |= FLAG_AF;
                        }
                        cpu.pending_flags = *pending;
                    }
                    let af = cpu.flag(FLAG_AF);
                    compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1000);
                    let stats = cpu.dynarec_mkii_stats();
                    assert_eq!(stats.regions, 1);
                    assert_eq!(stats.native, 2);
                    assert_eq!(stats.helpers, 0);
                    assert_eq!(cpu.pending_flags.a, 0);
                    assert_eq!(cpu.pending_flags.b, 0);
                    assert_eq!(cpu.pending_flags.cf_override(), None);
                    assert_eq!(cpu.pending_flags.op(), LazyFlagOp::Logic);
                    assert_eq!(cpu.pending_flags.width(), width);
                    assert_eq!(cpu.flag(FLAG_AF), af, "{pending:?} {width:?} op={op}");
                    assert!(!cpu.flag(FLAG_CF));
                    assert!(!cpu.flag(FLAG_OF));
                    cpu.materialize_flags();
                    oracle.materialize_flags();
                    assert_eq!(cpu.registers, oracle.registers);
                }
            }
        }
    }
}

#[test]
fn mkii_logical_operand_forms_and_test_no_write_match_oracle() {
    for inert in [false, true] {
        let forms: &[&[u8]] = &[
            &[0x08, 0xec],
            &[0x09, 0xcb],
            &[0x0a, 0xe5],
            &[0x0b, 0xd9],
            &[0x0c, 0x80],
            &[0x66, 0x0d, 0x80, 0, 0, 0x80],
            &[0x20, 0xec],
            &[0x21, 0xcb],
            &[0x22, 0xe5],
            &[0x23, 0xd9],
            &[0x24, 0x0f],
            &[0x25, 0x0f, 0xf0],
            &[0x30, 0xec],
            &[0x31, 0xcb],
            &[0x32, 0xe5],
            &[0x33, 0xd9],
            &[0x34, 0x80],
            &[0x35, 0, 0x80],
            &[0x80, 0xcc, 0x80],
            &[0x81, 0xe3, 0x0f, 0xf0],
            &[0x82, 0xf5, 0xff],
            &[0x83, 0xe3, 0x80],
            &[0x66, 0x83, 0xcb, 0x80],
            &[0x66, 0x81, 0xf3, 0, 0, 0, 0x80],
            &[0x84, 0xec],
            &[0x84, 0xe0],
            &[0x85, 0xcb],
            &[0x66, 0x85, 0xcb],
            &[0xa8, 0],
            &[0xa9, 0, 0],
            &[0x66, 0xa9, 0, 0, 0, 0],
        ];
        for form in forms {
            for grant in [false, true] {
                let mut code = form.to_vec();
                code.extend_from_slice(&[0x90, 0xe6, 0x60]);
                let (mut cpu, mut bus) = fixture(&code);
                let (mut oracle, mut other) = fixture(&code);
                for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
                    if inert {
                        enable_inert_regions(bus);
                    } else {
                        enable_read_regions(bus);
                    }
                    bus.mkii_read_regions = grant;
                    bus.mkii_inert_regions &= grant;
                    cpu.registers.set_eax(0xabcd1234);
                    cpu.registers.set_ebx(0xa55a00f0);
                    cpu.registers.set_ecx(0xdeadb50f);
                    warm_code(cpu, bus, code.len() as u32);
                }
                let before = cpu.registers.clone();
                compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1000);
                let opcode = form[usize::from(form[0] == 0x66)];
                let helper = opcode == 0x84 && form.last() == Some(&0xe0);
                let canonical = opcode == 0xa8 || helper;
                assert_eq!(
                    cpu.dynarec_mkii_stats().native,
                    2 - u64::from(canonical),
                    "{form:x?}"
                );
                assert_eq!(cpu.dynarec_mkii_stats().helpers, u64::from(helper));
                assert_eq!(
                    cpu.dynarec_mkii_stats().regions,
                    u64::from(grant && !canonical)
                );
                if matches!(opcode, 0x84 | 0x85 | 0xa8 | 0xa9) {
                    assert_eq!(cpu.registers.eax(), before.eax());
                    assert_eq!(cpu.registers.ebx(), before.ebx());
                    assert_eq!(cpu.registers.ecx(), before.ecx());
                    if opcode >= 0xa8 {
                        assert_eq!(cpu.pending_flags.result, 0);
                    }
                }
            }
        }
    }
}

#[test]
fn mkii_logical_sequence_accepts_native_and_helper_flag_producers() {
    for inert in [false, true] {
        for producer in [
            &[0x05, 1, 0][..],
            &[0xd1, 0xe0],
            &[0xd1, 0xd0],
            &[0xd3, 0xe0],
        ] {
            let mut code = producer.to_vec();
            code.extend_from_slice(&[0x0b, 0xd9, 0x33, 0xc0, 0x90, 0xe6, 0x60]);
            let (mut cpu, mut bus) = fixture(&code);
            let (mut oracle, mut other) = fixture(&code);
            for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
                if inert {
                    enable_inert_regions(bus);
                } else {
                    enable_read_regions(bus);
                }
                cpu.registers.set_eax(15);
                cpu.registers.set_ecx(0);
                cpu.registers.eflags |= FLAG_AF | FLAG_CF;
                warm_code(cpu, bus, code.len() as u32);
            }
            compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1000);
            assert_eq!(
                cpu.dynarec_mkii_stats().native,
                if producer[0] == 0x05 { 4 } else { 3 }
            );
            assert_eq!(cpu.registers.eax(), 0);
            assert!(cpu.flag(FLAG_AF));
        }
    }
}

#[test]
fn mkii_logical_branches_preserve_all_conditions_and_later_faults() {
    for inert in [false, true] {
        for op in [1u8, 4, 6, 8] {
            for width in [BusWidth::Byte, BusWidth::Word, BusWidth::Dword] {
                for condition in 0..16 {
                    for (a, b) in [(0, 0), (0x80008080, u32::MAX), (1, 0x101)] {
                        let mut code = Vec::new();
                        if width == BusWidth::Dword {
                            code.push(0x66);
                        }
                        let byte = width == BusWidth::Byte;
                        code.extend_from_slice(&[
                            if op == 8 {
                                if byte { 0x84 } else { 0x85 }
                            } else {
                                op * 8 + if byte { 2 } else { 3 }
                            },
                            if op == 8 { 0xcb } else { 0xd9 },
                            0x70 + condition,
                            4,
                            0x8b,
                            0x16,
                            0,
                            0x30,
                            0xe6,
                            0x60,
                        ]);
                        let (mut cpu, mut bus) = fixture(&code);
                        let (mut oracle, mut other) = fixture(&code);
                        for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
                            if inert {
                                enable_inert_regions(bus);
                            } else {
                                enable_read_regions(bus);
                            }
                            cpu.registers.set_ebx(a);
                            cpu.registers.set_ecx(b);
                            warm_read(cpu, bus, 0x2000);
                            warm_code(cpu, bus, code.len() as u32);
                            let mut ds = cpu.registers.segment(SegmentIndex::Ds);
                            ds.limit = 0x2fff;
                            cpu.registers.set_segment(SegmentIndex::Ds, ds);
                        }
                        compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, 1000);
                        let stats = cpu.dynarec_mkii_stats();
                        assert_eq!(stats.regions, 1);
                        assert_eq!(stats.native, 2, "op={op} {width:?} condition={condition}");
                        assert_eq!(
                            stats.region_guard_misses,
                            u64::from(!oracle.condition(condition))
                        );
                        if op == 8 {
                            assert_eq!(cpu.registers.ebx(), a);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn mkii_logical_region_caps_match_each_instruction_boundary() {
    for inert in [false, true] {
        let code = [
            0x0b, 0xc1, 0x84, 0xec, 0x33, 0xda, 0x83, 0xe3, 0x80, 0xe6, 0x60,
        ];
        for grant in [false, true] {
            for cap in 0..40 {
                for remainder in 0..12 {
                    let (mut cpu, mut bus) = fixture(&code);
                    let (mut oracle, mut other) = fixture(&code);
                    for (cpu, bus) in [(&mut cpu, &mut bus), (&mut oracle, &mut other)] {
                        if inert {
                            enable_inert_regions(bus);
                        } else {
                            enable_read_regions(bus);
                        }
                        bus.mkii_read_regions = grant;
                        bus.mkii_inert_regions &= grant;
                        cpu.registers.set_eax(0x12340001);
                        cpu.registers.set_ecx(0xaabb0000);
                        cpu.registers.set_ebx(0x55aa00ff);
                        cpu.registers.set_edx(0xff);
                        warm_code(cpu, bus, code.len() as u32);
                        cpu.timing_rem = remainder;
                    }
                    compare_pair_run(&mut cpu, &mut bus, &mut oracle, &mut other, cap);
                }
            }
        }
    }
}
