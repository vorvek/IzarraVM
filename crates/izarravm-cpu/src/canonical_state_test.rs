// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use std::ops::Range;

use izarravm_core::{
    CanonicalSectionId, CanonicalSectionRequirement, CanonicalSectionVersion, CanonicalStateView,
    CanonicalStateWriter,
};

use super::*;
use crate::{
    AddressSize, DecodeGroup, DecodedInsn, Msrs, OperandSize, PendingFlags, Prefixes, RepBudget,
    RepResume, TRACKED_WRITE_PAGES,
};

const ARCH_PAYLOAD_LEN: usize = 217;
const EFLAGS_RANGE: Range<usize> = 108..112;
const TSC_RANGE: Range<usize> = 168..176;
type SpanMutation = (Range<usize>, fn(&mut CpuGsw));

fn arch_payload(cpu: &CpuGsw) -> Vec<u8> {
    let mut state = CanonicalStateWriter::new().unwrap();
    state
        .section(
            CanonicalSectionId::new(1).unwrap(),
            CanonicalSectionVersion::new(1).unwrap(),
            CanonicalSectionRequirement::Required,
            |out| cpu.write_canonical_arch_payload(out),
        )
        .unwrap();
    let bytes = state.finish().unwrap();
    let view = CanonicalStateView::parse(&bytes).unwrap();
    view.sections()[0].payload().to_vec()
}

fn execution_payload(cpu: &CpuGsw) -> Result<Vec<u8>, CpuCanonicalCaptureError> {
    let capture = cpu.canonical_execution_capture()?;
    let mut state = CanonicalStateWriter::new().unwrap();
    state
        .section(
            CanonicalSectionId::new(2).unwrap(),
            CanonicalSectionVersion::new(1).unwrap(),
            CanonicalSectionRequirement::Required,
            |out| capture.write_payload(out),
        )
        .unwrap();
    let bytes = state.finish().unwrap();
    let view = CanonicalStateView::parse(&bytes).unwrap();
    Ok(view.sections()[0].payload().to_vec())
}

fn live_prefetch_cpu(length: u8) -> CpuGsw {
    let mut cpu = CpuGsw::default();
    let cs = SegmentRegister {
        selector: 0x1234,
        base: 0x0001_0000,
        limit: 0x0000_ffff,
        access: 0x9b,
        default_size_32: true,
    };
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    cpu.registers.eip = 0x101;
    cpu.prefetch.cs = cs;
    cpu.prefetch.linear_base = cs.base + 0x100;
    cpu.prefetch.physical_base = 0x0008_0000;
    cpu.prefetch.len = length;
    for (index, byte) in cpu.prefetch.bytes.iter_mut().enumerate() {
        *byte = 0x80u8.wrapping_add(index as u8);
    }
    cpu
}

fn dummy_rep_resume() -> RepResume {
    RepResume {
        insn: DecodedInsn {
            len: 1,
            prefixes: Prefixes::default(),
            opcode: 0x90,
            operand_size: OperandSize::Word,
            address_size: AddressSize::Word,
            modrm: None,
            operand: None,
            imm: 0,
            imm2: 0,
            group: DecodeGroup::Misc,
            continuable: false,
            disp_len: 0,
            imm_len: 0,
        },
        start_eip: 0x100,
        post_eip: 0x101,
        cs: SegmentRegister::default(),
        precharged_core: 1,
    }
}

fn append_segment(expected: &mut Vec<u8>, segment: SegmentRegister) {
    expected.extend_from_slice(&segment.selector.to_le_bytes());
    expected.extend_from_slice(&segment.base.to_le_bytes());
    expected.extend_from_slice(&segment.limit.to_le_bytes());
    expected.push(segment.access);
    expected.push(u8::from(segment.default_size_32));
}

