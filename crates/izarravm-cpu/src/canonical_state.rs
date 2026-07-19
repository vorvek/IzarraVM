// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use izarravm_core::{CanonicalFieldWriter, CanonicalStateError};

use crate::{CpuGsw, GswMode, SegmentIndex, SegmentRegister};

const fn mode_tag(mode: GswMode) -> u32 {
    match mode {
        GswMode::Gsw386Slow => 1,
        GswMode::Gsw386 => 2,
        GswMode::Gsw486 => 3,
        GswMode::Gsw586 => 4,
    }
}

fn write_segment(
    out: &mut CanonicalFieldWriter<'_>,
    segment: SegmentRegister,
) -> Result<(), CanonicalStateError> {
    out.write_u16(segment.selector)?;
    out.write_u32(segment.base)?;
    out.write_u32(segment.limit)?;
    out.write_u8(segment.access)?;
    out.write_bool(segment.default_size_32)
}

impl CpuGsw {
    /// Writes version 1 of the CPU architectural payload without changing CPU state.
    /// Hidden execution representations and x87 state use separate payloads.
    pub fn write_canonical_arch_payload(
        &self,
        out: &mut CanonicalFieldWriter<'_>,
    ) -> Result<(), CanonicalStateError> {
        out.write_u32(self.registers.eax())?;
        out.write_u32(self.registers.ecx())?;
        out.write_u32(self.registers.edx())?;
        out.write_u32(self.registers.ebx())?;
        out.write_u32(self.registers.esp())?;
        out.write_u32(self.registers.ebp())?;
        out.write_u32(self.registers.esi())?;
        out.write_u32(self.registers.edi())?;
        for index in [
            SegmentIndex::Es,
            SegmentIndex::Cs,
            SegmentIndex::Ss,
            SegmentIndex::Ds,
            SegmentIndex::Fs,
            SegmentIndex::Gs,
        ] {
            write_segment(out, self.registers.segment(index))?;
        }
        out.write_u32(self.registers.eip)?;
        out.write_u32(self.eflags())?;
        out.write_u32(self.control.cr0)?;
        out.write_u32(self.control.cr2)?;
        out.write_u32(self.control.cr3)?;
        out.write_u32(self.control.cr4)?;
        for value in self.control.dr0_3 {
            out.write_u32(value)?;
        }
        out.write_u32(self.control.dr6)?;
        out.write_u32(self.control.dr7)?;
        out.write_u64(self.msr.mcar)?;
        out.write_u64(self.msr.mctr)?;
        out.write_u64(self.time_stamp_counter())?;
        out.write_u32(self.gdtr.base)?;
        out.write_u16(self.gdtr.limit)?;
        out.write_u32(self.idtr.base)?;
        out.write_u16(self.idtr.limit)?;
        write_segment(out, self.ldtr)?;
        write_segment(out, self.tr)?;
        out.write_tag(mode_tag(self.mode))?;
        out.write_u8(self.cpl)
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use izarravm_core::{
        CanonicalSectionId, CanonicalSectionRequirement, CanonicalSectionVersion,
        CanonicalStateView, CanonicalStateWriter,
    };

    use super::*;
    use crate::{Msrs, PendingFlags};

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
    #[cfg(feature = "jit")]
    fn arch_payload_keeps_pending_flags_offset_pinned() {
        assert_eq!(core::mem::offset_of!(CpuGsw, pending_flags), 4440);
        let cpu = sentinel_cpu();
        let _ = arch_payload(&cpu);
        assert_eq!(core::mem::offset_of!(CpuGsw, pending_flags), 4440);
    }
}
