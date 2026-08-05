// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The Direct backend's structural-stop census and the diagnostic reporting surface on
//! `JitState`: the per-barrier rows, the unbound-exit and dynamic-miss class tallies, and the
//! stall snapshot. Split out of `direct.rs` verbatim to keep that file under the source-line
//! ceiling; nothing here changed but the visibility the module boundary forces.

use super::*;

// ---------------------------------------------------------------------------------------
// The compile-walk side of the census: the structural-stop recorder and the forward scan that
// prices what a barrier costs the block behind it. Moved out of `direct.rs` when the
// attribution-completeness slice gave the recorder two more call sites, so the file that is
// near its source-line ceiling carries only the three call sites and none of the machinery.
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct CensusSuffix {
    instructions: usize,
}

/// The compile walk's live accumulators at the moment a barrier stopped it, handed to the forward
/// scan so the scan can stop where the real walk would.
///
/// A STRUCT rather than more positional arguments. `record_structural_barrier` carried eleven,
/// two of them adjacent `u32`s (`entry_lin` and `next`) that an earlier review of this campaign
/// caught being confused for one another. Every accumulator here is a small integer and three are
/// `u8`, so positional passing had no type safety left to offer.
///
/// `scan_start` rides here for that reason specifically rather than for tidiness: it is the second
/// of those two `u32`s, and naming it at every call site is what stops the confusion recurring.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct SuffixSeed {
    /// Linear address the forward scan begins at: the instruction AFTER the barrier, which is
    /// where the counterfactual block would carry on.
    pub(super) scan_start: u32,
    /// Slots the block had already committed. The forward scan's slot count is
    /// `prefix_instructions + 1 + suffix`: the prefix, the barrier itself counterfactually
    /// lowered, and the suffix so far.
    pub(super) prefix_instructions: usize,
    pub(super) stack_accesses: u8,
    pub(super) memory_alu_slots: u8,
    pub(super) callout_slots: u8,
    /// Segments the block had already overwritten when the barrier stopped it.
    pub(super) dirty_segments: u8,
    /// Whether the forward scan applies the dirty-segment rule at all.
    ///
    /// TRUE for every arm but one, because the rule survives lowering the opcode those arms name:
    /// admitting `RCL r16,1` does not make a baked segment base any less stale.
    ///
    /// FALSE for `BarrierStop::DirtySegment`, and that arm is the whole reason this field exists.
    /// Its suffix answers "how much longer would this block be if segment bases were DYNAMIC",
    /// and disabling the rule is precisely what that change means. Left true, the scan would walk
    /// forward from the barred instruction with the segment already dirty, stop at the first
    /// following slot that pins it, and report a suffix near zero. The instrument built to rank
    /// dynamic segment bases would then report that dynamic segment bases gain nothing.
    pub(super) model_dirty: bool,
}

/// The segment a BARRED instruction would have overwritten, as a bitmask.
///
/// The forward scan counts the barrier as one slot of the counterfactual block, so if that
/// instruction is itself a segment load then everything behind it runs with the segment already
/// dirty. Without this the suffix over-reports for exactly the rows a segment slice would rank:
/// `0x1f` POP DS alone is 10.3% of the census.
///
/// Read off the `DecodedInsn` and not off a `DirectKind`, because there is no kind: these are the
/// forms `classify` refused, which is why they are barriers. `DirectKind::written_segment` covers
/// the admitted ones (`LoadSegReal`) and this covers the rest, so the two are complements rather
/// than duplicates.
///
/// `0x8e /1` is absent deliberately. `MOV CS, r/m16` raises #GP(0) rather than loading anything
/// (execute.rs, the 0x8e arm), so it writes no segment and seeding one would model a transfer the
/// guest never performs.
fn barred_segment_write(insn: &DecodedInsn) -> u8 {
    let segment = match insn.opcode {
        // The register forms of `/0` and `/3` are lowered to `LoadSegReal`; the memory forms and
        // the other three register forms are not, and all of them land here.
        0x8e => match insn.modrm.map(|modrm| modrm.reg) {
            Some(0) => SegmentIndex::Es,
            Some(2) => SegmentIndex::Ss,
            Some(3) => SegmentIndex::Ds,
            Some(4) => SegmentIndex::Fs,
            Some(5) => SegmentIndex::Gs,
            _ => return 0,
        },
        // POP Sreg, then the two-byte POP FS/GS.
        0x07 => SegmentIndex::Es,
        0x17 => SegmentIndex::Ss,
        0x1f => SegmentIndex::Ds,
        0x0fa1 => SegmentIndex::Fs,
        0x0fa9 => SegmentIndex::Gs,
        // The far-pointer loads. Each writes its segment and a GPR in one instruction.
        0xc4 => SegmentIndex::Es,
        0xc5 => SegmentIndex::Ds,
        0x0fb2 => SegmentIndex::Ss,
        0x0fb4 => SegmentIndex::Fs,
        0x0fb5 => SegmentIndex::Gs,
        _ => return 0,
    };
    segment_bit(segment)
}

/// Record one structural stop into the barrier census, from ANY of the arms that can set
/// `CompileStop::Structural`.
///
/// Those arms are the whole population of rejections, by construction rather than by
/// measurement: `BlockState::Rejected` is installed only by `run.rs`'s
/// `CompileOutcome::StructuralReject` arm; that outcome is produced only by the short-block
/// return at the end of `compile_with_instruction_limit`; and `CompileStop::Structural` is set
/// only at the prefix / non-continuable arm, the Word-persona arm and the `HardBoundary` arm.
/// Only the last was instrumented until the attribution-completeness slice, which is why doom's
/// attributed row sum had fallen to 0.97% of its rejected class while the campaign was about to
/// declare the row work finished.
///
/// CALL-SITE GATED like every other census hook ([[default-off-instruments-tax-hot-path]]): each
/// caller checks `barrier_census_enabled()` before building a single argument, because
/// `census_native_suffix` walks the decode cache forward. Callers also require the FULL-length
/// pass — `compile_with_page_len` re-enters this walk once per binary-search step and converts
/// any structural reject it sees into a `Retry`, so a shorter pass can neither install a
/// rejection nor be allowed to double-count a row.
pub(super) fn record_structural_barrier(
    cpu: &mut CpuGsw,
    insn: &DecodedInsn,
    stop: BarrierStop,
    key: BlockKey,
    entry_lin: u32,
    d: bool,
    mut seed: SuffixSeed,
) {
    // Folded in HERE rather than at the three call sites, so a future arm cannot forget it. The
    // call sites carry the compile walk's live mask; this adds the barred instruction's own write,
    // which the walk never reached.
    seed.dirty_segments |= barred_segment_write(insn);
    let suffix = census_native_suffix(cpu, key, entry_lin, d, seed);
    cpu.jit_direct.record_barrier(
        insn,
        BarrierObservation {
            entry_linear: entry_lin,
            native_prefix: seed.prefix_instructions,
            native_suffix: suffix.instructions,
            stop,
        },
    );
}