fn sentinel_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.registers.gpr = [
        0x0102_0304,
        0x1112_1314,
        0x2122_2324,
        0x3132_3334,
        0x4142_4344,
        0x5152_5354,
        0x6162_6364,
        0x7172_7374,
    ];
    for (index, segment) in cpu.registers.segments.iter_mut().enumerate() {
        *segment = SegmentRegister {
            selector: 0x1000 + index as u16,
            base: 0x2000_0000 + index as u32,
            limit: 0x3000_0000 + index as u32,
            access: 0x40 + index as u8,
            default_size_32: index % 2 != 0,
        };
    }
    cpu.registers.eip = 0x8182_8384;
    cpu.registers.eflags = 0x9192_9394;
    cpu.control.cr0 = 0xa1a2_a3a4;
    cpu.control.cr2 = 0xb1b2_b3b4;
    cpu.control.cr3 = 0xc1c2_c3c4;
    cpu.control.cr4 = 0xd1d2_d3d4;
    cpu.control.dr0_3 = [0xe1e2_e3e4, 0xf1f2_f3f4, 0x0101_0202, 0x0303_0404];
    cpu.control.dr6 = 0x0505_0606;
    cpu.control.dr7 = 0x0707_0808;
    cpu.msr.mcar = 0x1112_1314_1516_1718;
    cpu.msr.mctr = 0x2122_2324_2526_2728;
    cpu.elapsed_clocks = 0x0102_0304_0506_0708;
    cpu.msr.tsc_offset = 0x10;
    cpu.gdtr.base = 0x3132_3334;
    cpu.gdtr.limit = 0x3536;
    cpu.idtr.base = 0x4142_4344;
    cpu.idtr.limit = 0x4546;
    cpu.ldtr = SegmentRegister {
        selector: 0x5152,
        base: 0x5354_5556,
        limit: 0x5758_595a,
        access: 0x5b,
        default_size_32: true,
    };
    cpu.tr = SegmentRegister {
        selector: 0x6162,
        base: 0x6364_6566,
        limit: 0x6768_696a,
        access: 0x6b,
        default_size_32: false,
    };
    cpu.mode = GswMode::Gsw586;
    cpu.cpl = 3;
    cpu
}

#[test]
fn arch_payload_has_exact_golden_bytes() {
    let cpu = sentinel_cpu();
    let mut expected = Vec::new();
    for value in cpu.registers.gpr {
        expected.extend_from_slice(&value.to_le_bytes());
    }
    for segment in cpu.registers.segments {
        append_segment(&mut expected, segment);
    }
    expected.extend_from_slice(&0x8182_8384u32.to_le_bytes());
    expected.extend_from_slice(&0x9192_9394u32.to_le_bytes());
    for value in [
        0xa1a2_a3a4u32,
        0xb1b2_b3b4,
        0xc1c2_c3c4,
        0xd1d2_d3d4,
        0xe1e2_e3e4,
        0xf1f2_f3f4,
        0x0101_0202,
        0x0303_0404,
        0x0505_0606,
        0x0707_0808,
    ] {
        expected.extend_from_slice(&value.to_le_bytes());
    }
    expected.extend_from_slice(&0x1112_1314_1516_1718u64.to_le_bytes());
    expected.extend_from_slice(&0x2122_2324_2526_2728u64.to_le_bytes());
    expected.extend_from_slice(&0x0102_0304_0506_0718u64.to_le_bytes());
    expected.extend_from_slice(&0x3132_3334u32.to_le_bytes());
    expected.extend_from_slice(&0x3536u16.to_le_bytes());
    expected.extend_from_slice(&0x4142_4344u32.to_le_bytes());
    expected.extend_from_slice(&0x4546u16.to_le_bytes());
    append_segment(&mut expected, cpu.ldtr);
    append_segment(&mut expected, cpu.tr);
    expected.extend_from_slice(&4u32.to_le_bytes());
    expected.push(3);
    assert_eq!(expected.len(), ARCH_PAYLOAD_LEN);
    assert_eq!(arch_payload(&cpu), expected);
}

fn assert_only_span_changes<F>(cpu: &CpuGsw, span: Range<usize>, mutate: F)
where
    F: FnOnce(&mut CpuGsw),
{
    let before = arch_payload(cpu);
    let mut changed = cpu.clone();
    mutate(&mut changed);
    let after = arch_payload(&changed);
    let changed_offsets: Vec<_> = before
        .iter()
        .zip(&after)
        .enumerate()
        .filter_map(|(index, (left, right))| (left != right).then_some(index))
        .collect();
    assert!(
        !changed_offsets.is_empty(),
        "mutation did not change payload"
    );
    assert!(
        changed_offsets.iter().all(|offset| span.contains(offset)),
        "changed offsets {changed_offsets:?} escaped {span:?}"
    );
}

