// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The Direct backend's structural-stop census and the diagnostic reporting surface on
//! `JitState`: the per-barrier rows, the unbound-exit and dynamic-miss class tallies, and the
//! stall snapshot. Split out of `direct.rs` to keep that file under the source-line ceiling.

use super::*;

#[cfg(feature = "direct-admission-census")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AdmissionDecline {
    HeatRefusal,
    KeyFailure,
    DormantProbe,
    RejectedProbe,
}

#[cfg(feature = "direct-admission-census")]
impl AdmissionDecline {
    pub(crate) const ALL: [Self; 4] = [
        Self::HeatRefusal,
        Self::KeyFailure,
        Self::DormantProbe,
        Self::RejectedProbe,
    ];
    pub(crate) const COUNT: usize = Self::ALL.len();

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::HeatRefusal => "heat_refusal",
            Self::KeyFailure => "key_failure",
            Self::DormantProbe => "dormant_probe",
            Self::RejectedProbe => "rejected_probe",
        }
    }
}

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
    /// x87 slots the block had already committed when the barrier stopped it. Non-zero arms BOTH
    /// x87 gates in the compile walk -- the loop-top cap at `MAX_X87_BLOCK_INSTRUCTIONS` and the
    /// x87/call-out mixing `Retry` -- and both are PREFIX-armed, so the scan's own refusal of x87
    /// KINDS does not make them unreachable. Leaving this out was the review finding: a barrier
    /// whose prefix held one `FLD` over-reported its suffix by up to 20 instructions against the
    /// 12-instruction x87 ceiling, on exactly the x87-adjacent population (Quake) the campaign is
    /// trying to rank.
    pub(super) x87_slots: u8,
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
/// deliberately left open, and two belong to the dirty-segment slice. A seventh arrived with the
/// 2026-08-19 L1 arm and is closed below:
///
/// * CLOSED, the L1 heat gate (`IZARRAVM_ROTATE_ROWS=heat`). That arm's refusal is NOT a
///   `classify` None -- `classify` admits the group-2 row and the compile walk downgrades it to
///   `HardBoundary` afterwards, where the physical address and the heat map are in scope. This
///   scan calls `classify` directly, so before the mirror below it walked straight through a
///   heat-gated ROL that the compile walk stops at. The resulting over-report is the worst
///   possible shape for this particular slice: it is correlated with `(1 - u)`, the PATCHED
///   share, so it would move the suffix columns in the heat-vs-off difference that the arm's
///   whole non-vacuity argument reads. The gate is mirrored below, immediately after `classify`,
///   and it is arm-gated so the `off` and `on` arms see no change at all.
///
/// * NOT A DIVERGENCE, the 2026-08-20 L2 arm-2 count lane (`IZARRAVM_COUNT_LANES`), recorded here
///   because it is the first place a reader will look for it. That slice attaches a lane to a kind
///   `classify` has ALREADY admitted; every one of `count_lane_for`'s bars narrows which admitted
///   slots take a lane and none can turn a Native classification into a boundary. So unlike the L1
///   heat gate above -- the one admission rule that was not a `classify` answer -- it needs no
///   mirror, and this scan stops exactly where the compile walk stops on both of its arms.
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
/// * PARTLY CLOSED, x87. The `kind.is_x87()` break below still refuses EVERY x87 slot in the
///   SUFFIX, a deliberate conservative floor for the kinds themselves. But both x87 gates in the
///   compile walk are armed by the PREFIX (`x87_slots != 0`), not by the suffix, so "the scan
///   never adds an x87 slot" never made them unreachable: a barrier whose prefix held an `FLD`
///   over-reported by up to 20 instructions against the 12-instruction x87 ceiling. The seed now
///   carries `x87_slots` and the scan mirrors the loop-top cap and the call-out mixing refusal.
///   What remains open is only the suffix-side admission of x87 kinds, and THAT half genuinely
///   can only under-report.
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
        x87_slots,
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
        // The x87 loop-top cap, prefix-armed exactly like the memory-ALU one above: the compile
        // walk breaks at `slots.len() == MAX_X87_BLOCK_INSTRUCTIONS` whenever the block already
        // holds an x87 slot, before decoding the next instruction. `x87_slots` never grows during
        // this scan (the `kind.is_x87()` refusal below sees to that), so testing the seed value is
        // exact rather than conservative.
        if x87_slots != 0
            && prefix_instructions + 1 + result.instructions >= MAX_X87_BLOCK_INSTRUCTIONS
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
        // The L1 heat gate, mirrored from the compile walk. `classify` alone is NOT the compile
        // walk's admission on the `heat_gated` arm: a group-2 row whose count byte carries a heat
        // record classifies Native and is then downgraded to `HardBoundary` one step later. A scan
        // that took the bare `classify` answer would walk straight through the very instruction
        // the compile walk stops at, and the over-report would be correlated with the unpatched
        // share `u` -- i.e. it would corrupt exactly the heat-vs-off census difference the arm
        // exists to measure. See the ledger entry above and `rotate_row_count_byte_is_patched`.
        if rotate_rows_arm() == RotateRowsArm::HeatGated
            && rotate_row_count_byte_is_patched(cpu, &insn, expected_phys)
        {
            break;
        }
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
            // The x87/call-out mixing refusal's live half: a call-out into a block whose PREFIX
            // holds an x87 slot is a `Retry` in the compile walk. The other half (an x87 slot
            // after a call-out) stays dead here because the scan refuses x87 kinds outright.
            || (kind.is_call_out() && x87_slots != 0)
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
    /// Host wall nanoseconds inside `compact_arena`'s successful body. The §2 regression in
    /// `dev_docs/duke3d-open-area-profile-results.md` INFERRED 7.44 ms per event from interval
    /// wall; this measures it.
    pub arena_compaction_ns: u64,
    pub links: u64,
    pub unlinks: u64,
    pub decode_dependencies_scanned: u64,
    pub portals_hidden: u64,
}