/// Walk forward from a barrier and count how many more instructions the block WOULD have carried
/// had that barrier been lowered.
///
/// This is `max_native_suffix`'s and `native_suffix_instructions`' only source, which makes it a
/// ranking column: a row that reports a long suffix is claiming a long block is being lost. So the
/// scan has one correctness property, and it is not "does it look like the compile walk" but
/// **does it stop where the compile walk would**. Anywhere the two disagree, the column lies in a
/// direction the reader cannot see.
///
/// The audit that produced the current form found SIX divergences. Three are closed here, one is
/// deliberately left open, and two belong to the dirty-segment slice:
///
/// * CLOSED, the memory-ALU BLOCK cap. `compile_with_instruction_limit` breaks at its LOOP TOP on
///   `memory_alu_slots != 0 && slots.len() == MAX_MEMORY_ALU_BLOCK_INSTRUCTIONS`, regardless of
///   what the next instruction turns out to be. This scan applied the same bound only when the
///   next kind was ITSELF memory-ALU, so a barrier whose prefix held one read-modify-write slot
///   over-reported by up to 28 instructions against a 32-instruction ceiling. That was the largest
///   error in the column and it is the one the 32-bit rows were ranked on.
/// * CLOSED, the call-out slot cap.
/// * CLOSED, `jit_admits_non_continuable`. The compile walk takes `insn.continuable ||
///   jit_admits_non_continuable(opcode)`; this scan took the bare flag, truncating any suffix that
///   reached an IMUL-with-immediate.
/// * OPEN BY DESIGN, x87. The `kind.is_x87()` break below refuses EVERY x87 slot, while the
///   compile walk admits up to `MAX_X87_SLOTS` within `MAX_X87_BLOCK_INSTRUCTIONS`. That makes the
///   x87 block cap and the x87/call-out mixing refusal unreachable here, so mirroring them would
///   be dead code no test could make fire. Left as a deliberate conservative FLOOR: it
///   under-reports, and an under-report cannot inflate a ranking. Closing it is a Quake-facing
///   change with no bearing on the 16-bit campaign.
/// * CLOSED, the dirty-segment rule, but PER ARM rather than unconditionally. See
///   `SuffixSeed::model_dirty`: every arm but `DirtySegment` models it, and that one must not,
///   because its suffix prices the rule's own removal. The seed also carries the barred
///   instruction's own segment write, which the compile walk never reached
///   (`barred_segment_write`).
///
/// One residual the seed cannot express: the barrier is counted as one slot without knowing its
/// kind, because `classify` refused it. So a barrier that would itself have been the block's first
/// memory-ALU or call-out slot does not arm those caps. Bounded by one slot and always in the
/// over-reporting direction.
fn census_native_suffix(
    cpu: &CpuGsw,
    key: BlockKey,
    entry_lin: u32,
    d: bool,
    seed: SuffixSeed,
) -> CensusSuffix {
    let SuffixSeed {
        scan_start: mut lin,
        prefix_instructions,
        mut stack_accesses,
        mut memory_alu_slots,
        mut callout_slots,
        mut dirty_segments,
        model_dirty,
    } = seed;
    let cs = cpu.registers.cs();
    let mut result = CensusSuffix::default();
    while prefix_instructions + 1 + result.instructions < MAX_BLOCK_INSTRUCTIONS {
        // The compile walk's LOOP-TOP caps, which fire before the next instruction is even
        // decoded and so cannot be folded into the per-kind refusals below. `>=` rather than the
        // compile walk's `==` because `prefix_instructions` arrives from a caller rather than
        // being counted up from zero here; the two agree wherever the compile walk can reach.
        if memory_alu_slots != 0
            && prefix_instructions + 1 + result.instructions >= MAX_MEMORY_ALU_BLOCK_INSTRUCTIONS
        {
            break;
        }
        let Some(insn) = cpu.decode_cache.get(lin, d) else {
            break;
        };
        let insn_len = u32::from(insn.len);
        let Some(next) = (insn_len != 0).then(|| lin.checked_add(insn_len)).flatten() else {
            break;
        };
        let slot_eip = lin.wrapping_sub(cs.base);
        if slot_eip
            .checked_add(insn_len - 1)
            .is_none_or(|last| last > cs.limit)
            || entry_lin >> BLOCK_PAGE_SHIFT != next.wrapping_sub(1) >> BLOCK_PAGE_SHIFT
        {
            break;
        }
        let Some(expected_phys) = key.physical.checked_add(lin.wrapping_sub(entry_lin)) else {
            break;
        };
        if expected_phys
            .checked_add(insn_len - 1)
            .is_none_or(|last| key.physical >> BLOCK_PAGE_SHIFT != last >> BLOCK_PAGE_SHIFT)
            || cpu.decode_cache.line_phys_start(lin, d) != Some(expected_phys)
            || !prefixes_supported_for(insn.prefixes, insn.operand_size, d)
            || !(insn.continuable || jit_admits_non_continuable(insn.opcode))
            || (insn.operand_size == OperandSize::Word && !word_operands_admitted(cpu))
        {
            break;
        }
        let PlannedInsn::Native(kind) = DirectUnitPlanner::classify(&insn, lin, entry_lin) else {
            break;
        };
        let Some(kind) = stack_width_kind(cpu, kind, insn.operand_size) else {
            break;
        };
        if kind.is_x87()
            || !static_control_target_within_limit(
                kind,
                entry_lin.wrapping_sub(cs.base),
                control_target_limit(insn.operand_size, cs.limit),
            )
            || !kind_segment_access_supported(cpu, kind)
            || (kind.is_memory_alu()
                && (memory_alu_slots == MAX_MEMORY_ALU_SLOTS
                    || prefix_instructions + 1 + result.instructions
                        >= MAX_MEMORY_ALU_BLOCK_INSTRUCTIONS))
            || (kind.is_call_out() && callout_slots == MAX_BLOCK_CALLOUT_SLOTS)
            || (kind.uses_stack() && stack_accesses == MAX_BLOCK_STACK_ACCESSES)
            || (model_dirty && kind.pinned_segments() & dirty_segments != 0)
        {
            break;
        }
        stack_accesses += u8::from(kind.uses_stack());
        memory_alu_slots += u8::from(kind.is_memory_alu());
        callout_slots += u8::from(kind.is_call_out());
        // Accumulated even when `model_dirty` is false. The mask costs nothing unread, and a scan
        // that tracked it only when it was going to test it would go wrong the moment the flag
        // became anything other than a constant per arm.
        if let Some(segment) = kind.written_segment() {
            dirty_segments |= segment_bit(segment);
        }
        result.instructions += 1;
        lin = next;
        if kind.is_terminal() {
            break;
        }
    }
    result
}