#[test]
fn every_architectural_field_has_one_declared_span() {
    let cpu = sentinel_cpu();
    let mut checks = 0;
    for index in 0..8 {
        assert_only_span_changes(&cpu, index * 4..index * 4 + 4, |changed| {
            changed.registers.gpr[index] ^= 1;
        });
        checks += 1;
    }
    for index in 0..6 {
        let start = 32 + index * 12;
        assert_only_span_changes(&cpu, start..start + 2, |changed| {
            changed.registers.segments[index].selector ^= 1;
        });
        assert_only_span_changes(&cpu, start + 2..start + 6, |changed| {
            changed.registers.segments[index].base ^= 1;
        });
        assert_only_span_changes(&cpu, start + 6..start + 10, |changed| {
            changed.registers.segments[index].limit ^= 1;
        });
        assert_only_span_changes(&cpu, start + 10..start + 11, |changed| {
            changed.registers.segments[index].access ^= 1;
        });
        assert_only_span_changes(&cpu, start + 11..start + 12, |changed| {
            changed.registers.segments[index].default_size_32 ^= true;
        });
        checks += 5;
    }
    assert_only_span_changes(&cpu, 104..108, |changed| changed.registers.eip ^= 1);
    assert_only_span_changes(&cpu, EFLAGS_RANGE, |changed| changed.registers.eflags ^= 1);
    checks += 2;
    let control_mutations: [SpanMutation; 4] = [
        (112..116, |cpu: &mut CpuGsw| cpu.control.cr0 ^= 1),
        (116..120, |cpu: &mut CpuGsw| cpu.control.cr2 ^= 1),
        (120..124, |cpu: &mut CpuGsw| cpu.control.cr3 ^= 1),
        (124..128, |cpu: &mut CpuGsw| cpu.control.cr4 ^= 1),
    ];
    for (span, mutate) in control_mutations {
        assert_only_span_changes(&cpu, span, mutate);
        checks += 1;
    }
    for index in 0..4 {
        let start = 128 + index * 4;
        assert_only_span_changes(&cpu, start..start + 4, |changed| {
            changed.control.dr0_3[index] ^= 1;
        });
        checks += 1;
    }
    assert_only_span_changes(&cpu, 144..148, |changed| changed.control.dr6 ^= 1);
    assert_only_span_changes(&cpu, 148..152, |changed| changed.control.dr7 ^= 1);
    checks += 2;
    assert_only_span_changes(&cpu, 152..160, |changed| changed.msr.mcar ^= 1);
    assert_only_span_changes(&cpu, 160..168, |changed| changed.msr.mctr ^= 1);
    assert_only_span_changes(&cpu, TSC_RANGE, |changed| changed.elapsed_clocks ^= 1);
    checks += 3;
    assert_only_span_changes(&cpu, 176..180, |changed| changed.gdtr.base ^= 1);
    assert_only_span_changes(&cpu, 180..182, |changed| changed.gdtr.limit ^= 1);
    assert_only_span_changes(&cpu, 182..186, |changed| changed.idtr.base ^= 1);
    assert_only_span_changes(&cpu, 186..188, |changed| changed.idtr.limit ^= 1);
    checks += 4;
    for (start, task) in [(188, false), (200, true)] {
        assert_only_span_changes(&cpu, start..start + 2, |changed| {
            let segment = if task {
                &mut changed.tr
            } else {
                &mut changed.ldtr
            };
            segment.selector ^= 1;
        });
        assert_only_span_changes(&cpu, start + 2..start + 6, |changed| {
            let segment = if task {
                &mut changed.tr
            } else {
                &mut changed.ldtr
            };
            segment.base ^= 1;
        });
        assert_only_span_changes(&cpu, start + 6..start + 10, |changed| {
            let segment = if task {
                &mut changed.tr
            } else {
                &mut changed.ldtr
            };
            segment.limit ^= 1;
        });
        assert_only_span_changes(&cpu, start + 10..start + 11, |changed| {
            let segment = if task {
                &mut changed.tr
            } else {
                &mut changed.ldtr
            };
            segment.access ^= 1;
        });
        assert_only_span_changes(&cpu, start + 11..start + 12, |changed| {
            let segment = if task {
                &mut changed.tr
            } else {
                &mut changed.ldtr
            };
            segment.default_size_32 ^= true;
        });
        checks += 5;
    }
    assert_only_span_changes(&cpu, 212..216, |changed| changed.mode = GswMode::Gsw486);
    assert_only_span_changes(&cpu, 216..217, |changed| changed.cpl ^= 1);
    checks += 2;
    assert_eq!(checks, 69);
}