/// One allowlist row's four outcome counts. A struct rather than four parallel arrays so a row's
/// numbers stay adjacent in the tally, the report and the probe JSON alike.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct InterpretOneRowTally {
    pub(crate) executed: u64,
    pub(crate) resync: u64,
    pub(crate) resync_fault: u64,
    pub(crate) demoted: u64,
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
    /// Port call-outs SERVED through the TSS-bitmap arm -- the V86 / CPL>IOPL state whose
    /// permission check the helper's two-phase probe now satisfies natively.
    ///
    /// Separate from `callout_executed` because that count sums both arms, so on a guest that
    /// runs compiled INs at both privilege states it cannot say whether the bitmap arm served
    /// anything at all. This is the numerator of the slice's non-vacuity ratio; `executed` stays
    /// the denominator.
    pub callout_port_v86_served: u64,
    /// The `InterpretOne` family, five counters that price the mechanism on its own terms.
    ///
    /// `executed` is every slot the helper ENTERED, the denominator the rest need.
    /// `resync` and `resync_fault` are the two RESYNC statuses -- the predicate refusing after a
    /// retired instruction, and the step faulting. `abnormal` is the helper's one fail-closed
    /// return: NO RESIDENT DECODE VIEW, and nothing else.
    ///
    /// **A demoted slot is not in `abnormal`, and cannot be.** The demotion is a byte the emitted
    /// prologue tests before the call, so a demoted execution never reaches the helper and never
    /// reaches any counter here. It is observable as the DIFFERENCE between two counters that do
    /// fire: `side_exit_callout_abnormal` counts every execution that took the abnormal exit,
    /// `callout_interpret_one_abnormal` counts the subset that got there through the helper, and
    /// the gap is executions refused by the governor. `demoted` then counts the CELLS, once each
    /// at the transition, so `gap / demoted` is what a demotion costs the block that carries it.
    ///
    /// The design asked for a `BarrierStop::CallOutDemoted` row instead. It is not implementable
    /// as specified and the reason is worth recording rather than leaving as an omission: the
    /// barrier census records COMPILE-WALK stops, and demotion is a runtime event on an already
    /// compiled block that never changes its classification (design review M11 replaced
    /// demotion-by-recompile precisely so it would not have to). There is no compile walk to
    /// attribute a row to.
    ///
    /// Separate from `callout_executed` and `side_exit_callout_abnormal` rather than folded into
    /// them, for the reason `callout_port_v86_served` is separate: those sum every helper class,
    /// so on a guest running both they cannot say whether THIS class served anything. The
    /// acceptance ratio the slice is graded on is `resync / executed`.
    pub callout_interpret_one_executed: u64,
    pub callout_interpret_one_resync: u64,
    pub callout_interpret_one_resync_fault: u64,
    pub callout_interpret_one_abnormal: u64,
    pub callout_interpret_one_demoted: u64,
    /// The same family, split by the allowlist ROW the slot was admitted as, indexed by
    /// `InterpretOneRow::index`.
    ///
    /// The scalars above cannot answer the question the plan grades this slice on. Its rule is "a
    /// row the governor demotes on the loader at more than 50% is refuted and removed from the
    /// list", and a whole-CPU `resync / executed` says the family resynced without saying which
    /// row did it. With nine rows admitted, one bad row hiding behind eight good ones is exactly
    /// the shape that ratio cannot see.
    ///
    /// ABNORMAL is deliberately not among the four. A demoted slot takes the abnormal exit from
    /// its emitted prologue WITHOUT calling the helper, so there is no cell in scope to attribute
    /// it to and a per-row abnormal count would be silently short by every post-demotion exit.
    /// The scalar stays the honest home for it.
    pub callout_interpret_one_rows: [InterpretOneRowTally; InterpretOneRow::COUNT],
    /// Code writes taken while a call-out window was open, i.e. writes that reported a hit and
    /// were replayed at the drain instead of invalidating under a live native frame.
    ///
    /// The always-on evidence for design review B2, and the only evidence there can be: the
    /// deferral is invisible from outside because the drain makes the outcome identical. A slot
    /// whose step stores onto watched code contributes one; a slot whose step FAULTS onto watched
    /// code contributes one per pushed word of the delivery frame, which is the case the window
    /// has to stay open across.
    pub callout_deferred_code_writes: u64,
    /// Compile walks that stopped because the block already held `MAX_BLOCK_CALLOUT_SLOTS`. The
    /// evidence for or against raising the cap, which S5 prices; zero says the cap is not what
    /// bounds the loader's blocks.
    pub callout_slot_cap_hits: u64,
    /// G1 lane trials granted: hot-chunk compilations allowed through the heat gates on the
    /// one-per-key-per-epoch budget (`lane_trial_enabled`), and how many of them installed a
    /// lane-carrying block under a hot span. The gap between the two is trials that learned
    /// their region is not lane-shaped. Here rather than `PerfCounters` for the layout reason on
    /// this struct's doc.
    pub lane_trials: u64,
    pub lane_trial_installs: u64,
    /// Displacement lanes registered at install (the `0x8A` family), the disp share of the
    /// aggregate `PerfCounters::smc_lane_registrations`. The split is what the A/B needs:
    /// `smc_lane_accepts` moving with this counter flat would say the imm lanes did the work.
    pub disp_lane_registrations: u64,
    /// One-byte immediate lanes registered at install (the `0x80 /r` family behind
    /// `IZARRAVM_IMM8_LANES`), the imm8 share of the aggregate
    /// `PerfCounters::smc_lane_registrations`. Same job as `disp_lane_registrations` one class
    /// over: `smc_lane_accepts` moving with this counter flat would say the L2 arm-1 lanes were
    /// not the cause.
    pub imm8_lane_registrations: u64,
    /// Group-2 COUNT lanes registered at install (the `0xC1`/`0xC0` count byte behind
    /// `IZARRAVM_COUNT_LANES`), the L2 arm-2 share of the aggregate
    /// `PerfCounters::smc_lane_registrations`. Kept apart from `imm8_lane_registrations` even
    /// though both register at `IMM8_LANE_WIDTH`: the two arms are independent knobs, so a
    /// combined leg has to be able to attribute an accepts movement to one of them.
    pub count_lane_registrations: u64,
    /// Interpreted continuations whose decode line had died between the packed first touch and
    /// the deferred full-view fetch (`IZARRAVM_DECODE_PACK`). The staleness argument in
    /// `run_budgeted_inner` says admission cannot invalidate the slot it screened, so this is the
    /// counter that makes that a measurement instead of a claim: any nonzero value on a real run
    /// falsifies it, and the packed arm loses continuations the unpacked arm would have run.
    pub decode_pack_late_view_miss: u64,
    /// Entries refused because a call-out-bearing block met the privilege state whose port reads
    /// consult the TSS bitmap. Zero on a guest that never runs a compiled IN at CPL>IOPL or in
    /// V86, which is the isolation claim for the whole call-out slice on the shipped fixtures.
    pub reject_callout_privileged: u64,
    /// The call-out admission governor, three counters that price it without a new instrument.
    ///
    /// `trials` is entries admitted at trial quota 1 -- the governor's whole cost, one
    /// spill/call/serve/reload/side-exit per call-out block per epoch. `lazy` and `io_touching`
    /// are the classifications those trials reached. `trials - lazy - io_touching` is the rest:
    /// abnormal serves (`Denied`), trials that served nothing, and the truncated ones that
    /// learned nothing; the abnormal share is already visible in
    /// `side_exit_callout_abnormal`.
    pub callout_governor_trials: u64,
    pub callout_governor_lazy: u64,
    pub callout_governor_io_touching: u64,
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
    /// The x87 TOP-mismatch retire cap (`retire_key_for_top_mismatch`). `suppressed` counts every
    /// retire the cap refused -- one per mismatch on an already-capped key, so on a churning guest
    /// it approaches `jit_direct_reject_x87_top`. The identity
    /// `reject_x87_top == x87_top_retires_suppressed + retires taken` closes across the run.
    pub x87_top_retires_suppressed: u64,
    /// Cap CROSSINGS, not live keys: a key whose page is rewritten loses its count and can cross
    /// again. Exact on a guest with no SMC and one cache generation; an over-report on an
    /// SMC-heavy one. Named for what it counts.
    pub x87_top_sticky_crossings: u64,
    /// Sticky-decline memo instruments, always on. Here for this struct's stated reason:
    /// `PerfCounters` sits ahead of `pending_flags` in `CpuGsw` at an offset emitted code bakes,
    /// and `BlockCacheStats` is drained per dispatcher exit — these have to accumulate for the
    /// whole run and be read by `stall_snapshot`. See `DirectStallSnapshot` for why they are not
    /// census-gated.
    pub decline_memo_hits: u64,
    pub decline_memo_advances: u64,
    pub decline_memo_sweeps: u64,
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
    /// The CHAIN segment requirements of the two ends do not merge: they disagree about `cs`, or
    /// about a `data` descriptor that some block in one of the two chains actually pins
    /// (`SegmentLayout::link_merge`). A descriptor no block in either chain pins is not a reason
    /// to refuse -- that admission is the whole of the 2026-08-18 chain-used mask slice.
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