// ---------------------------------------------------------------------------------------
// The stall/census taxonomy. Moved VERBATIM out of `direct.rs` (source-line ceiling) to sit
// beside `stall_snapshot` and `snapshot`, the two builders that already read every one of
// these. `direct.rs` re-exports the whole set, so no path outside this module changed.
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct BlockCacheStats {
    pub hot_hits: u64,
    pub hash_hits: u64,
    pub lookup_misses: u64,
    pub cache_resets: u64,
    pub arena_compactions: u64,
    pub arena_compaction_live_blocks: u64,
    pub arena_compaction_bytes: u64,
    pub arena_compaction_failures: u64,
    pub links: u64,
    pub unlinks: u64,
    pub decode_dependencies_scanned: u64,
    pub portals_hidden: u64,
}

/// The stall tallies, deliberately NOT part of `BlockCacheStats`.
///
/// `BlockCacheStats` is DRAINED by `take_stats` on every dispatcher exit and folded into
/// `PerfCounters`; a field added there is zeroed before anything can read it back off the cache.
/// These three groups have no `PerfCounters` home to be folded into -- growing that struct shifts
/// the pinned `pending_flags` offset (see the pin tests in cpu_test.rs/canonical_state_test.rs
/// for the current value) -- so they live here, accumulate for the whole run,
/// and are read directly by `stall_snapshot`. Not cleared by `reset_storage` either: a diagnostic
/// that reset itself mid-run would under-report exactly the pathological runs it exists for.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DirectStallTally {
    /// Why a compile attempt parked its key `Dormant` instead of installing, indexed by
    /// `DormantReason`. Before this split all four reasons folded into one terminal state that
    /// `classify_unbound_target` then reported as `SeenNotCompiled`, indistinguishable from a key
    /// that had simply never been hot enough to try -- which is what made the 5.2M-exit
    /// seen-not-compiled bucket unattributable.
    pub dormant: [u64; DormantReason::COUNT],
    /// Why `try_link_inner` refused an edge, indexed by `LinkRefusal`. Every one of these leaves
    /// the source cell on the zero portal, so the exit reports `StaticUnbound` and is
    /// indistinguishable from a target that was never compiled.
    pub link_refusals: [u64; LinkRefusal::COUNT],
    /// Which cause cleared each link, indexed by `LinkClearCause`. The aggregate
    /// `jit_direct_links_cleared` counts the same events through `BlockCacheStats::unlinks` and
    /// is left alone; these three are the attribution the aggregate could not carry. Always on:
    /// each is one increment beside an increment that already happens, on paths that are already
    /// doing map work.
    pub links_cleared: [u64; LinkClearCause::COUNT],
    /// The two halves the old `SideExitReason::Other` counter conflated.
    pub side_exit_segment_limit: u64,
    pub side_exit_x87_eligibility: u64,
    /// Lowered DIV/IDIV guard refusals. Always-on evidence that the guard is COLD: a
    /// divide-by-zero is a crash on a healthy guest, so a nonzero count is either a fault the
    /// guest handles or the conservative divisor == -1 arm, and the ratio against
    /// `jit_direct_insns` is what says whether that arm ever needs an exact form.
    pub side_exit_divide_guard: u64,
    /// The two interpreter call-out exit shapes, split because they mean opposite things: a step
    /// break is the mechanism working (a port touched device state, the run ends where an
    /// interpreted continuation would), an abnormal is the helper refusing and the interpreter
    /// re-running the instruction. Lumping them into `jit_direct_exit_other` would have made the
    /// slice's own mechanism unreadable, which is the mistake `Other` already cost this campaign.
    pub side_exit_callout_step_break: u64,
    pub side_exit_callout_abnormal: u64,
    /// Every interpreter call-out the helper entered, counted before it can refuse. The
    /// DENOMINATOR the two counters above needed: an abnormal count of zero says nothing without
    /// it, because zero is also what a mechanism that never ran reports.
    pub callout_executed: u64,
    /// Entries refused because a call-out-bearing block met the privilege state whose port reads
    /// consult the TSS bitmap. Zero on a guest that never runs a compiled IN at CPL>IOPL or in
    /// V86, which is the isolation claim for the whole call-out slice on the shipped fixtures.
    pub reject_callout_privileged: u64,
    /// Dispatcher entries whose HEAD block was barred from publishing successors because it
    /// overwrites a segment register, and the instructions those entries retired.
    ///
    /// The name says `head` because that is the honest limit of what this can see, and the limit
    /// matters more than the number. `run.rs` increments the entry counter once per DISPATCHER
    /// ENTRY, attributed to the block the dispatcher entered. Inbound links to a segment-write
    /// block are deliberately preserved (only its OUTBOUND edges are barred), so a segment-write
    /// block reached as the tail of a chain increments nothing here. The undercount is exactly the
    /// share of them that have an inbound link, which is not known.
    ///
    /// It is therefore an UPPER BOUND on removable entries read one way and an undercount of
    /// affected blocks read another, and it must be reported against `jit_direct_entries` and
    /// `jit_direct_chain_quota_entries` rather than alone. What lifting the bar would actually
    /// remove is the SUCCESSOR's separate entry, which is counted under the successor's own
    /// identity and is invisible from here; and some of the removed seams would reappear as
    /// budget-quota exhaustion, because a longer chain drains `budget_quota` faster.
    ///
    /// The instruction lane is the useful half: `insns / entries` for this population against the
    /// global figure says directly whether these blocks are short because they cannot chain.
    pub segment_write_block_head_entries: u64,
    pub segment_write_block_head_insns: u64,
}