#[test]
fn effective_eflags_is_normalized_without_mutation() {
    let mut lazy = CpuGsw::default();
    lazy.registers.eflags = 0x202;
    lazy.pending_flags = PendingFlags {
        tag: (1 << 31) | (2 << 8),
        a: 0xffff_ffff,
        b: 1,
        result: 0,
    };
    let mut materialized = lazy.clone();
    materialized.registers.eflags = lazy.eflags();
    materialized.pending_flags = PendingFlags::default();
    assert_eq!(arch_payload(&lazy), arch_payload(&materialized));

    let raw_before = lazy.registers.eflags;
    let pending_before = lazy.pending_flags;
    let _ = arch_payload(&lazy);
    assert_eq!(lazy.registers.eflags, raw_before);
    assert_eq!(lazy.pending_flags, pending_before);
    assert_only_span_changes(&lazy, EFLAGS_RANGE, |changed| {
        changed.pending_flags.result = 1;
    });
}

#[test]
fn architectural_tsc_is_normalized() {
    let first = CpuGsw {
        elapsed_clocks: 100,
        msr: Msrs {
            tsc_offset: 200,
            ..Msrs::default()
        },
        ..CpuGsw::default()
    };
    let mut second = first.clone();
    second.elapsed_clocks = 250;
    second.msr.tsc_offset = 50;
    assert_eq!(arch_payload(&first), arch_payload(&second));
    assert_only_span_changes(&first, TSC_RANGE, |changed| {
        changed.msr.tsc_offset += 1;
    });
}

#[test]
fn mode_tags_are_fixed_and_nonzero() {
    for (mode, tag) in [
        (GswMode::Gsw386Slow, 1u32),
        (GswMode::Gsw386, 2),
        (GswMode::Gsw486, 3),
        (GswMode::Gsw586, 4),
    ] {
        let cpu = CpuGsw {
            mode,
            ..CpuGsw::default()
        };
        assert_eq!(&arch_payload(&cpu)[212..216], &tag.to_le_bytes());
    }
}

#[test]
fn hidden_and_host_state_do_not_enter_arch_payload() {
    let baseline = CpuGsw::default();
    let expected = arch_payload(&baseline);
    let mut changed = baseline;
    changed.fpu.control ^= 1;
    changed.halted = true;
    changed.interrupt_shadow = true;
    changed.core_clocks_so_far = 17;
    changed.timing_rem = 3;
    changed.fp_rem = 4;
    changed.alignment_armed = true;
    changed.perf.instructions = 5;
    assert_eq!(arch_payload(&changed), expected);
}

#[test]
fn execution_payload_has_exact_minimum_golden() {
    let payload = execution_payload(&CpuGsw::default()).unwrap();
    let mut expected = vec![0; 56];
    expected[..4].copy_from_slice(&2u32.to_le_bytes());
    assert_eq!(payload, expected);
}