#[cfg(feature = "direct-link-refusal-census")]
const LINK_REFUSAL_BUCKET_OFFSET: usize = 2;
#[cfg(feature = "direct-link-refusal-census")]
const LINK_CLEAR_BUCKET_OFFSET: usize = LINK_REFUSAL_BUCKET_OFFSET + LinkRefusal::COUNT;
#[cfg(feature = "direct-link-refusal-census")]
const UNEXPECTED_LINKED_BUCKET: usize = LINK_CLEAR_BUCKET_OFFSET + LinkClearCause::COUNT;
#[cfg(feature = "direct-link-refusal-census")]
const CLOSED_BUCKET: usize = UNEXPECTED_LINKED_BUCKET + 1;
#[cfg(feature = "direct-link-refusal-census")]
const DIRECT_LINK_REFUSAL_BUCKET_COUNT: usize = CLOSED_BUCKET + 1;

#[cfg(feature = "direct-link-refusal-census")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DirectLinkRefusalState {
    Suppressed,
    Waiting,
    Refused { reason: LinkRefusal },
    Linked,
    Cleared { cause: LinkClearCause },
    Closed,
}

#[cfg(feature = "direct-link-refusal-census")]
impl DirectLinkRefusalState {
    fn bucket(self) -> usize {
        match self {
            Self::Suppressed => 0,
            Self::Waiting => 1,
            Self::Refused { reason, .. } => LINK_REFUSAL_BUCKET_OFFSET + reason as usize,
            Self::Linked => UNEXPECTED_LINKED_BUCKET,
            Self::Cleared { cause, .. } => LINK_CLEAR_BUCKET_OFFSET + cause as usize,
            Self::Closed => CLOSED_BUCKET,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Suppressed => "suppressed",
            Self::Waiting => "not_attempted",
            Self::Refused { reason, .. } => link_refusal_bucket_label(reason),
            Self::Linked => "unexpected_linked",
            Self::Cleared { cause, .. } => link_clear_bucket_label(cause),
            Self::Closed => "closed",
        }
    }
}

#[cfg(feature = "direct-link-refusal-census")]
fn link_refusal_bucket_label(reason: LinkRefusal) -> &'static str {
    match reason {
        LinkRefusal::Inactive => "refused_inactive",
        LinkRefusal::StaleEpoch => "refused_stale_epoch",
        LinkRefusal::SegmentLayout => "refused_segment_layout",
        LinkRefusal::BlockShape => "refused_block_shape",
        LinkRefusal::DynamicIntegerToFloat => "refused_dynamic_integer_to_float",
        LinkRefusal::DynamicFloatToInteger => "refused_dynamic_float_to_integer",
        LinkRefusal::MissingX87Pad => "refused_missing_x87_pad",
    }
}

#[cfg(feature = "direct-link-refusal-census")]
fn link_clear_bucket_label(cause: LinkClearCause) -> &'static str {
    match cause {
        LinkClearCause::Replaced => "cleared_replaced",
        LinkClearCause::Retired => "cleared_retired",
        LinkClearCause::Flushed => "cleared_flushed",
        LinkClearCause::Reset => "cleared_reset",
        LinkClearCause::ChainWiden => "cleared_chain_widen",
    }
}

#[cfg(feature = "direct-link-refusal-census")]
fn direct_link_refusal_bucket_labels() -> [&'static str; DIRECT_LINK_REFUSAL_BUCKET_COUNT] {
    let mut labels = [""; DIRECT_LINK_REFUSAL_BUCKET_COUNT];
    labels[0] = "suppressed";
    labels[1] = "not_attempted";
    let mut index = 0;
    while index < LinkRefusal::COUNT {
        labels[LINK_REFUSAL_BUCKET_OFFSET + index] =
            link_refusal_bucket_label(LinkRefusal::ALL[index]);
        index += 1;
    }
    index = 0;
    while index < LinkClearCause::COUNT {
        labels[LINK_CLEAR_BUCKET_OFFSET + index] =
            link_clear_bucket_label(LinkClearCause::ALL[index]);
        index += 1;
    }
    labels[UNEXPECTED_LINKED_BUCKET] = "unexpected_linked";
    labels[CLOSED_BUCKET] = "closed";
    labels
}