/// The four terminal states a non-structural compile failure can land in. Threaded from the three
/// `dormant()` call sites plus the heat demotion so the counter names a mechanism rather than an
/// outcome; see `BlockCacheStats::dormant`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DormantReason {
    /// `CompileOutcome::Retry`. Twenty distinct `CompileStop::Retry` sites fold into this one
    /// value, so it is still coarse -- but it separates "the compiler gave up" from the three
    /// post-compile gates below, which is the split that decides whether retrying is worth
    /// anything.
    CompileRetry,
    /// G4: no single RAM direct page covers the block's whole physical span.
    PageCoverFailed,
    /// G1: the span's SMC heat is hot at install time. This one DOES carry a heat stamp and so
    /// has a designed recovery path through `lift_cold_smc_dormant`; the other three do not.
    SpanHot,
    /// The arena refused the allocation.
    InstallFailed,
}

impl DormantReason {
    pub(crate) const COUNT: usize = 4;
    pub(crate) const ALL: [Self; Self::COUNT] = [
        Self::CompileRetry,
        Self::PageCoverFailed,
        Self::SpanHot,
        Self::InstallFailed,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::CompileRetry => "compile_retry",
            Self::PageCoverFailed => "page_cover_failed",
            Self::SpanHot => "span_hot",
            Self::InstallFailed => "install_failed",
        }
    }
}

/// The six ways `try_link_inner` can refuse. Ordered as the function tests them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LinkRefusal {
    /// Source or target is no longer an active index.
    Inactive,
    /// One end predates the current link epoch.
    StaleEpoch,
    /// `SegmentLayout::link_compatible` refused.
    SegmentLayout,
    /// `CompiledBlock::link_compatible` refused.
    BlockShape,
    /// The RET-PIC-only strict `has_x87` equality, INTEGER source into a FLOAT target. RETIRED,
    /// and kept only so the counter can prove it: `emit_completed_dynamic_path` now selects
    /// `BlockPortal::integer_entry` for an integer source, which is the shared x87 re-entry pad
    /// for a float target, so nothing refuses for this reason any more and this must read zero.
    /// A non-zero value means an edge reached the refusal without going through the pad.
    DynamicIntegerToFloat,
    /// The same equality, FLOAT source into an INTEGER target. RETIRED on the same terms:
    /// `emit_completed_dynamic_path` emits the boundary spill that `link_compatible`'s
    /// float-to-integer case relies on, so this must read zero too. The two are kept apart rather
    /// than merged because they were fixed by two different mechanisms in two commits, and a
    /// regression in either one should name itself.
    DynamicFloatToInteger,
    /// Integer source into a float target with no x87 re-entry pad built.
    MissingX87Pad,
}

impl LinkRefusal {
    pub(crate) const COUNT: usize = 7;
    pub(crate) const ALL: [Self; Self::COUNT] = [
        Self::Inactive,
        Self::StaleEpoch,
        Self::SegmentLayout,
        Self::BlockShape,
        Self::DynamicIntegerToFloat,
        Self::DynamicFloatToInteger,
        Self::MissingX87Pad,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Inactive => "inactive",
            Self::StaleEpoch => "stale_epoch",
            Self::SegmentLayout => "segment_layout",
            Self::BlockShape => "block_shape",
            Self::DynamicIntegerToFloat => "dynamic_integer_to_float",
            Self::DynamicFloatToInteger => "dynamic_float_to_integer",
            Self::MissingX87Pad => "missing_x87_pad",
        }
    }
}

/// Result of `classify_unbound_target`. Diagnostic.
///
/// EXHAUSTIVE AND MUTUALLY EXCLUSIVE by construction: every unbound-exit classification call
/// lands on exactly one variant, including the `NoKey` early-out, so the per-run totals sum to
/// the unresolved-exit counter the classifier is gated behind. `unbound_target_classes_are_exhaustive`
/// (direct_test.rs) pins the state-to-class mapping, and `unbound_exit_classes_sum_to_the_static_unbound_counter`
/// plus `dynamic_miss_classes_sum_to_the_dynamic_miss_counter` (cpu_jit_direct_execution_test.rs)
/// pin the sum on the real dispatcher path for both lanes. Do not add a classification path that
/// returns without noting a variant -- that is exactly what the second pair of tests exists to
/// catch, and the first pair cannot see it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UnboundTarget {
    /// The exiting EIP could not be turned into a `BlockKey` at all (`key_for` refused, e.g. the
    /// successor page is not mapped). No entry-map probe happened, so this is not `Absent`: it
    /// is "the question could not be asked". Exists so the classes close on the exit counter.
    NoKey,
    /// Probed for, but the successor address has no entry at all — a genuinely cold edge.
    Absent,
    /// The key is tracked and admissible, just not hot enough to have been compiled yet.
    Seen,
    /// Parked `Dormant` by the G1 SMC-heat gate (`DormantReason::SpanHot`). Split out from the
    /// other three dormant reasons because this is the only one with a designed recovery path
    /// (`lift_cold_smc_dormant`), so it is churn rather than a permanent refusal.
    DormantHeat,
    /// Parked `Dormant` by any non-heat reason — compile retry, page-cover failure, or an arena
    /// install failure. None of these lift on their own.
    DormantOther,
    /// Compilation was attempted and structurally refused. These are the edges an opcode
    /// lowering slice would convert.
    Rejected,
    /// The target is compiled and live, but the edge was never linked — a `link_compatible`
    /// refusal, or the transient window before the next probe binds it.
    Compiled,
    /// The target compiled once and its slot has since been retired or reused. Currently
    /// UNREACHABLE and reading zero on both fixtures by construction, not by luck: every retire
    /// path rewrites or removes the entry before anything can exit to it, so a live
    /// `Compiled(id)` whose slot is dead has no way to be observed today. Retained as a
    /// tripwire -- a future slot-reuse or deferred-retire path would surface here first, and it
    /// is cheaper to keep the class than to re-derive it. Do NOT read its zero as evidence
    /// about retirement.
    CompiledRetired,
}