#[test]
fn execution_payload_has_exact_maximum_golden() {
    let mut cpu = live_prefetch_cpu(PREFETCH_WINDOW_BYTES as u8);
    cpu.registers.eflags = 0x8000_0202;
    cpu.pending_flags = PendingFlags {
        tag: 0x8123_4567,
        a: 0x1020_3040,
        b: 0x5060_7080,
        result: 0x90a0_b0c0,
    };
    cpu.elapsed_clocks = 0x0102_0304_0506_0708;
    cpu.timing_rem = 0x1112_1314_1516_1718;
    cpu.fp_rem = 0x2122_2324_2526_2728;
    cpu.halted = true;
    cpu.interrupt_shadow = true;
    for slot in (0..TLB_ENTRIES).rev() {
        let page = 0x4000 + slot as u32;
        cpu.tlb.insert(
            page,
            0x0400_0000 + (slot as u32) * 0x1000,
            slot % 2 == 0,
            slot % 3 == 0,
            slot % 5 == 0,
        );
    }

    let mut expected = Vec::new();
    expected.extend_from_slice(&0x8000_0202u32.to_le_bytes());
    expected.extend_from_slice(&0x8123_4567u32.to_le_bytes());
    expected.extend_from_slice(&0x1020_3040u32.to_le_bytes());
    expected.extend_from_slice(&0x5060_7080u32.to_le_bytes());
    expected.extend_from_slice(&0x90a0_b0c0u32.to_le_bytes());
    expected.extend_from_slice(&0x0102_0304_0506_0708u64.to_le_bytes());
    expected.extend_from_slice(&0x1112_1314_1516_1718u64.to_le_bytes());
    expected.extend_from_slice(&0x2122_2324_2526_2728u64.to_le_bytes());
    expected.extend_from_slice(&[1, 1]);
    expected.extend_from_slice(&(TLB_ENTRIES as u64).to_le_bytes());
    for slot in 0..TLB_ENTRIES {
        let page = 0x4000 + slot as u32;
        expected.extend_from_slice(&page.to_le_bytes());
        expected.extend_from_slice(&(0x0400_0000 + (slot as u32) * 0x1000).to_le_bytes());
        expected.push(u8::from(slot % 2 == 0));
        expected.push(u8::from(slot % 3 == 0));
        expected.push(u8::from(slot % 5 == 0));
    }
    expected.extend_from_slice(&[0, 1]);
    append_segment(&mut expected, cpu.prefetch.cs);
    expected.extend_from_slice(&cpu.prefetch.linear_base.to_le_bytes());
    expected.extend_from_slice(&cpu.prefetch.physical_base.to_le_bytes());
    expected.extend_from_slice(&(PREFETCH_WINDOW_BYTES as u64).to_le_bytes());
    expected.extend_from_slice(&cpu.prefetch.bytes);

    // 54 fixed header bytes through the live-entry count, 11 per TLB entry (tag, phys, three
    // bools), 62 for the prefetch tail. Spelled out rather than golden-ed to a single number
    // because the entry count is a tuning knob (see TLB_ENTRIES): a bare literal has to be
    // re-goldened on every sweep, while reusing the loop's own arithmetic would be a tautology
    // that could not catch a field width changing. Checks out at every size measured: 64 gives
    // 820, 256 gives 2932, 1024 gives 11380.
    assert_eq!(expected.len(), 54 + TLB_ENTRIES * 11 + 62);
    assert_eq!(execution_payload(&cpu).unwrap(), expected);
}

fn assert_only_execution_span_changes<F>(cpu: &CpuGsw, span: Range<usize>, mutate: F)
where
    F: FnOnce(&mut CpuGsw),
{
    let before = execution_payload(cpu).unwrap();
    let mut changed = cpu.clone();
    mutate(&mut changed);
    let after = execution_payload(&changed).unwrap();
    assert_eq!(before.len(), after.len());
    let changed_offsets: Vec<_> = before
        .iter()
        .zip(&after)
        .enumerate()
        .filter_map(|(index, (left, right))| (left != right).then_some(index))
        .collect();
    assert!(
        !changed_offsets.is_empty(),
        "mutation did not change execution payload"
    );
    assert!(
        changed_offsets.iter().all(|offset| span.contains(offset)),
        "changed offsets {changed_offsets:?} escaped {span:?}"
    );
}

#[test]
fn every_fixed_execution_field_has_one_declared_span() {
    let cpu = CpuGsw::default();
    let checks: [SpanMutation; 10] = [
        (0..4, |cpu| cpu.registers.eflags ^= 1),
        (4..8, |cpu| cpu.pending_flags.tag ^= 1),
        (8..12, |cpu| cpu.pending_flags.a ^= 1),
        (12..16, |cpu| cpu.pending_flags.b ^= 1),
        (16..20, |cpu| cpu.pending_flags.result ^= 1),
        (20..28, |cpu| cpu.elapsed_clocks ^= 1),
        (28..36, |cpu| cpu.timing_rem ^= 1),
        (36..44, |cpu| cpu.fp_rem ^= 1),
        (44..45, |cpu| cpu.halted = true),
        (45..46, |cpu| cpu.interrupt_shadow = true),
    ];
    for (span, mutate) in checks {
        assert_only_execution_span_changes(&cpu, span, mutate);
    }
}

