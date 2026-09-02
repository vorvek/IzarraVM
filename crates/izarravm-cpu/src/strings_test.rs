// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const ALL_STRING_OPS: [StringOp; 7] = [
    StringOp::Movs,
    StringOp::Cmps,
    StringOp::Scas,
    StringOp::Stos,
    StringOp::Lods,
    StringOp::Ins,
    StringOp::Outs,
];

const ALL_PERSONAS: [(GswMode, CpuPersona); 3] = [
    (GswMode::Gsw386, CpuPersona::I386),
    (GswMode::Gsw486, CpuPersona::I486),
    (GswMode::Gsw586, CpuPersona::I586),
];

/// `level_timing`'s literals, pinned. `rep_core_upper`'s match carries these two pairs verbatim
/// rather than reading `level_timing` at a runtime cost; if a future change to `level_timing`
/// moves either pair without the match following, this fails at COMPILE time rather than after
/// the match has silently stopped agreeing with it.
const _: () = assert!(matches!(level_timing(CpuPersona::I386), (2, 5)));
const _: () = assert!(matches!(level_timing(CpuPersona::I486), (1, 12)));
const _: () = assert!(matches!(level_timing(CpuPersona::I586), (1, 12)));

/// T11 (code-smell batch 2, S2), corrected per the adversarial review's finding 9: the oracle
/// calls `level_timing_for_test`, never a hardcoded copy of its literals, so this is blind to
/// nothing -- a future change to `level_timing` that `rep_core_upper`'s match does not follow
/// makes this fail, rather than comparing two copies of the same numbers written by the same
/// change.
///
/// Three personas times seven `StringOp` variants times `rep_resume_active` in {false, true}:
/// 42 cases, matching `rep_chunk_limit` and `rep_budget_exhausted`'s shared `rep_core_upper`
/// call exactly.
#[test]
fn rep_core_upper_matches_the_pre_slice_divide_for_every_reachable_input() {
    for &(mode, persona) in &ALL_PERSONAS {
        let (num, den) = level_timing_for_test(persona);
        for &op in &ALL_STRING_OPS {
            for &resume_active in &[false, true] {
                let mut cpu = CpuGsw::default();
                cpu.set_mode(mode);
                cpu.rep_resume_active = resume_active;

                // The pre-slice expression, deliberately re-typed here rather than reused, with
                // (num, den) read from level_timing_for_test rather than hardcoded.
                let oracle = if resume_active {
                    0
                } else {
                    u64::from(CpuGsw::rep_core_clocks(op))
                        .saturating_mul(u64::from(num))
                        .saturating_add(u64::from(den) - 1)
                        / u64::from(den)
                };
                assert_eq!(
                    cpu.rep_core_upper(op),
                    oracle,
                    "{persona:?} {op:?} resume_active={resume_active}"
                );
            }
        }
    }
}

/// Review finding 11: the 21 `rep_resume_active = true` cases in the identity test above all
/// collapse to `0 == 0` and exercise no divide at all. Restated as its own assertion so a reader
/// does not have to notice that on their own; the 21 `false` cases are the ones that matter for
/// mutation coverage.
#[test]
fn rep_core_upper_collapses_to_zero_exactly_when_resuming() {
    for &(mode, _persona) in &ALL_PERSONAS {
        for &op in &ALL_STRING_OPS {
            let mut cpu = CpuGsw::default();
            cpu.set_mode(mode);
            cpu.rep_resume_active = true;
            assert_eq!(
                cpu.rep_core_upper(op),
                0,
                "resume_active must zero core_upper"
            );
        }
    }
}

/// The third of the three bails that keep the bulk scratch buffer's aliasing surface nil
/// (code-smell batch 2, S4; the other two, DF=1 and overlap, are pinned in
/// cpu_strings_segments_test.rs, which has the bus access this one does not need).
/// `string_forward_chunk_iterations` clamps `max_bytes` to `0x1000 - (linear & 0x0fff)`, so its
/// result can never exceed one page (4096 bytes) regardless of how large CX is -- exactly the
/// bound RepBulkScratch's fixed 4 KiB size assumes. Swept across every byte offset within a page
/// and every width, with CX large enough that the clamp, not CX, is always what binds.
#[test]
fn string_forward_chunk_iterations_never_exceeds_one_page() {
    let mut cpu = CpuGsw::default();
    cpu.registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::flat(0x10, 0x93));

    for width in [BusWidth::Byte, BusWidth::Word, BusWidth::Dword] {
        let huge_count = u32::MAX / width.bytes();
        for page_offset in [0u32, 1, 2, 4, 0x0ff0, 0x0ffc, 0x0ffd, 0x0ffe, 0x0fff] {
            for base in [0u32, 0x1000, 0x2000, 0x1_0000] {
                let offset = base.wrapping_add(page_offset);
                let iterations = cpu.string_forward_chunk_iterations(
                    SegmentIndex::Ds,
                    offset,
                    AddressSize::Dword,
                    width,
                    huge_count,
                );
                let bytes = iterations as usize * width.bytes() as usize;
                assert!(
                    bytes <= 4096,
                    "{width:?} at offset {offset:#x}: {bytes} bytes exceeds the one-page clamp \
                     RepBulkScratch's fixed size assumes"
                );
            }
        }
    }
}