impl UnboundTarget {
    // `pub(crate)` where the pre-move copy was private: the module boundary this extraction
    // introduced puts `direct_test.rs` outside it, and that file's
    // `unbound_target_classes_are_exhaustive` reads the constant. Matches `DormantReason::COUNT`
    // and `LinkRefusal::COUNT`, which were already `pub(crate)` for the same reason.
    pub(crate) const COUNT: usize = 8;
    pub(crate) const ALL: [Self; Self::COUNT] = [
        Self::NoKey,
        Self::Absent,
        Self::Seen,
        Self::DormantHeat,
        Self::DormantOther,
        Self::Rejected,
        Self::Compiled,
        Self::CompiledRetired,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::NoKey => "no_key",
            Self::Absent => "absent",
            Self::Seen => "seen",
            Self::DormantHeat => "dormant_heat",
            Self::DormantOther => "dormant_other",
            Self::Rejected => "rejected",
            Self::Compiled => "compiled",
            Self::CompiledRetired => "compiled_retired",
        }
    }
}

/// Which of the four unlink sites cleared a link, for the split behind the aggregate
/// `jit_direct_links_cleared`. That aggregate is NOT re-derived from these: it keeps its own
/// independent `BlockCacheStats::unlinks` feed, so the two are a cross-check rather than one
/// number counted twice. `link_clear_causes_close_on_the_aggregate` (direct_test.rs) pins the
/// sum identity, site by site.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LinkClearCause {
    /// `try_link_inner` displaced an existing edge in a cell it wanted for a different target.
    Replaced,
    /// The target (or source) block was retired, via `unlink_block`.
    Retired,
    /// `invalidate_translation`: a paging/CR3/decode-slot flush tearing down link cells while
    /// the blocks themselves STAY compiled. Split from `Reset` because the two look alike in an
    /// aggregate and are nothing alike as a lever -- this one leaves the code in place and only
    /// costs the re-binding, and it is where essentially all bulk clears actually come from
    /// (`jit_direct_cache_resets` is a single-digit number over a whole fixture run).
    Flushed,
    /// `reset_storage`: the cache-wide drop, code and all. Rare; counted apart so a rise in it
    /// cannot hide inside the flush lane.
    Reset,
}

impl LinkClearCause {
    pub(crate) const COUNT: usize = 4;
    pub(crate) const ALL: [Self; Self::COUNT] =
        [Self::Replaced, Self::Retired, Self::Flushed, Self::Reset];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Replaced => "replaced",
            Self::Retired => "retired",
            Self::Flushed => "flushed",
            Self::Reset => "reset",
        }
    }
}

/// WHICH structural-stop arm of the compile walk refused the block.
///
/// Added by the attribution-completeness slice. Before it, `record_barrier` fired on the
/// `HardBoundary` arm ALONE, so the two other arms that produce a `CompileStop::Structural` --
/// and therefore a `BlockState::Rejected` that static exits pile into -- installed a rejected
/// span with no census row at all. That was tolerable when `HardBoundary` was 78% of doom's
/// rejected class and fatal by the time lowering slices had pushed it under 1%: "every row is
/// under the stop floor" was about to be true while ~19.8M doom exits sat in a class the
/// instrument could not name.
///
/// Carried IN THE ROW KEY rather than as a per-row side tally, so the arms are separate rows and
/// each can be ranked, diffed and lowered independently. Pre-existing rows keep their identity:
/// every row the census produced before this slice was a `HardBoundary`, so the extra key field
/// is purely additive and a before/after row diff still lines up.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum BarrierStop {
    /// `classify` returned `PlannedInsn::HardBoundary` — the opcode-coverage arm, and the only
    /// one instrumented before this slice.
    HardBoundary,
    /// `prefixes_supported_for` refused: the backend takes the operand-size override and nothing
    /// else, so LOCK, REP/REPNE, an address-size override and **any explicit segment override**
    /// all stop the walk here. The row's `prefix_mask` names which.
    PrefixUnsupported,
    /// `DecodedInsn::continuable` is false — `block_continuable` (decode.rs) refused the shape
    /// for straight-line batching, and the compile walk inherits that refusal.
    NonContinuable,
    /// A Word-size instruction on a persona other than I586. Dead on both shipped fixtures (they
    /// run 586) and instrumented anyway, because "it reads zero" is a measurement and "nothing
    /// records it" is not.
    WordPersona,
    /// The dirty-segment rule: a slot wanted a segment an earlier slot in the same block had
    /// overwritten, so it would have baked a stale base or selector.
    ///
    /// THE ODD ONE OUT, in three ways that a reader ranking rows has to know about.
    ///
    /// It is a `CompileStop::Boundary` rather than a `Structural`, so the block it stopped is
    /// compiled and installed rather than rejected. That is why it does not install a rejected
    /// span (see `installs_rejected_span`) and why `unbound_exits` and `dynamic_unbound_exits`
    /// are structurally zero for it. `runtime_hits` is near zero as well, because the barred
    /// instruction becomes the ENTRY of the next block and gets compiled rather than retiring
    /// through the interpreter. `snapshot` sorts on those columns, so these rows land last while
    /// being the ones a segment slice is looking for. Read `hits` and the suffix instead.
    ///
    /// Its rows must be SUMMED, not ranked individually. The key is the shape of the instruction
    /// that was barred -- the segment's USER -- so one structural cause spreads itself across
    /// `0x8a`, `0x8b`, `0x88` and every other form that happens to touch the segment, rather than
    /// concentrating in one row the way an opcode-coverage barrier does.
    ///
    /// Its suffix is computed with the dirty rule DISABLED. See `SuffixSeed::model_dirty`.
    DirtySegment,
}