#[test]
fn every_tlb_entry_field_has_one_declared_span() {
    const PAGE: u32 = 5;
    let mut cpu = CpuGsw::default();
    cpu.tlb.insert(PAGE, 0x1234_5000, false, false, false);
    let slot = Tlb::slot(PAGE);
    assert_only_execution_span_changes(&cpu, 54..58, move |changed| {
        changed.tlb.entries[slot].tag += TLB_ENTRIES as u32;
    });
    assert_only_execution_span_changes(&cpu, 58..62, move |changed| {
        changed.tlb.entries[slot].phys ^= 0x1000;
    });
    assert_only_execution_span_changes(&cpu, 62..63, move |changed| {
        changed.tlb.entries[slot].writable = true;
    });
    assert_only_execution_span_changes(&cpu, 63..64, move |changed| {
        changed.tlb.entries[slot].user = true;
    });
    assert_only_execution_span_changes(&cpu, 64..65, move |changed| {
        changed.tlb.entries[slot].dirty = true;
    });
}

#[test]
fn execution_payload_preserves_raw_lazy_flag_representation() {
    let mut lazy = CpuGsw::default();
    lazy.registers.eflags = 0x202;
    lazy.pending_flags = PendingFlags {
        tag: (1 << 31) | (2 << 8),
        a: 0xffff_ffff,
        b: 1,
        result: 0,
    };
    let mut materialized = lazy.clone();
    materialized.registers.eflags = lazy.eflags();
    materialized.pending_flags = PendingFlags::default();
    assert_eq!(arch_payload(&lazy), arch_payload(&materialized));
    assert_ne!(
        execution_payload(&lazy).unwrap(),
        execution_payload(&materialized).unwrap()
    );

    let mut stale_none = CpuGsw::default();
    stale_none.pending_flags.a = 1;
    stale_none.pending_flags.b = 2;
    stale_none.pending_flags.result = 3;
    assert_ne!(
        execution_payload(&CpuGsw::default()).unwrap(),
        execution_payload(&stale_none).unwrap()
    );
}

#[test]
fn logical_tlb_projection_normalizes_generation_order_and_dead_residue() {
    let mut first = CpuGsw::default();
    first.tlb.insert(3, 0x3000, true, false, true);
    first.tlb.insert(68, 0x44000, false, true, false);

    let mut second = CpuGsw::default();
    second.tlb.flush();
    second.tlb.insert(68, 0x44000, false, true, false);
    second.tlb.insert(3, 0x3000, true, false, true);
    let dead_slot = Tlb::slot(10);
    second.tlb.entries[dead_slot] = TlbEntry {
        tag: 10,
        phys: 0xdead_0000,
        generation: second.tlb.generation.wrapping_sub(1),
        writable: true,
        user: true,
        dirty: true,
    };
    second.tlb.entries[12] = TlbEntry {
        tag: 13,
        phys: 0xbeef_0000,
        generation: second.tlb.generation,
        writable: true,
        user: true,
        dirty: true,
    };
    assert_eq!(
        execution_payload(&first).unwrap(),
        execution_payload(&second).unwrap()
    );
}

#[test]
fn logical_tlb_projection_observes_replacement_invalidation_and_entry_bits() {
    const PAGE: u32 = 5;
    let mut baseline = CpuGsw::default();
    baseline.tlb.insert(PAGE, 0x5000, false, false, false);
    let expected = execution_payload(&baseline).unwrap();
    let slot = Tlb::slot(PAGE);

    let mut changed = baseline.clone();
    changed
        .tlb
        .insert(PAGE + TLB_ENTRIES as u32, 0x69000, false, false, false);
    assert_ne!(execution_payload(&changed).unwrap(), expected);
    changed = baseline.clone();
    changed.tlb.invalidate(PAGE);
    assert_ne!(execution_payload(&changed).unwrap(), expected);
    for mutate in [
        |entry: &mut TlbEntry| entry.phys ^= 0x1000,
        |entry: &mut TlbEntry| entry.writable = true,
        |entry: &mut TlbEntry| entry.user = true,
        |entry: &mut TlbEntry| entry.dirty = true,
    ] {
        changed = baseline.clone();
        mutate(&mut changed.tlb.entries[slot]);
        assert_ne!(execution_payload(&changed).unwrap(), expected);
    }
}