#[cfg(feature = "direct-link-refusal-census")]
struct DirectLinkRefusalCell {
    source: BlockKey,
    source_generation: u64,
    slot: u8,
    target: LinkTarget,
    state: DirectLinkRefusalState,
    last_target_generation: Option<u64>,
    unbound_exits: u64,
    buckets: [u64; DIRECT_LINK_REFUSAL_BUCKET_COUNT],
}

#[cfg(feature = "direct-link-refusal-census")]
#[derive(Default)]
pub(crate) struct DirectLinkRefusalCensus {
    seen: u64,
    missing_id: u64,
    invalid_id: u64,
    rows: Vec<DirectLinkRefusalCell>,
}

#[cfg(feature = "direct-link-refusal-census")]
impl DirectLinkRefusalCensus {
    pub(super) fn register(
        &mut self,
        source: BlockKey,
        source_generation: u64,
        slot: u8,
        target: LinkTarget,
        suppressed: bool,
    ) -> u32 {
        let id = self
            .rows
            .len()
            .checked_add(1)
            .and_then(|id| u32::try_from(id).ok())
            .expect("direct link-refusal census row ID exhausted");
        self.rows.push(DirectLinkRefusalCell {
            source,
            source_generation,
            slot,
            target,
            state: if suppressed {
                DirectLinkRefusalState::Suppressed
            } else {
                DirectLinkRefusalState::Waiting
            },
            last_target_generation: None,
            unbound_exits: 0,
            buckets: [0; DIRECT_LINK_REFUSAL_BUCKET_COUNT],
        });
        id
    }

    fn row_mut(&mut self, id: u32) -> Option<&mut DirectLinkRefusalCell> {
        let index = id.checked_sub(1)? as usize;
        self.rows.get_mut(index)
    }

    pub(super) fn refused(&mut self, id: u32, reason: LinkRefusal, target_generation: u64) {
        if let Some(row) = self.row_mut(id) {
            row.state = DirectLinkRefusalState::Refused { reason };
            row.last_target_generation = Some(target_generation);
        }
    }

    pub(super) fn linked(&mut self, id: u32, target_generation: u64) {
        if let Some(row) = self.row_mut(id) {
            row.state = DirectLinkRefusalState::Linked;
            row.last_target_generation = Some(target_generation);
        }
    }

    pub(super) fn cleared(&mut self, id: u32, cause: LinkClearCause, target_generation: u64) {
        if let Some(row) = self.row_mut(id) {
            row.state = DirectLinkRefusalState::Cleared { cause };
            row.last_target_generation = Some(target_generation);
        }
    }

    pub(super) fn close(&mut self, id: u32) {
        if let Some(row) = self.row_mut(id) {
            row.state = DirectLinkRefusalState::Closed;
        }
    }

    pub(super) fn note_exit(&mut self, id: u32) {
        self.seen += 1;
        if id == 0 {
            self.missing_id += 1;
            return;
        }
        let Some(row) = self.row_mut(id) else {
            self.invalid_id += 1;
            return;
        };
        let bucket = row.state.bucket();
        row.unbound_exits += 1;
        row.buckets[bucket] += 1;
    }

    pub(super) fn snapshot(&self) -> crate::DirectLinkRefusalCensusSnapshot {
        let labels = direct_link_refusal_bucket_labels();
        crate::DirectLinkRefusalCensusSnapshot {
            seen: self.seen,
            missing_id: self.missing_id,
            invalid_id: self.invalid_id,
            rows: self
                .rows
                .iter()
                .enumerate()
                .map(|(index, row)| crate::DirectLinkRefusalCensusRow {
                    id: u32::try_from(index + 1).expect("registered census row ID must fit u32"),
                    source_linear: row.source.linear,
                    source_physical: row.source.physical,
                    source_mode_key: row.source.mode_key,
                    source_generation: row.source_generation,
                    slot: row.slot,
                    target_linear: row.target.linear,
                    target_mode_key: row.target.mode_key,
                    last_target_generation: row.last_target_generation,
                    state: row.state.label(),
                    unbound_exits: row.unbound_exits,
                    buckets: labels
                        .iter()
                        .copied()
                        .zip(row.buckets.iter().copied())
                        .collect(),
                })
                .collect(),
        }
    }
}

#[cfg(feature = "direct-link-refusal-census")]
pub(crate) fn direct_link_refusal_census_default() -> Option<Box<DirectLinkRefusalCensus>> {
    matches!(
        std::env::var("IZARRAVM_DIRECT_LINK_REFUSAL_CENSUS").as_deref(),
        Ok("1")
    )
    .then(|| Box::new(DirectLinkRefusalCensus::default()))
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
    /// A downstream link widened the target's CHAIN segment requirement past what this source can
    /// satisfy, so the edge was cut to keep the chain sound. Both blocks stay compiled and the
    /// cell reverts to the zero portal, so the source's exits report `StaticUnbound`; the edge is
    /// NOT re-parked in `waiting`, because the widen is monotone and a retry could only re-derive
    /// the same conflict. See dev_docs/plans/2026-08-18-chain-used-link-mask.md.
    ChainWiden,
}