impl BarrierStop {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::HardBoundary => "hard_boundary",
            Self::PrefixUnsupported => "prefix_unsupported",
            Self::NonContinuable => "non_continuable",
            Self::WordPersona => "word_persona",
            Self::DirtySegment => "dirty_segment",
        }
    }

    /// Whether a stop on this arm leaves behind a `BlockState::Rejected` span that static and
    /// dynamic exits can pile into.
    ///
    /// Every `CompileStop::Structural` arm does. `DirtySegment` does not: it is a
    /// `CompileStop::Boundary`, and the key it leaves behind is Compiled, or Dormant when the
    /// break landed with too few slots to install (`compile_with_instruction_limit`'s short-block
    /// return). Never Rejected.
    ///
    /// So `record` must not write `rejected_barrier` for it. That map is keyed on entry linear
    /// alone, and a linear claimed by a non-rejected block would hand a genuinely rejected block's
    /// exits to the wrong row, in whichever direction the two happened to be recorded.
    ///
    /// Derived from the arm rather than passed in, because a call site that could get this wrong
    /// is a call site that eventually will.
    fn installs_rejected_span(self) -> bool {
        !matches!(self, Self::DirtySegment)
    }
}

/// The normalized instruction shape a barrier row is keyed on, WITHOUT the stop arm. Split out
/// of `BarrierKey` so `note_interpreted` — which sits on the per-interpreted-instruction retire
/// path — still costs exactly ONE map probe now that a shape can own up to four rows.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct BarrierShape {
    opcode: u16,
    modrm_reg: u8,
    operand_form: u8,
    operand_size: u8,
    address_size: u8,
    prefix_mask: u16,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct BarrierKey {
    shape: BarrierShape,
    stop: BarrierStop,
}