#[test]
fn logical_prefetch_projection_keeps_only_live_bytes() {
    let baseline = live_prefetch_cpu(4);
    let expected = execution_payload(&baseline).unwrap();
    let mut changed = baseline.clone();
    changed.prefetch.bytes[0] ^= 1;
    assert_ne!(execution_payload(&changed).unwrap(), expected);
    changed = baseline.clone();
    changed.prefetch.bytes[4] ^= 1;
    assert_eq!(execution_payload(&changed).unwrap(), expected);

    let mut stale_cs = baseline.clone();
    stale_cs.prefetch.cs.selector ^= 1;
    assert_eq!(
        execution_payload(&stale_cs).unwrap(),
        execution_payload(&CpuGsw::default()).unwrap()
    );
    let mut stale_eip = baseline;
    stale_eip.registers.eip = 0x200;
    assert_eq!(
        execution_payload(&stale_eip).unwrap(),
        execution_payload(&CpuGsw::default()).unwrap()
    );
}

#[test]
fn pending_prefetch_invalidation_is_canonical() {
    let baseline = live_prefetch_cpu(4);
    let expected = execution_payload(&baseline).unwrap();
    assert_eq!(&expected[54..56], &[0, 1]);

    let target_page = baseline.prefetch.physical_base >> 12;
    let mut same_page = baseline.clone();
    same_page.written_pages[0] = Some(target_page);
    same_page.written_count = 1;
    let pending = execution_payload(&same_page).unwrap();
    assert_eq!(pending.len(), 56);
    assert_eq!(&pending[54..56], &[1, 0]);

    let mut unrelated = baseline.clone();
    unrelated.written_pages[0] = Some(target_page + 1);
    unrelated.written_count = 1;
    assert_eq!(execution_payload(&unrelated).unwrap(), expected);

    let mut overflow = baseline.clone();
    for (index, page) in overflow.written_pages.iter_mut().enumerate() {
        *page = Some(target_page + index as u32 + 1);
    }
    overflow.written_count = TRACKED_WRITE_PAGES as u8;
    overflow.written_pages_overflow = true;
    assert_eq!(execution_payload(&overflow).unwrap(), pending);

    let mut reordered = same_page.clone();
    reordered.written_pages[1] = Some(target_page + 1);
    reordered.written_count = 2;
    let mut reverse = reordered.clone();
    reverse.written_pages.swap(0, 1);
    assert_eq!(
        execution_payload(&reordered).unwrap(),
        execution_payload(&reverse).unwrap()
    );

    let mut duplicate = same_page;
    duplicate.written_pages[1] = Some(target_page);
    duplicate.written_count = 2;
    assert_eq!(execution_payload(&duplicate).unwrap(), pending);
}

#[test]
fn capture_accepts_decode_residue_and_excludes_boundary_scratch() {
    let baseline = CpuGsw::default();
    let expected = execution_payload(&baseline).unwrap();
    let mut changed = baseline;
    changed.decode_tail_start = 0x1234_5678;
    changed.decode_disp_len = 4;
    changed.core_clocks_so_far = 0x1122_3344_5566_7788;
    assert_eq!(execution_payload(&changed).unwrap(), expected);
}

#[test]
fn capture_rejects_every_rep_residual() {
    let mut cases = Vec::new();
    let active = CpuGsw {
        rep_resume_active: true,
        ..CpuGsw::default()
    };
    cases.push(active);
    let mut budget = CpuGsw::default();
    budget.rep_execution.budget = Some(RepBudget {
        bus_at_entry: 1,
        cap: 2,
    });
    cases.push(budget);
    let mut yielded = CpuGsw::default();
    yielded.rep_execution.yielded = true;
    cases.push(yielded);
    let mut resume = CpuGsw::default();
    resume.rep_execution.resume = Some(dummy_rep_resume());
    cases.push(resume);

    for cpu in cases {
        assert_eq!(
            cpu.canonical_execution_capture().err(),
            Some(CpuCanonicalCaptureError::ActiveRepContinuation)
        );
    }
}