impl LinkClearCause {
    pub(crate) const COUNT: usize = 5;
    pub(crate) const ALL: [Self; Self::COUNT] = [
        Self::Replaced,
        Self::Retired,
        Self::Flushed,
        Self::Reset,
        Self::ChainWiden,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Replaced => "replaced",
            Self::Retired => "retired",
            Self::Flushed => "flushed",
            Self::Reset => "reset",
            Self::ChainWiden => "chain_widen",
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
    /// are structurally zero for it. `runtime_hits` is UNREGISTERED by it, deliberately (the
    /// column can still read non-zero for a shape that ALSO barriers under a genuine coverage
    /// stop, since registration is shared by shape): the column
    /// counts interpreted executions by SHAPE, and this variant's rows key on the segment's user
    /// (`0x8a`, `0x8b`, memory forms), shapes that are fully lowered and executed constantly. An
    /// address-based reading ("the barred instruction becomes the next block's entry, so it never
    /// interprets") predicted near-zero and was wrong for a shape-keyed counter; `record` skips
    /// the registration instead, so the column cannot mislead. `snapshot` sorts on those columns,
    /// so these rows land last while being the ones a segment slice is looking for. Read `hits`
    /// and the suffix instead.
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
    /// `Rejected`-class STATIC exits whose entry linear was absent from `rejected_barrier`, i.e.
    /// the exits `note_unbound_rejected_at` used to drop with no trace. This is the barrier
    /// census's analogue of the link census's `missing_id`, and without it the row-attribution
    /// identity
    ///
    /// ```text
    /// sum(row.unbound_exits) + rejected_unattributed == unbound[Rejected]
    /// ```
    ///
    /// cannot be evaluated on a fixture run at all. A nonzero value is not automatically a defect:
    /// a block rejected before the census was armed, a rejection installed by an arm that does not
    /// record, or an entry invalidated and recompiled at another linear all land here honestly.
    ///
    /// CAVEAT, and it is the reason `rejected_barrier_overwrites` exists beside it: this residual
    /// CANNOT see stale-hit mis-attribution. `rejected_barrier` is never pruned and is keyed on
    /// linear alone, so an exit that resolves to a stale or colliding row is charged to that row
    /// and leaves the residual at zero. The residual detects DROPPED exits, never MISDIRECTED ones.
    #[cfg(feature = "barrier-census-closure")]
    rejected_unattributed: u64,
    /// The dynamic-lane twin of `rejected_unattributed`, carried separately because Slice 4's
    /// lesson is that the two lanes move by wildly different factors for the same row.
    #[cfg(feature = "barrier-census-closure")]
    dynamic_rejected_unattributed: u64,
    /// How many times a rejection claimed a linear `rejected_barrier` already held. This is the
    /// honest signal for the stale-hit hazard the two residuals above are blind to: an overwritten
    /// key still resolves, so its exits are attributed — to the row recorded LAST. Duke runs
    /// DOS4GW protected mode over JEMMEX with paging and overlays, which is exactly the workload
    /// where linear-only keys collide, so a large value here devalues the row ranking even while
    /// both residuals read zero.
    #[cfg(feature = "barrier-census-closure")]
    rejected_barrier_overwrites: u64,
    /// B.3: the `DormantHeat` linear histogram — block entry linear -> `(static, dynamic)` exits.
    ///
    /// `note_unbound_target` has always RECEIVED the entry linear and discarded it for every class
    /// but `Rejected`, so the census could count 141.7M `dormant_heat` exits on duke and not say
    /// whether they were fifty addresses or fifty thousand. The whole Track B fork — targeted lane
    /// class versus policy-parameter sweep — turns on that one question, and no knob should be
    /// swept against a class whose addresses are unknown.
    ///
    /// Keyed on linear alone and never pruned, exactly like `rejected_barrier`, and it inherits
    /// that map's caveat unchanged: two dormant spans sharing a linear across mode/physical merge
    /// into one row. Acceptable for a diagnostic whose question is "concentrated or diffuse".
    ///
    /// Both lanes in ONE map with two columns rather than two maps, mirroring `BarrierStats`:
    /// Slice 4's lesson is that the two lanes move by wildly different factors for the same row,
    /// so they must be separable — but they are separable as columns, and a site that exits both
    /// ways is one site.
    #[cfg(feature = "barrier-census-closure")]
    dormant_heat_sites: HashMap<u32, DormantHeatStats>,
    /// The `Rejected` twin of `dormant_heat_sites`, and it locates the largest pool on the board
    /// that still has no addresses: `rejected` is 33.64% of duke-586's static-unbound exits and
    /// 40.1% of duke-486's, against `dormant_heat`'s 46.04%.
    ///
    /// `rejected_barrier` already attributes those exits to the BARRIER that refused the block —
    /// an opcode shape — and `rejected_unattributed` counts the ones it could not resolve. Neither
    /// says how many distinct addresses the class covers, which is the question that decided the
    /// dormant-heat fork (7 addresses carried 80% there) and the one that has to be asked before
    /// any knob is swept at this class. Unlike `rejected_barrier` this histogram cannot miss: it
    /// IS the map, so its closure against `unbound_targets[rejected]` is exact and carries no
    /// residual.
    ///
    /// Same key, same two columns, same caveat as its sibling — two rejected spans sharing a
    /// linear across mode or physical merge into one row.
    #[cfg(feature = "barrier-census-closure")]
    rejected_sites: HashMap<u32, DormantHeatStats>,
    /// B.3's lane-match export: block entry linear -> `LaneProbe` bits, recorded by the compile
    /// walk itself.
    ///
    /// This is the field that TESTS the §B.4 hypothesis "the 5.1M uncovered one-byte-imm patch
    /// events are the population behind the 24,722 failing lane trials". A `WALKED` bit with no
    /// `IMM`/`DISP` bit means a compile walk ran over these bytes and no lane matcher fired on any
    /// slot of it — the region is not lane-shaped, whatever else is true of it. Joined against
    /// `dormant_heat_sites` on the same key, that answers the hypothesis directly instead of
    /// inferring it from two aggregates that were never shown to describe the same population.
    ///
    /// WALK, NOT TRIAL. The heat gate's `lane trial` is the one-per-key-per-epoch exception that
    /// installs only when the compilation registered a lane; a `compile walk` is any pass of
    /// `compile_with_instruction_limit`, trial or ordinary, and this map records walks. Trials are
    /// a subset and nothing here can say which pass was one. That is fine for §B.4 — the question
    /// is whether the BYTES are lane-shaped, and an ordinary walk answers it as well as a trial —
    /// but the two words must not be swapped, because "a trial was spent here" is a claim about
    /// the epoch budget that this record cannot support.
    ///
    /// Keyed on the block ENTRY linear, not on the laned instruction's own address, and that is
    /// deliberate: the heat gate refuses per KEY, the trial budget is spent per KEY, and
    /// `dormant_heat_sites` is keyed per KEY. Keying on the instruction would make the join a
    /// coincidence of which 16-byte chunk the laned slot happened to fall in.
    ///
    /// A SET, not a tally: `compile_with_page_len` binary-searches the prefix length and re-walks
    /// the same entry several times, so a count here would report host search effort as guest
    /// behaviour. Bit-or is idempotent under that re-walk.
    #[cfg(feature = "barrier-census-closure")]
    lane_probes: HashMap<u32, u8>,
    #[cfg(feature = "direct-admission-census")]
    /// Partial attribution of dispatcher declines. Other routes remain outside this array.
    admission_declines: [u64; AdmissionDecline::COUNT],
}

/// The two exit columns of one `dormant_heat` site. Mirrors `BarrierStats`'s static/dynamic split.
#[cfg(feature = "barrier-census-closure")]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct DormantHeatStats {
    static_exits: u64,
    dynamic_exits: u64,
}