impl BarrierShape {
    fn from_insn(insn: &DecodedInsn) -> Self {
        let operand_form = match insn.operand {
            None => 0,
            Some(DecodedOperand::Reg(_)) => 1,
            Some(DecodedOperand::Mem(_)) => 2,
        };
        let mut prefix_mask = u16::from(insn.prefixes.operand_size_override)
            | (u16::from(insn.prefixes.address_size_override) << 1)
            | (u16::from(insn.prefixes.lock) << 2);
        prefix_mask |= match insn.prefixes.rep {
            None => 0,
            Some(crate::RepKind::Repe) => 1 << 3,
            Some(crate::RepKind::Repne) => 2 << 3,
        };
        if let Some(segment) = insn.prefixes.segment_override {
            prefix_mask |= (u16::try_from(segment_index(segment)).unwrap_or(0) + 1) << 5;
        }
        Self {
            opcode: insn.opcode,
            modrm_reg: insn.modrm.map_or(u8::MAX, |modrm| modrm.reg),
            operand_form,
            operand_size: u8::from(insn.operand_size == OperandSize::Dword),
            address_size: u8::from(insn.address_size == AddressSize::Dword),
            prefix_mask,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct BarrierStats {
    hits: u64,
    native_prefix_instructions: u64,
    native_suffix_instructions: u64,
    max_native_prefix: u8,
    max_native_suffix: u8,
    /// Exits that actually happened into a block this barrier rejected. RUNTIME-weighted, unlike
    /// `hits` (compile attempts) which mis-ranked the ShiftCl slice by three orders of magnitude.
    unbound_exits: u64,
    /// The same, for the DYNAMIC lane: a computed RET/JMP/CALL target whose inline cache missed
    /// into a block this barrier rejected. Slice 4 found that lane was 65% the size of the static
    /// one for the row it lowered and attributed to NOTHING, because `note_dynamic_miss_target`
    /// classified without ever looking the entry linear back up. Ranking a row on the static
    /// column alone under-prices it by whatever this column holds.
    dynamic_unbound_exits: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct BarrierObservation {
    pub(super) entry_linear: u32,
    pub(super) native_prefix: usize,
    pub(super) native_suffix: usize,
    pub(super) stop: BarrierStop,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DirectBarrierCensus {
    rows: HashMap<BarrierKey, BarrierStats>,
    /// Why static successor cells were unbound at the exits that hit them, indexed by
    /// `UnboundTarget`. Lives HERE and not in `PerfCounters` on purpose: `PerfCounters` is
    /// embedded in `CpuGsw` ahead of `pending_flags`, whose offset is pinned by
    /// `arch_payload_keeps_pending_flags_offset_pinned` (canonical_state_test.rs) and
    /// `pending_flags_offset` (cpu_test.rs) because emitted code bakes it. Growing `PerfCounters`
    /// for a diagnostic shifts that pin; the census is an `Option<Box<_>>` on `JitState` and costs
    /// the layout nothing.
    unbound: [u64; UnboundTarget::COUNT],
    /// The same classification for DYNAMIC successor misses (computed RET/JMP/CALL targets),
    /// kept in its own lane because the two have different fixes: a static unbound wants its
    /// target compiled, a dynamic miss whose target reads `CompiledButUnlinked` wants a wider
    /// inline cache than the hardcoded two ways.
    unbound_dynamic: [u64; UnboundTarget::COUNT],
    /// Block entry linear -> the barrier row that refused it, so a rejected-target exit can be
    /// attributed back to the opcode responsible. Keyed on linear alone: two rejected blocks
    /// sharing a linear across mode/physical would merge, which is acceptable for a diagnostic
    /// and keeps the compile-side insert to one word.
    rejected_barrier: HashMap<u32, BarrierKey>,
    /// Interpreted retirements per SHAPE, i.e. ignoring which arm stopped the walk. Held apart
    /// from `rows` so `note_interpreted` stays at one map probe: a shape can now own up to four
    /// rows (one per `BarrierStop`), and probing `rows` per arm would multiply the cost of the
    /// census's hottest hook by four. Every row of a shape reports this same total, which is the
    /// honest reading — `runtime_hits` counts executions of an instruction SHAPE and an
    /// executing instruction has no stop arm.
    runtime_hits: HashMap<BarrierShape, u64>,
}

impl DirectBarrierCensus {
    fn note_unbound(&mut self, kind: UnboundTarget) {
        self.unbound[kind as usize] += 1;
    }

    fn note_unbound_dynamic(&mut self, kind: UnboundTarget) {
        self.unbound_dynamic[kind as usize] += 1;
    }

    /// Attribute one rejected-target exit back to the barrier that refused that block.
    fn note_unbound_rejected_at(&mut self, linear: u32) {
        let Some(&key) = self.rejected_barrier.get(&linear) else {
            return;
        };
        let row = self.rows.entry(key).or_default();
        row.unbound_exits = row.unbound_exits.saturating_add(1);
    }

    /// The dynamic-lane counterpart. Same map, separate column: the two lanes have different
    /// fixes and Slice 4 showed they move by wildly different factors for the same row.
    fn note_dynamic_rejected_at(&mut self, linear: u32) {
        let Some(&key) = self.rejected_barrier.get(&linear) else {
            return;
        };
        let row = self.rows.entry(key).or_default();
        row.dynamic_unbound_exits = row.dynamic_unbound_exits.saturating_add(1);
    }

    fn record(&mut self, insn: &DecodedInsn, observation: BarrierObservation) {
        let BarrierObservation {
            entry_linear,
            native_prefix,
            native_suffix,
            stop,
        } = observation;
        let key = BarrierKey {
            shape: BarrierShape::from_insn(insn),
            stop,
        };
        if stop.installs_rejected_span() {
            self.rejected_barrier.insert(entry_linear, key);
        }
        // Register the shape so `note_interpreted` can find it with one probe and without ever
        // creating a row for an instruction that never barriered.
        self.runtime_hits.entry(key.shape).or_default();
        let row = self.rows.entry(key).or_default();
        row.hits = row.hits.saturating_add(1);
        row.native_prefix_instructions = row
            .native_prefix_instructions
            .saturating_add(native_prefix as u64);
        row.native_suffix_instructions = row
            .native_suffix_instructions
            .saturating_add(native_suffix as u64);
        row.max_native_prefix = row.max_native_prefix.max(native_prefix as u8);
        row.max_native_suffix = row.max_native_suffix.max(native_suffix as u8);
    }

    fn note_interpreted(&mut self, insn: &DecodedInsn) {
        // EVERY row, not only the ex-helper families. `runtime_hits` counts how many times the
        // guest actually EXECUTES this shape interpreted, which makes it the census's only
        // per-execution, position-free column - and therefore the only one that can rank a shape
        // by what it costs rather than by where a block happened to stop.
        //
        // It used to carry `&& row.helper_family.is_some()`, an artifact of the commit that
        // instrumented the three helper-eligible opcodes, and that one conjunct left 34 of 36
        // rows reading zero. It is what let `unbound_exits` be the ranking column by default, and
        // `unbound_exits` ranked `0x8C` (a segment reload run ~1.2M times) SEVEN TIMES ABOVE
        // `0x38 /0` (an inner-loop CMP), when the second was worth three times the whole rest of
        // the night put together. Costs nothing when the census is off: the call site in `run.rs`
        // is gated on `barrier_census_active()` before the arguments are even built.
        let shape = BarrierShape::from_insn(insn);
        if let Some(hits) = self.runtime_hits.get_mut(&shape) {
            *hits = hits.saturating_add(1);
        }
    }

    pub(crate) fn snapshot(&self) -> DirectBarrierCensusSnapshot {
        let mut keyed_rows: Vec<_> = self
            .rows
            .iter()
            .map(|(&key, &stats)| {
                let runtime_hits = self.runtime_hits.get(&key.shape).copied().unwrap_or(0);
                (key, census_row(key, stats, runtime_hits))
            })
            .collect();
        // Sorted by RUNTIME unbound exits first, tiebroken by compile attempts (`hits`).
        keyed_rows.sort_by(|(left_key, left), (right_key, right)| {
            right
                .unbound_exits
                .cmp(&left.unbound_exits)
                .then_with(|| right.hits.cmp(&left.hits))
                .then_with(|| left_key.cmp(right_key))
        });
        DirectBarrierCensusSnapshot {
            rows: keyed_rows.into_iter().map(|(_, row)| row).collect(),
            unbound_targets: UnboundTarget::ALL
                .iter()
                .map(|kind| (kind.label(), self.unbound[*kind as usize]))
                .collect(),
            dynamic_miss_targets: UnboundTarget::ALL
                .iter()
                .map(|kind| (kind.label(), self.unbound_dynamic[*kind as usize]))
                .collect(),
        }
    }
}

fn census_row(key: BarrierKey, stats: BarrierStats, runtime_hits: u64) -> DirectBarrierCensusRow {
    let shape = key.shape;
    DirectBarrierCensusRow {
        opcode: shape.opcode,
        modrm_reg: (shape.modrm_reg != u8::MAX).then_some(shape.modrm_reg),
        operand_form: match shape.operand_form {
            1 => "register",
            2 => "memory",
            _ => "none",
        },
        operand_size: if shape.operand_size != 0 {
            "dword"
        } else {
            "word"
        },
        address_size: if shape.address_size != 0 {
            "dword"
        } else {
            "word"
        },
        prefix_mask: shape.prefix_mask,
        stop_reason: key.stop.label(),
        unbound_exits: stats.unbound_exits,
        dynamic_unbound_exits: stats.dynamic_unbound_exits,
        hits: stats.hits,
        runtime_hits,
        native_prefix_instructions: stats.native_prefix_instructions,
        native_suffix_instructions: stats.native_suffix_instructions,
        max_native_prefix: stats.max_native_prefix,
        max_native_suffix: stats.max_native_suffix,
    }
}

pub(crate) fn barrier_census_default() -> Option<Box<DirectBarrierCensus>> {
    matches!(
        std::env::var("IZARRAVM_DIRECT_BARRIER_CENSUS").as_deref(),
        Ok("1")
    )
    .then(|| Box::new(DirectBarrierCensus::default()))
}

impl crate::jit::JitState {
    pub(super) fn barrier_census_enabled(&self) -> bool {
        self.direct_barrier_census.is_some()
    }

    pub(super) fn record_barrier(&mut self, insn: &DecodedInsn, observation: BarrierObservation) {
        if let Some(census) = self.direct_barrier_census.as_mut() {
            census.record(insn, observation);
        }
    }

    /// Whether the census exists at all. Callers MUST gate on this before calling
    /// `note_barrier_census_interpreted`, which sits on the per-interpreted-instruction retire
    /// path. Checking `is_some` inside the callee is too late for the gate to save anything.
    #[inline]
    pub(crate) fn barrier_census_active(&self) -> bool {
        self.direct_barrier_census.is_some()
    }

    pub(crate) fn note_barrier_census_interpreted(&mut self, insn: &DecodedInsn) {
        if let Some(census) = self.direct_barrier_census.as_mut() {
            census.note_interpreted(insn);
        }
    }

    /// Record why a static successor was unbound. No-op unless the census is allocated, and the
    /// CALLER still gates on `barrier_census_active` so the key construction is skipped too.
    pub(crate) fn note_unbound_target(&mut self, kind: UnboundTarget, linear: u32) {
        if let Some(census) = self.direct_barrier_census.as_mut() {
            census.note_unbound(kind);
            if kind == UnboundTarget::Rejected {
                census.note_unbound_rejected_at(linear);
            }
        }
    }

    /// Unlike the census snapshot this is ALWAYS available: none of its three groups is census
    /// gated, because each is a single increment on a path that has already left native code.
    pub(crate) fn stall_snapshot(&self) -> crate::DirectStallSnapshot {
        crate::DirectStallSnapshot {
            dormant: DormantReason::ALL
                .iter()
                .map(|r| (r.label(), self.stalls.dormant[*r as usize]))
                .collect(),
            link_refusals: LinkRefusal::ALL
                .iter()
                .map(|r| (r.label(), self.stalls.link_refusals[*r as usize]))
                .collect(),
            links_cleared: LinkClearCause::ALL
                .iter()
                .map(|c| (c.label(), self.stalls.links_cleared[*c as usize]))
                .collect(),
            side_exit_segment_limit: self.stalls.side_exit_segment_limit,
            side_exit_x87_eligibility: self.stalls.side_exit_x87_eligibility,
            side_exit_divide_guard: self.stalls.side_exit_divide_guard,
            side_exit_callout_step_break: self.stalls.side_exit_callout_step_break,
            side_exit_callout_abnormal: self.stalls.side_exit_callout_abnormal,
            callout_executed: self.stalls.callout_executed,
            reject_callout_privileged: self.stalls.reject_callout_privileged,
            segment_write_block_head_entries: self.stalls.segment_write_block_head_entries,
            segment_write_block_head_insns: self.stalls.segment_write_block_head_insns,
        }
    }

    /// The dynamic-lane counterpart of `note_unbound_target`, and it now takes the entry linear
    /// for the same reason that one does. It used to discard it, so every dynamic miss into a
    /// rejected block was attributed to nothing: on quake that is 2.86M exits, larger than the
    /// whole attributed static row set. Same call-site gate.
    pub(crate) fn note_dynamic_miss_target(&mut self, kind: UnboundTarget, linear: u32) {
        if let Some(census) = self.direct_barrier_census.as_mut() {
            census.note_unbound_dynamic(kind);
            if kind == UnboundTarget::Rejected {
                census.note_dynamic_rejected_at(linear);
            }
        }
    }

    /// Both are one unconditional increment on a path that has already taken a dispatcher exit,
    /// so unlike the census hooks these are NOT gated: the gate would cost as much as the work.
    pub(crate) fn note_side_exit_segment_limit(&mut self) {
        self.stalls.side_exit_segment_limit += 1;
    }

    pub(crate) fn note_side_exit_x87_eligibility(&mut self) {
        self.stalls.side_exit_x87_eligibility += 1;
    }

    pub(crate) fn note_side_exit_divide_guard(&mut self) {
        self.stalls.side_exit_divide_guard += 1;
    }

    pub(crate) fn note_side_exit_callout_step_break(&mut self) {
        self.stalls.side_exit_callout_step_break += 1;
    }

    pub(crate) fn note_side_exit_callout_abnormal(&mut self) {
        self.stalls.side_exit_callout_abnormal += 1;
    }

    /// One unconditional increment inside the call-out helper, which has already left native code
    /// and is about to touch the bus -- the same "the gate would cost as much as the work"
    /// reasoning as the two side-exit counters above.
    pub(crate) fn note_callout_executed(&mut self) {
        self.stalls.callout_executed += 1;
    }

    pub(crate) fn note_reject_callout_privileged(&mut self) {
        self.stalls.reject_callout_privileged += 1;
    }

    /// BRANCHLESS, and on purpose: this sits beside `jit_direct_entries` on the hottest path in
    /// the backend, next to the sixteen-bit split that is written the same way for the same
    /// reason. The caller passes the predicate already widened, so both lanes are an unconditional
    /// add and neither can mispredict.
    pub(crate) fn note_segment_write_block_entry(&mut self, is_segment_write: u64, insns: u64) {
        self.stalls.segment_write_block_head_entries += is_segment_write;
        self.stalls.segment_write_block_head_insns += is_segment_write * insns;
    }

    pub(crate) fn barrier_census_snapshot(&self) -> Option<DirectBarrierCensusSnapshot> {
        self.direct_barrier_census
            .as_deref()
            .map(DirectBarrierCensus::snapshot)
    }

    pub(crate) fn set_barrier_census_enabled(&mut self, enabled: bool) {
        self.direct_barrier_census = enabled.then(|| Box::new(DirectBarrierCensus::default()));
    }
}