#[test]
fn capture_validates_alignment_prefetch_and_write_tracking() {
    let mut alignment = CpuGsw {
        alignment_armed: true,
        ..CpuGsw::default()
    };
    assert_eq!(
        alignment.canonical_execution_capture().err(),
        Some(CpuCanonicalCaptureError::InconsistentAlignmentCache)
    );
    alignment.control.cr0 |= CR0_AM;
    alignment.registers.eflags |= FLAG_AC;
    assert!(alignment.canonical_execution_capture().is_ok());

    let mut prefetch = CpuGsw::default();
    prefetch.prefetch.len = PREFETCH_WINDOW_BYTES as u8 + 1;
    assert_eq!(
        prefetch.canonical_execution_capture().err(),
        Some(CpuCanonicalCaptureError::InvalidPrefetchLength {
            length: PREFETCH_WINDOW_BYTES as u8 + 1,
        })
    );

    let too_many = CpuGsw {
        written_count: TRACKED_WRITE_PAGES as u8 + 1,
        ..CpuGsw::default()
    };
    assert_eq!(
        too_many.canonical_execution_capture().err(),
        Some(CpuCanonicalCaptureError::InvalidWriteTracker)
    );
    let mut nonpacked = CpuGsw::default();
    nonpacked.written_pages[1] = Some(1);
    nonpacked.written_count = 1;
    assert_eq!(
        nonpacked.canonical_execution_capture().err(),
        Some(CpuCanonicalCaptureError::InvalidWriteTracker)
    );
    let mut short_overflow = CpuGsw::default();
    short_overflow.written_pages[0] = Some(1);
    short_overflow.written_count = 1;
    short_overflow.written_pages_overflow = true;
    assert_eq!(
        short_overflow.canonical_execution_capture().err(),
        Some(CpuCanonicalCaptureError::InvalidWriteTracker)
    );
}

#[test]
fn transparent_execution_caches_do_not_enter_payload() {
    let baseline = CpuGsw::default();
    let expected = execution_payload(&baseline).unwrap();
    let mut changed = baseline;
    changed.code_page.valid = true;
    changed.code_page.linear_page = 1;
    changed.code_page.physical_page = 2;
    changed.fetch_page.entries[0].valid = true;
    changed.fetch_page.entries[0].linear_page = 3;
    changed.fetch_page.entries[0].physical_page = 4;
    changed.fetch_page.entries[0].ptr = std::ptr::NonNull::<u8>::dangling().as_ptr();
    changed.fetch_page.entries[0].len = 4096;
    changed.data_read_pages.entries[0].physical_page = 5;
    changed.data_write_pages.entries[0].physical_page = 6;
    changed.decode_cache.generation = changed.decode_cache.generation.wrapping_add(1);
    changed.perf.instructions = 7;
    changed.profile.enabled = true;
    assert_eq!(execution_payload(&changed).unwrap(), expected);
    changed.invalidate_code_caches();
    assert_eq!(execution_payload(&changed).unwrap(), expected);
}

#[test]
fn execution_serialization_does_not_mutate_cpu() {
    let cpu = live_prefetch_cpu(8);
    let before = execution_payload(&cpu).unwrap();
    let raw_eflags = cpu.registers.eflags;
    let pending = cpu.pending_flags;
    let prefetch = cpu.prefetch.bytes;
    let after = execution_payload(&cpu).unwrap();
    assert_eq!(after, before);
    assert_eq!(cpu.registers.eflags, raw_eflags);
    assert_eq!(cpu.pending_flags, pending);
    assert_eq!(cpu.prefetch.bytes, prefetch);
}

#[test]
#[cfg(feature = "jit")]
fn arch_payload_keeps_pending_flags_offset_pinned() {
    assert_eq!(core::mem::offset_of!(CpuGsw, pending_flags), 4488);
    let cpu = sentinel_cpu();
    let _ = arch_payload(&cpu);
    assert_eq!(core::mem::offset_of!(CpuGsw, pending_flags), 4488);
}