/// Which lane matcher, if any, ever fired on a compile walk from a given block entry.
///
/// `WALKED` is carried apart from the two lane bits so the export can tell "a walk ran over these
/// bytes and no matcher fired" from "no walk was ever seen here". Those are different answers to
/// §B.4: the first says the region is genuinely not lane-shaped, the second says nothing at all
/// about its shape, and only the first supports widening the lane class.
///
/// The second is also a WEAK negative — the census may simply have been armed after the walk. See
/// `DirectDormantHeatSite::compile_walked` for that caveat, which bounds how far the absence can
/// be read.
#[cfg(feature = "barrier-census-closure")]
pub(crate) mod lane_probe {
    /// A compile walk started from this entry linear.
    pub(crate) const WALKED: u8 = 1;
    /// `imm_lane_for` attached a lane on some slot of that walk.
    pub(crate) const IMM: u8 = 2;
    /// `disp_lane_for` attached a lane on some slot of that walk.
    pub(crate) const DISP: u8 = 4;
}

/// How many `dormant_heat` sites the snapshot publishes, descending by total exits.
///
/// The tail is not dropped: it is summed into `dormant_heat_truncated_*` so the closure identity
/// below holds against the class total whatever this constant is. 64 matches the SMC trace's
/// `REPORT_SITES`, and B.3's decision rule only needs the top 50.
#[cfg(feature = "barrier-census-closure")]
pub(crate) const DORMANT_HEAT_SITES: usize = 64;

impl DirectBarrierCensus {
    #[cfg(feature = "direct-admission-census")]
    fn note_admission_decline(&mut self, kind: AdmissionDecline) {
        self.admission_declines[kind as usize] =
            self.admission_declines[kind as usize].saturating_add(1);
    }

    fn note_unbound(&mut self, kind: UnboundTarget) {
        self.unbound[kind as usize] += 1;
    }

    fn note_unbound_dynamic(&mut self, kind: UnboundTarget) {
        self.unbound_dynamic[kind as usize] += 1;
    }

    /// Attribute one rejected-target exit back to the barrier that refused that block.
    fn note_unbound_rejected_at(&mut self, linear: u32) {
        let Some(&key) = self.rejected_barrier.get(&linear) else {
            #[cfg(feature = "barrier-census-closure")]
            {
                self.rejected_unattributed = self.rejected_unattributed.saturating_add(1);
            }
            return;
        };
        let row = self.rows.entry(key).or_default();
        row.unbound_exits = row.unbound_exits.saturating_add(1);
    }

    /// Record one `DormantHeat` exit against the block entry linear it failed to reach.
    ///
    /// UNCONDITIONAL on the class already having been counted by `note_unbound`, which is what
    /// makes the closure exact rather than approximate: there is no early return, no lookup that
    /// can miss, and therefore no residual to carry. `rejected_barrier` needs a residual because
    /// it resolves a linear through a map that may not hold it; this histogram IS the map.
    #[cfg(feature = "barrier-census-closure")]
    fn note_dormant_heat_at(&mut self, linear: u32, dynamic: bool) {
        Self::note_site(&mut self.dormant_heat_sites, linear, dynamic);
    }

    /// The `Rejected` twin, with the same unconditional-and-therefore-exact property. See
    /// `rejected_sites`.
    #[cfg(feature = "barrier-census-closure")]
    fn note_rejected_site_at(&mut self, linear: u32, dynamic: bool) {
        Self::note_site(&mut self.rejected_sites, linear, dynamic);
    }

    #[cfg(feature = "barrier-census-closure")]
    fn note_site(sites: &mut HashMap<u32, DormantHeatStats>, linear: u32, dynamic: bool) {
        let site = sites.entry(linear).or_default();
        if dynamic {
            site.dynamic_exits = site.dynamic_exits.saturating_add(1);
        } else {
            site.static_exits = site.static_exits.saturating_add(1);
        }
    }

    #[cfg(feature = "barrier-census-closure")]
    fn note_lane_probe(&mut self, entry_linear: u32, bits: u8) {
        *self.lane_probes.entry(entry_linear).or_default() |= bits;
    }

