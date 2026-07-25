// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use izarravm_core::{CanonicalFieldWriter, CanonicalStateError};
use thiserror::Error;

use crate::{
    CR0_AM, CpuGsw, FLAG_AC, GswMode, PREFETCH_WINDOW_BYTES, SegmentIndex, SegmentRegister,
    TLB_ENTRIES, Tlb, TlbEntry,
};

/// A CPU boundary that cannot be represented by the compare-only execution payload.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum CpuCanonicalCaptureError {
    #[error("a budgeted REP continuation is still active")]
    ActiveRepContinuation,
    #[error("the cached alignment-check state disagrees with CR0 and EFLAGS")]
    InconsistentAlignmentCache,
    #[error("the prefetch window length {length} exceeds its storage")]
    InvalidPrefetchLength { length: u8 },
    #[error("the pending write-page tracker is inconsistent")]
    InvalidWriteTracker,
}

/// An immutable, validated view of CPU execution state for canonical comparison.
///
/// This is not a restorable microarchitectural save state. Host pointers and transparent
/// execution caches are deliberately absent.
#[must_use]
pub struct CanonicalCpuExecution<'a> {
    cpu: &'a CpuGsw,
}

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

fn live_tlb_entries(cpu: &CpuGsw) -> [Option<TlbEntry>; TLB_ENTRIES] {
    let mut live = [None; TLB_ENTRIES];
    for (slot, entry) in cpu.tlb.entries.iter().copied().enumerate() {
        if entry.generation == cpu.tlb.generation && Tlb::slot(entry.tag) == slot {
            live[slot] = Some(entry);
        }
    }
    live.sort_unstable_by_key(|entry| match entry {
        Some(entry) => (false, entry.tag),
        None => (true, 0),
    });
    live
}

impl CanonicalCpuExecution<'_> {
    /// Writes version 1 of the validated CPU execution payload.
    pub fn write_payload(
        &self,
        out: &mut CanonicalFieldWriter<'_>,
    ) -> Result<(), CanonicalStateError> {
        let cpu = self.cpu;
        out.write_u32(cpu.registers.eflags)?;
        out.write_u32(cpu.pending_flags.tag)?;
        out.write_u32(cpu.pending_flags.a)?;
        out.write_u32(cpu.pending_flags.b)?;
        out.write_u32(cpu.pending_flags.result)?;
        out.write_u64(cpu.elapsed_clocks)?;
        out.write_u64(cpu.timing_rem)?;
        out.write_u64(cpu.fp_rem)?;
        out.write_bool(cpu.halted)?;
        out.write_bool(cpu.interrupt_shadow)?;

        let live_tlb = live_tlb_entries(cpu);
        out.write_count(live_tlb.iter().flatten().count() as u64)?;
        for entry in live_tlb.into_iter().flatten() {
            out.write_u32(entry.tag)?;
            out.write_u32(entry.phys)?;
            out.write_bool(entry.writable)?;
            out.write_bool(entry.user)?;
            out.write_bool(entry.dirty)?;
        }

        let cs = cpu.registers.cs();
        let current_linear =
            (cpu.registers.eip <= cs.limit).then(|| cs.base.wrapping_add(cpu.registers.eip));
        let prefetch_can_serve = current_linear
            .and_then(|linear| cpu.prefetch.get(cs, linear))
            .is_some();
        let pending_prefetch_invalidation = prefetch_can_serve
            && (cpu.written_pages_overflow
                || cpu.written_pages[..usize::from(cpu.written_count)]
                    .iter()
                    .flatten()
                    .any(|page| *page == cpu.prefetch.physical_base >> 12));
        let prefetch_present = prefetch_can_serve && !pending_prefetch_invalidation;
        out.write_bool(pending_prefetch_invalidation)?;
        out.write_bool(prefetch_present)?;
        if prefetch_present {
            write_segment(out, cpu.prefetch.cs)?;
            out.write_u32(cpu.prefetch.linear_base)?;
            out.write_u32(cpu.prefetch.physical_base)?;
            out.write_count(u64::from(cpu.prefetch.len))?;
            out.write_raw_bytes(&cpu.prefetch.bytes[..usize::from(cpu.prefetch.len)])?;
        }
        Ok(())
    }
}

impl CpuGsw {
    /// Validates and borrows the CPU state used by the compare-only execution payload.
    pub fn canonical_execution_capture(
        &self,
    ) -> Result<CanonicalCpuExecution<'_>, CpuCanonicalCaptureError> {
        if self.rep_resume_active
            || self.rep_execution.resume.is_some()
            || self.rep_execution.budget.is_some()
            || self.rep_execution.yielded
        {
            return Err(CpuCanonicalCaptureError::ActiveRepContinuation);
        }
        let expected_alignment =
            self.control.cr0 & CR0_AM != 0 && self.registers.eflags & FLAG_AC != 0;
        if self.alignment_armed != expected_alignment {
            return Err(CpuCanonicalCaptureError::InconsistentAlignmentCache);
        }
        if usize::from(self.prefetch.len) > PREFETCH_WINDOW_BYTES {
            return Err(CpuCanonicalCaptureError::InvalidPrefetchLength {
                length: self.prefetch.len,
            });
        }
        let written_count = usize::from(self.written_count);
        let packed_write_pages = written_count <= self.written_pages.len()
            && self.written_pages[..written_count]
                .iter()
                .all(Option::is_some)
            && self.written_pages[written_count..]
                .iter()
                .all(Option::is_none);
        let valid_overflow =
            !self.written_pages_overflow || written_count == self.written_pages.len();
        if !packed_write_pages || !valid_overflow {
            return Err(CpuCanonicalCaptureError::InvalidWriteTracker);
        }
        Ok(CanonicalCpuExecution { cpu: self })
    }

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
#[path = "canonical_state_test.rs"]
mod tests;