    /// The dynamic-lane counterpart. Same map, separate column: the two lanes have different
    /// fixes and Slice 4 showed they move by wildly different factors for the same row.
    fn note_dynamic_rejected_at(&mut self, linear: u32) {
        let Some(&key) = self.rejected_barrier.get(&linear) else {
            #[cfg(feature = "barrier-census-closure")]
            {
                self.dynamic_rejected_unattributed =
                    self.dynamic_rejected_unattributed.saturating_add(1);
            }
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
            let displaced = self.rejected_barrier.insert(entry_linear, key);
            #[cfg(feature = "barrier-census-closure")]
            if displaced.is_some() {
                self.rejected_barrier_overwrites =
                    self.rejected_barrier_overwrites.saturating_add(1);
            }
            #[cfg(not(feature = "barrier-census-closure"))]
            let _ = displaced;
        }
        // Register the shape so `note_interpreted` can find it with one probe and without ever
        // creating a row for an instruction that never barriered.
        //
        // EXCEPT for the dirty-segment rule. Its rows key on the segment's USER -- `0x8a`, `0x8b`
        // and the other fully-lowered forms that happen to read the segment -- and
        // `note_interpreted` counts by SHAPE, not by address. Registering those shapes would land
        // every interpreted `MOV r,[mem]` in the guest in a barrier row, and `runtime_hits` is
        // the column the campaign ranks by. The census's own history already records this failure
        // mode once (`unbound_exits` ranking `0x8C` seven places above `0x38 /0`). The row keeps
        // its compile-time attribution, which is what it exists to provide.
        if stop != BarrierStop::DirtySegment {
            self.runtime_hits.entry(key.shape).or_default();
        }
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

    /// Top-`DORMANT_HEAT_SITES` sites plus the summed tail, joined against the lane-probe set.
    ///
    /// Returns `(rows, truncated_static, truncated_dynamic, distinct_sites)`. The two truncated
    /// sums are what keeps the C3-style closure identity
    ///
    /// ```text
    /// sum(rows.static_exits) + truncated_static == unbound[DormantHeat]
    /// ```
    ///
    /// true independently of the head size, which is the whole reason the tail is summed rather
    /// than dropped: a truncated head would make the histogram silently under-report the class it
    /// exists to explain, and that is exactly the instrument defect the Prince census's first run
    /// turned out to be.
    #[cfg(feature = "barrier-census-closure")]
    fn dormant_heat_snapshot(&self) -> (Vec<crate::DirectDormantHeatSite>, u64, u64, u64) {
        self.site_snapshot(&self.dormant_heat_sites)
    }

    /// The `Rejected` twin, closing against `unbound_targets[rejected]` and
    /// `dynamic_miss_targets[rejected]` by the same identity.
    #[cfg(feature = "barrier-census-closure")]
    fn rejected_site_snapshot(&self) -> (Vec<crate::DirectDormantHeatSite>, u64, u64, u64) {
        self.site_snapshot(&self.rejected_sites)
    }

    #[cfg(feature = "barrier-census-closure")]
    fn site_snapshot(
        &self,
        map: &HashMap<u32, DormantHeatStats>,
    ) -> (Vec<crate::DirectDormantHeatSite>, u64, u64, u64) {
        let mut sites: Vec<_> = map
            .iter()
            .map(|(&linear, &stats)| (linear, stats))
            .collect();
        // Descending by TOTAL exits, tiebroken by linear so the head is deterministic across runs
        // with identical counts -- a census whose head reorders between two runs cannot be diffed.
        sites.sort_by(|(left_linear, left), (right_linear, right)| {
            (right.static_exits + right.dynamic_exits)
                .cmp(&(left.static_exits + left.dynamic_exits))
                .then_with(|| left_linear.cmp(right_linear))
        });
        let distinct = sites.len() as u64;
        let tail = sites.split_off(sites.len().min(DORMANT_HEAT_SITES));
        let truncated_static = tail.iter().map(|(_, stats)| stats.static_exits).sum();
        let truncated_dynamic = tail.iter().map(|(_, stats)| stats.dynamic_exits).sum();
        let rows = sites
            .into_iter()
            .map(|(linear, stats)| {
                let probe = self.lane_probes.get(&linear).copied().unwrap_or(0);
                crate::DirectDormantHeatSite {
                    linear,
                    static_exits: stats.static_exits,
                    dynamic_exits: stats.dynamic_exits,
                    compile_walked: probe & lane_probe::WALKED != 0,
                    imm_lane_matched: probe & lane_probe::IMM != 0,
                    disp_lane_matched: probe & lane_probe::DISP != 0,
                }
            })
            .collect();
        (rows, truncated_static, truncated_dynamic, distinct)
    }

    pub(crate) fn snapshot(&self) -> DirectBarrierCensusSnapshot {
        #[cfg(feature = "barrier-census-closure")]
        let (
            dormant_heat_sites,
            dormant_heat_truncated_static,
            dormant_heat_truncated_dynamic,
            dormant_heat_distinct_sites,
        ) = self.dormant_heat_snapshot();
        #[cfg(feature = "barrier-census-closure")]
        let (
            rejected_sites,
            rejected_truncated_static,
            rejected_truncated_dynamic,
            rejected_distinct_sites,
        ) = self.rejected_site_snapshot();
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
            // Derived here rather than counted at runtime: the arrays already hold everything, so
            // the totals cost the hot path nothing at all.
            #[cfg(feature = "barrier-census-closure")]
            classified_static: self.unbound.iter().sum(),
            #[cfg(feature = "barrier-census-closure")]
            classified_dynamic: self.unbound_dynamic.iter().sum(),
            // Filled by the CpuGsw seam, which is the only place that can see both the census and
            // `PerfCounters`. Zero here is a placeholder, never an observation.
            #[cfg(feature = "barrier-census-closure")]
            static_unbound_exits: 0,
            #[cfg(feature = "barrier-census-closure")]
            dynamic_miss_exits: 0,
            #[cfg(feature = "barrier-census-closure")]
            rejected_unattributed: self.rejected_unattributed,
            #[cfg(feature = "barrier-census-closure")]
            dynamic_rejected_unattributed: self.dynamic_rejected_unattributed,
            #[cfg(feature = "barrier-census-closure")]
            rejected_barrier_overwrites: self.rejected_barrier_overwrites,
            #[cfg(feature = "barrier-census-closure")]
            dormant_heat_sites,
            #[cfg(feature = "barrier-census-closure")]
            dormant_heat_truncated_static,
            #[cfg(feature = "barrier-census-closure")]
            dormant_heat_truncated_dynamic,
            #[cfg(feature = "barrier-census-closure")]
            dormant_heat_distinct_sites,
            #[cfg(feature = "barrier-census-closure")]
            rejected_sites,
            #[cfg(feature = "barrier-census-closure")]
            rejected_truncated_static,
            #[cfg(feature = "barrier-census-closure")]
            rejected_truncated_dynamic,
            #[cfg(feature = "barrier-census-closure")]
            rejected_distinct_sites,
            #[cfg(feature = "barrier-census-closure")]
            walked_entries_run_wide: self.lane_probes.len() as u64,
            #[cfg(feature = "direct-admission-census")]
            admission_declines: AdmissionDecline::ALL
                .iter()
                .map(|kind| (kind.label(), self.admission_declines[*kind as usize]))
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

    #[cfg(feature = "direct-admission-census")]
    pub(crate) fn note_admission_decline(&mut self, kind: AdmissionDecline) {
        if let Some(census) = self.direct_barrier_census.as_mut() {
            census.note_admission_decline(kind);
        }
    }

    /// Record why a static successor was unbound. No-op unless the census is allocated, and the
    /// CALLER still gates on `barrier_census_active` so the key construction is skipped too.
    pub(crate) fn note_unbound_target(&mut self, kind: UnboundTarget, linear: u32) {
        if let Some(census) = self.direct_barrier_census.as_mut() {
            census.note_unbound(kind);
            if kind == UnboundTarget::Rejected {
                census.note_unbound_rejected_at(linear);
                // The sibling histogram. `note_unbound_rejected_at` resolves this linear to an
                // opcode SHAPE and may fail to; this records the ADDRESS and cannot.
                #[cfg(feature = "barrier-census-closure")]
                census.note_rejected_site_at(linear, false);
            }
            // B.3. The linear was already in hand and already discarded for this class; recording
            // it is one more `if kind ==` arm and one more map, which is the smallest change that
            // can answer "concentrated or diffuse" for the largest remaining lever.
            #[cfg(feature = "barrier-census-closure")]
            if kind == UnboundTarget::DormantHeat {
                census.note_dormant_heat_at(linear, false);
            }
        }
    }

    /// Record that a compile walk from `entry_linear` ran, and which lane matcher (if any) fired
    /// on it. See `DirectBarrierCensus::lane_probes` for why this is keyed on the block entry and
    /// why it is a set rather than a tally.
    ///
    /// The CALLER gates on `barrier_census_active` before building the bits, the same contract
    /// `note_barrier_census_interpreted` carries: this sits inside the compile loop and inside
    /// `compile_with_page_len`'s prefix search, so an `is_some` check inside the callee would be
    /// paid once per slot per search step.
    #[cfg(feature = "barrier-census-closure")]
    pub(crate) fn note_lane_probe(&mut self, entry_linear: u32, bits: u8) {
        if let Some(census) = self.direct_barrier_census.as_mut() {
            census.note_lane_probe(entry_linear, bits);
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
            callout_port_v86_served: self.stalls.callout_port_v86_served,
            callout_interpret_one_executed: self.stalls.callout_interpret_one_executed,
            callout_interpret_one_resync: self.stalls.callout_interpret_one_resync,
            callout_interpret_one_resync_fault: self.stalls.callout_interpret_one_resync_fault,
            callout_interpret_one_abnormal: self.stalls.callout_interpret_one_abnormal,
            callout_interpret_one_demoted: self.stalls.callout_interpret_one_demoted,
            callout_interpret_one_rows: InterpretOneRow::ALL
                .iter()
                .map(|row| {
                    let tally = self.stalls.callout_interpret_one_rows[row.index()];
                    crate::InterpretOneRowCounts {
                        row: row.label(),
                        executed: tally.executed,
                        resync: tally.resync,
                        resync_fault: tally.resync_fault,
                        demoted: tally.demoted,
                    }
                })
                .collect(),
            callout_deferred_code_writes: self.stalls.callout_deferred_code_writes,
            callout_slot_cap_hits: self.stalls.callout_slot_cap_hits,
            reject_callout_privileged: self.stalls.reject_callout_privileged,
            callout_governor_trials: self.stalls.callout_governor_trials,
            callout_governor_lazy: self.stalls.callout_governor_lazy,
            callout_governor_io_touching: self.stalls.callout_governor_io_touching,
            segment_write_block_head_entries: self.stalls.segment_write_block_head_entries,
            segment_write_block_head_insns: self.stalls.segment_write_block_head_insns,
            lane_trials: self.stalls.lane_trials,
            lane_trial_installs: self.stalls.lane_trial_installs,
            disp_lane_registrations: self.stalls.disp_lane_registrations,
            imm8_lane_registrations: self.stalls.imm8_lane_registrations,
            count_lane_registrations: self.stalls.count_lane_registrations,
            decode_pack_late_view_miss: self.stalls.decode_pack_late_view_miss,
            x87_top_retires_suppressed: self.stalls.x87_top_retires_suppressed,
            x87_top_sticky_crossings: self.stalls.x87_top_sticky_crossings,
            decline_memo_hits: self.stalls.decline_memo_hits,
            decline_memo_advances: self.stalls.decline_memo_advances,
            decline_memo_sweeps: self.stalls.decline_memo_sweeps,
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
                #[cfg(feature = "barrier-census-closure")]
                census.note_rejected_site_at(linear, true);
            }
            #[cfg(feature = "barrier-census-closure")]
            if kind == UnboundTarget::DormantHeat {
                census.note_dormant_heat_at(linear, true);
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

    /// One increment on the call-out's already-off-native path, for the same reason
    /// `note_callout_executed` is ungated: the gate would cost as much as the work.
    pub(crate) fn note_callout_port_v86_served(&mut self) {
        self.stalls.callout_port_v86_served += 1;
    }

    /// The `InterpretOne` family's five increments, all on the helper's already-off-native path
    /// or on a compile walk, and ungated for the reason `note_callout_executed` is.
    ///
    /// Four of the five keep a PER-ROW count beside the scalar, and the pair costs one extra
    /// increment on a path that is already off the native lane -- the row index is a field of the
    /// cell the helper is holding, so there is no lookup. Nothing is gated: these sit exactly
    /// where the scalars already sat.
    pub(crate) fn note_interpret_one_executed(&mut self, row: InterpretOneRow) {
        self.stalls.callout_interpret_one_executed += 1;
        self.stalls.callout_interpret_one_rows[row.index()].executed += 1;
    }

    pub(crate) fn note_interpret_one_resync(&mut self, row: InterpretOneRow) {
        self.stalls.callout_interpret_one_resync += 1;
        self.stalls.callout_interpret_one_rows[row.index()].resync += 1;
    }

    pub(crate) fn note_interpret_one_resync_fault(&mut self, row: InterpretOneRow) {
        self.stalls.callout_interpret_one_resync_fault += 1;
        self.stalls.callout_interpret_one_rows[row.index()].resync_fault += 1;
    }

    /// No per-row sibling; see the `callout_interpret_one_rows` field for why.
    pub(crate) fn note_interpret_one_abnormal(&mut self) {
        self.stalls.callout_interpret_one_abnormal += 1;
    }

    pub(crate) fn note_interpret_one_demoted(&mut self, row: InterpretOneRow) {
        self.stalls.callout_interpret_one_demoted += 1;
        self.stalls.callout_interpret_one_rows[row.index()].demoted += 1;
    }

    pub(crate) fn note_deferred_code_write(&mut self) {
        self.stalls.callout_deferred_code_writes += 1;
    }

    pub(crate) fn note_callout_slot_cap_hit(&mut self) {
        self.stalls.callout_slot_cap_hits += 1;
    }

    pub(crate) fn note_reject_callout_privileged(&mut self) {
        self.stalls.reject_callout_privileged += 1;
    }

    /// The admission governor's three counters. Here rather than beside the governor's storage in
    /// `direct.rs` so every `note_*` in the call-out family lives in one place.
    pub(crate) fn note_callout_governor_trial(&mut self) {
        self.stalls.callout_governor_trials += 1;
    }

    pub(crate) fn note_callout_governor_lazy(&mut self) {
        self.stalls.callout_governor_lazy += 1;
    }

    pub(crate) fn note_callout_governor_io_touching(&mut self) {
        self.stalls.callout_governor_io_touching += 1;
    }

    /// Every call-out the helper has entered so far. The governor reads it either side of one
    /// trial entry to learn whether the trial served anything at all; a block whose call-out sits
    /// behind an untaken branch serves nothing and must not classify from that.
    pub(crate) fn callout_executed_count(&self) -> u64 {
        self.stalls.callout_executed
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
