// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Admission and storage for page-local direct-code blocks.

mod callout;
#[cfg(feature = "direct-callout-attribution")]
mod callout_attribution;
pub(crate) mod census;
mod classify;
mod emit;
mod native_exit;

/// Re-exported rather than moved-and-repathed: every one of these names is referenced as
/// `jit::direct::X` from `run.rs`, `lib.rs` and the emitter, and the extraction is pure motion,
/// so the paths must not move with the text.
pub(crate) use native_exit::{
    DirectEntryFn, NativeBlockTrace, NativeExit, SideExitReason, UnresolvedReason,
};

pub(crate) use callout::{
    CALL_OUT_STACK_FRAME_DWORDS, CallOutHelper, CallOutSlotCounts, CallOutTable,
};
#[cfg(test)]
pub(crate) use callout::{
    STATUS_STEP_BREAK_BIT, pop_all_dword_for_test, port_read_al_dx_for_test,
    push_all_dword_for_test,
};
#[cfg(feature = "direct-callout-attribution")]
pub(crate) use callout_attribution::{
    CallOutAttribution, CallOutOutcome, direct_callout_attribution_default,
};

use std::{collections::HashMap, sync::Arc};

use izarravm_core::{CpuPersona, GswMode};

use census::{BarrierStop, SuffixSeed, record_structural_barrier};
// The stall/census TAXONOMY lives in `census.rs` beside the builder that already consumed it
// (`stall_snapshot`, `snapshot`), moved verbatim to keep this file under the source-line ceiling.
// Re-exported from here because every out-of-module path names them through `jit::direct`.
#[cfg(feature = "direct-admission-census")]
pub(crate) use census::AdmissionDecline;
pub(crate) use census::{
    BlockCacheStats, DirectBarrierCensus, DirectStallTally, DormantReason, LinkClearCause,
    LinkRefusal, UnboundTarget, barrier_census_default,
};
#[cfg(feature = "direct-link-refusal-census")]
pub(crate) use census::{DirectLinkRefusalCensus, direct_link_refusal_census_default};

use super::code_watch::NativeCodeWatch;
#[cfg(target_os = "windows")]
use super::encoder::Xmm;
use super::encoder::{Encoder, Label, Reg};
use super::exec_mem::ExecutableArena;
use super::native_x87::{NativeX87Insn, NativeX87MemoryAccess, NativeX87MemoryDirection};
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
use super::x87_avx2_emit::{
    Avx2X87EmitContext, emit_enter as emit_x87_enter, emit_native_x87, emit_spill as emit_x87_spill,
};
use crate::{
    AddressSize, CpuGsw, DecodeGroup, DecodedInsn, DecodedOperand, DirectBarrierCensusRow,
    DirectBarrierCensusSnapshot, OperandSize, PodKeyBuildHasher, Prefixes, Registers, SegmentIndex,
    SegmentRegister, U32BuildHasher,
};

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
use super::code_watch::NATIVE_CHUNK_SHIFT;
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
use super::fast_map::{
    NATIVE_KIND_MASK, NATIVE_MODE13_KIND, NATIVE_PAGE_SHIFT, NATIVE_PAGE_USER, NATIVE_PAGE_WATCHED,
    NATIVE_PAGE_WRITABLE, NATIVE_RAM_KIND, NATIVE_UNAVAILABLE_BIAS, NativeMapBases,
};

#[cfg(not(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
#[derive(Clone, Copy)]
struct NativeMapBases;

pub(crate) const MAX_BLOCK_INSTRUCTIONS: usize = 32;
pub(crate) const HOT_LOOKUP_LEN: usize = 65_536;
pub(crate) const MAX_CHAIN_BLOCKS: usize = 256;
pub(crate) const MIN_STANDALONE_INSTRUCTIONS: u8 = 8;
const MAX_BLOCK_STACK_ACCESSES: u8 = 4;
pub(crate) const MAX_X87_BLOCK_INSTRUCTIONS: usize = 12;
pub(crate) const MAX_X87_SLOTS: u8 = 8;
/// Interpreter call-out slots per block. A BUDGET bound, not a code-size one: every call-out
/// widens `compute_iteration_upper` by its worst-case interpreter charge plus worst-case bus I/O
/// (see the derivation there), and that bound divides the run's remaining budget to pick the
/// chain quota. Left unbounded, a block of nothing but port reads would inflate the bound far
/// enough to cut chains short across the whole cache. Four is the same order as
/// `MAX_BLOCK_STACK_ACCESSES` and comfortably above the one-or-two port reads a poll idiom
/// carries; `brk_cap` byte-identity on the no-call-out fixture is the isolation that this bound
/// does not leak into blocks that hold none.
pub(crate) const MAX_BLOCK_CALLOUT_SLOTS: u8 = 4;
/// Instruction bound for a block that holds ANY memory-ALU slot. It is a CODE SIZE bound, not a
/// timing one, and it is the second half of a pair with `MAX_MEMORY_ALU_SLOTS`.
///
/// Every installed block owns exactly one host page (`ExecutableArena::install` refuses a
/// compilation longer than `host_page_len`), so a block's emitted bytes are the scarce resource.
/// Memory-ALU slots are by far the largest emitters in the kind table: each one lowers an
/// address computation, a fast-map probe, a read, the ALU op, a watched/device fallback and a
/// transactional exit stub, which is of the order of a kilobyte per slot against a few tens of
/// bytes for an ordinary register slot. The size is pinned from the test side by
/// `repeated_memory_alu_root_splits_below_one_host_page_and_retires_natively`: it builds a root of
/// nothing but memory-ALU instructions and asserts the block that comes out is
/// `MAX_MEMORY_ALU_SLOTS` long and within a byte budget that is already most of one page.
///
/// `MAX_MEMORY_ALU_SLOTS` therefore bounds the memory-ALU term and this constant bounds
/// EVERYTHING ELSE sharing the page with it — the difference of the two is how many non-memory-ALU
/// slots may join, which at these values is one. Together they keep such a block inside its page
/// without going through `compile_with_page_len`'s fallback, which re-compiles a binary search of
/// shorter prefixes and lands on a shorter block anyway. Exceeding the page is a COST, never a
/// correctness question: the fallback is the safety net, this pair is the fast path.
/// `direct_byte_alu_memory_destination_matches_the_interpreter` documents the resulting worst
/// shape from the test side, and depends on it to keep its ops mid-block.
///
/// The bound is deliberately unrelated to the chain quota: `compute_global_block_upper` already
/// charges every hop `MAX_BLOCK_INSTRUCTIONS` instructions of worst-case bus traffic, so any value
/// up to that cap is covered there and raising this one cannot under-budget a chain.
///
/// MEASURED 2026-08-01 (phase 3 task 3), so nobody re-runs it: 8 and 16 are both admissible and
/// both really change formation — on quake the mean instructions per dispatcher entry rises about
/// 3% and installed blocks fall about a sixth, on doom the same figures move by under a tenth of a
/// percent. Neither bought wall. Six-pair A/B/B/A ladders against this value read noise_only on
/// BOTH fixtures with the geomean and min-wall estimators agreeing (doom especially flat), and the
/// SMC churn counters did not move either way. The break binds, and relaxing it is free of
/// benefit; 4 stays until something changes what a memory-ALU slot costs to emit.
const MAX_MEMORY_ALU_BLOCK_INSTRUCTIONS: usize = 4;
const MAX_MEMORY_ALU_SLOTS: u8 = 3;
/// Per-hop chain clock bound for a block with any x87 slot. Derived, not chosen:
/// the worst ADMISSIBLE slot is an `FpOpClass::IntConvert16` memory arith (raw 20, I586
/// scale 256, ceil(20 * 256 / 8) = 640 core clocks), MAX_X87_SLOTS of those is 5,120, plus
/// MAX_X87_BLOCK_INSTRUCTIONS non-x87 slots at the 10-clock worst constant charge is 120.
/// `max_x87_block_core_clocks_dominates_every_shape_in_the_metadata_table` re-derives this
/// from the metadata table and fails the build if a costlier shape ever joins it.
///
/// Raised from 3,928 (the IntConvert32 worst) AHEAD of the first IntConvert16 member, so that
/// slice measures its own effect cleanly. The raise alone was measured on the pinned corpus:
/// all six anchors byte-identical, the chain quota shrinks (a chain hop is assumed costlier,
/// so chains return to the dispatcher earlier), entries up about 136,000, roughly 8 ms of wall.
pub(crate) const MAX_X87_BLOCK_CORE_CLOCKS: u64 = 5_240;
/// Mutable imm32 lanes per block. One lane-admitted `0x81 /r` slot claims one lane; slots past
/// this cap keep their baked immediate, which is a missed optimisation and never a correctness
/// question. Four covered Doom's paired patch sites; duke3d's Build-engine patch bursts rewrite
/// around ten sites per region per iteration (duke586-smc-trace-20260808.txt, the 0x2AFxxx site
/// families), so a block spanning such a region needs the larger budget for its lanes to absorb
/// the whole burst — one uncovered patched slot is enough to keep killing the block.
pub(crate) const MAX_BLOCK_IMM_LANES: usize = 12;
/// The only store width a lane accepts. The dword field is patched whole or not at all; a byte
/// or word patch of it takes the normal invalidation path.
pub(crate) const IMM_LANE_WIDTH: u32 = 4;
/// Empty slot in a block's lane array. Physical address 0 is a real address (the real-mode IVT),
/// so the sentinel has to be an address no six-byte instruction's immediate can start at.
const NO_IMM_LANE: u32 = u32::MAX;
pub(crate) const DEFAULT_ENTRY_CAP: usize = 131_072;
const DEFAULT_DECODE_SLOT_COUNT: usize = 4_096;
const BLOCK_PAGE_SHIFT: u32 = 12;
#[cfg(not(test))]
const DEFAULT_ADMISSION_HEAT: u8 = 8;
#[cfg(test)]
const DEFAULT_ADMISSION_HEAT: u8 = 1;

#[cfg(test)]
pub(crate) use super::smc_heat::SMC_HEAT_THRESHOLD;
pub(crate) use super::smc_heat::{SMC_HEAT_EPOCH_SHIFT, SmcHeatMap};

// Track C C1d-pre hoist: LinkTarget/BlockPortal (+ zero_portal)/LinkCell/LinkSource moved,
// verbatim, into the backend-neutral `jit::links` module (see that module's doc comment for the
// mechanism-neutrality contract). `LinkSource` is generic over its source-id type; Direct
// instantiates it with its own `BlockId`, which stays here since it carries Direct's
// generational-slot semantics.
use super::links::LinkSource;
pub(crate) use super::links::{BlockPortal, LinkCell, LinkTarget, zero_portal};

/// Everything that can change the meaning of bytes at a linear entry point.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct BlockKey {
    pub linear: u32,
    pub physical: u32,
    pub mode_key: u32,
}

impl BlockKey {
    pub(crate) const fn new(linear: u32, physical: u32, mode_key: u32) -> Self {
        Self {
            linear,
            physical,
            mode_key,
        }
    }

    pub(crate) const fn linear(self) -> u32 {
        self.linear
    }

    fn hot_index(self) -> usize {
        self.linear as usize & (HOT_LOOKUP_LEN - 1)
    }
}

/// What one guest write did to the block cache. `blocks` is the retire count the invalidation
/// choke has always used; the three lane figures are the diagnostic trio the campaign reads.
///
/// `lane_accepts` is what suppresses this write's SMC heat contribution: the whole point of a lane
/// is that patching an immediate stops looking like code churn to the demotion gate.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct RangeInvalidation {
    pub(crate) blocks: usize,
    pub(crate) lane_accepts: u32,
    pub(crate) lane_reject_width: u32,
    pub(crate) lane_reject_address: u32,
    /// Block keys the overlap scan examined. The scan is O(keys registered on the written page)
    /// and a self-modifying guest walks that list once per store, so this is the quantity that
    /// decides whether this function is cheap or is the run. One add per page, not per key.
    pub(crate) keys_scanned: u32,
}

/// Validated guest extent for one compiled block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BlockSpan {
    pub key: BlockKey,
    pub guest_len: u16,
    pub instructions: u8,
}

const SEGMENT_ORDER: [SegmentIndex; 6] = [
    SegmentIndex::Es,
    SegmentIndex::Cs,
    SegmentIndex::Ss,
    SegmentIndex::Ds,
    SegmentIndex::Fs,
    SegmentIndex::Gs,
];

/// Segment state baked into one direct translation. Only data segments actually used by the
/// block are retained. A linked target must have the identical snapshot, so validating the root
/// block also validates every body reached through its successor cells.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SegmentLayout {
    cs: SegmentRegister,
    data: [SegmentRegister; 6],
    used: u8,
}

impl SegmentLayout {
    /// `pinned_segments` is the PINNED set — every segment `data_matches` will compare on entry —
    /// while the accessibility check below runs over the ACCESSED set only. The two used to be the
    /// same mask; they came apart when `MovSegToReg` arrived, which needs a segment's selector
    /// pinned without asserting anything about whether memory can be reached through it.
    ///
    /// The caller accumulates the pinned set through `DirectKind::pinned_segments`, which is the
    /// single definition of the question. This used to take the selector mask and OR the three
    /// together here, which put the union in one place and the question in three.
    pub(crate) fn capture(
        cpu: &CpuGsw,
        read_segments: u8,
        write_segments: u8,
        pinned_segments: u8,
    ) -> Option<Self> {
        debug_assert_eq!(
            pinned_segments & (read_segments | write_segments),
            read_segments | write_segments,
            "every accessed segment must also be pinned",
        );
        let data = SEGMENT_ORDER.map(|segment| cpu.registers.segment(segment));
        let used = pinned_segments;
        for segment in SEGMENT_ORDER {
            let bit = segment_bit(segment);
            if (read_segments | write_segments) & bit == 0 {
                continue;
            }
            let descriptor = data[segment_index(segment)];
            if !segment_access_supported(
                cpu,
                descriptor,
                read_segments & bit != 0,
                write_segments & bit != 0,
            ) {
                return None;
            }
        }
        Some(Self {
            cs: cpu.registers.cs(),
            data,
            used,
        })
    }

    pub(crate) fn cs_matches(self, cpu: &CpuGsw) -> bool {
        self.cs == cpu.registers.cs()
    }

    pub(crate) fn data_matches(self, cpu: &CpuGsw) -> bool {
        SEGMENT_ORDER.into_iter().all(|segment| {
            self.used & segment_bit(segment) == 0
                || self.data[segment_index(segment)] == cpu.registers.segment(segment)
        })
    }

    pub(crate) fn all_data_matches(self, cpu: &CpuGsw) -> bool {
        SEGMENT_ORDER
            .into_iter()
            .all(|segment| self.data[segment_index(segment)] == cpu.registers.segment(segment))
    }

    pub(crate) fn link_compatible(&self, target: &Self) -> bool {
        self.cs == target.cs && self.data == target.data
    }

    /// The pinned selector for `segment`, from whichever of the two snapshots holds it. CS lives
    /// in its own field and is pinned for every block; the other five must be in `used`, which
    /// `DirectKind::selector_segment` is what guarantees for a `MovSegToReg` slot.
    pub(crate) fn selector(self, segment: SegmentIndex) -> u16 {
        if segment == SegmentIndex::Cs {
            return self.cs.selector;
        }
        debug_assert_ne!(self.used & segment_bit(segment), 0);
        self.data[segment_index(segment)].selector
    }

    pub(crate) fn descriptor(self, segment: SegmentIndex) -> SegmentRegister {
        debug_assert_ne!(self.used & segment_bit(segment), 0);
        self.data[segment_index(segment)]
    }
}

fn segment_access_supported(
    cpu: &CpuGsw,
    descriptor: SegmentRegister,
    read: bool,
    write: bool,
) -> bool {
    if !cpu.is_protected_mode() || cpu.is_v86_mode() {
        return true;
    }
    let access = descriptor.access;
    if access & 0x80 == 0 || access & 0x10 == 0 {
        return false;
    }
    let code = access & 0x08 != 0;
    let expand_down = !code && access & 0x04 != 0;
    if expand_down || (read && code && access & 0x02 == 0) {
        return false;
    }
    !write || (!code && access & 0x02 != 0)
}

impl BlockSpan {
    pub(crate) fn new(key: BlockKey, guest_len: usize, instructions: usize) -> Option<Self> {
        if guest_len == 0 || !(1..=MAX_BLOCK_INSTRUCTIONS).contains(&instructions) {
            return None;
        }
        let guest_len = u16::try_from(guest_len).ok()?;
        let last = u32::from(guest_len) - 1;
        let linear_last = key.linear.checked_add(last)?;
        let physical_last = key.physical.checked_add(last)?;
        if key.linear >> 12 != linear_last >> 12 || key.physical >> 12 != physical_last >> 12 {
            return None;
        }
        Some(Self {
            key,
            guest_len,
            instructions: instructions as u8,
        })
    }
}

/// Complete byte-dependent reason that a short direct block cannot compile. Rejected spans are
/// page-local so one physical-page index can find every write that could make the decision stale.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RejectedSpan {
    key: BlockKey,
    guest_len: u16,
}

impl RejectedSpan {
    pub(crate) fn new(key: BlockKey, guest_len: usize) -> Option<Self> {
        let guest_len = u16::try_from(guest_len).ok().filter(|len| *len != 0)?;
        let last = u32::from(guest_len) - 1;
        let linear_last = key.linear.checked_add(last)?;
        let physical_last = key.physical.checked_add(last)?;
        if key.linear >> BLOCK_PAGE_SHIFT != linear_last >> BLOCK_PAGE_SHIFT
            || key.physical >> BLOCK_PAGE_SHIFT != physical_last >> BLOCK_PAGE_SHIFT
        {
            return None;
        }
        Some(Self { key, guest_len })
    }

    #[cfg(test)]
    pub(crate) const fn key(self) -> BlockKey {
        self.key
    }

    #[cfg(test)]
    pub(crate) const fn guest_len(self) -> u16 {
        self.guest_len
    }
}

/// Metadata for one sealed native block. Arena compaction can stale a copied entry address, so
/// callers re-resolve `id` immediately before entering native code.
///
/// This struct is COPIED several times per Direct entry (probe, the `run_direct_block`
/// argument, the pre-entry re-resolve), so its size is memcpy traffic multiplied by tens of
/// millions of entries. `segment_layout` used to live here and was 116 of its 240 bytes; it
/// now sits in `BlockCache::segment_layouts`, fetched once per entry instead of riding every
/// copy. Keep new fields out unless they are genuinely read on a uniform-fetch entry, and
/// keep `compiled_block_stays_small_enough_to_copy_per_entry` truthful.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CompiledBlock {
    id: BlockId,
    span: BlockSpan,
    entry: usize,
    body_entry: usize,
    code_len: u32,
    fetch_lens: [u8; MAX_BLOCK_INSTRUCTIONS],
    raw_clocks: u16,
    weighted_fp_clocks: u32,
    byte_reads: u8,
    word_reads: u8,
    dword_reads: u8,
    byte_stores: u8,
    word_stores: u8,
    dword_stores: u8,
    memory_cpl3: bool,
    has_wide_accesses: bool,
    self_loop: bool,
    has_x87: bool,
    callout_slots: CallOutSlotCounts,
    x87_entry_top: u8,
    x87_exit_top: u8,
    dynamic_successor: bool,
    successors: [Option<LinkTarget>; 2],
}

impl CompiledBlock {
    pub(crate) fn id(&self) -> BlockId {
        self.id
    }

    pub(crate) fn span(&self) -> BlockSpan {
        self.span
    }

    /// Whether this block was barred from publishing successors because it overwrites a segment
    /// register.
    ///
    /// DERIVED, not stored, and that is a size decision rather than a stylistic one.
    /// `compiled_block_stays_small_enough_to_copy_per_entry` pins `CompiledBlock` at 120 bytes
    /// and the budget is exactly full, so a `bool` field would round the struct to 128 and cost
    /// eight bytes on every per-entry memcpy. `CallOutSlotCounts` is bit-packed for the same
    /// reason.
    ///
    /// The equivalence is exact rather than approximate. Only TWO arms of `compile`'s `successors`
    /// match produce `[None, None]`: the segment-write arm, which is reached only when
    /// `dynamic_successor` was already forced false by its own `!segment_write_block` term; and
    /// the Ret/JmpMem/CallReg/CallMem arm, which is reached only when `segment_write_block` is
    /// false and therefore sets `dynamic_successor` TRUE from the identical kind list. So the
    /// second conjunct is what separates them, and nothing else in the function can produce the
    /// pair.
    ///
    /// What this costs the block is not one extra boundary. `run_direct_block` computes
    /// `chain_eligible` from `has_linked_successor`, so a block with no successors can never
    /// chain and its quota is clamped to 1: every entry runs this block alone and returns through
    /// the full prologue and epilogue.
    pub(crate) fn is_segment_write_block(&self) -> bool {
        self.successors == [None, None] && !self.dynamic_successor
    }

    pub(crate) fn entry_ptr(&self) -> *const u8 {
        self.entry as *const u8
    }

    fn body_ptr(&self) -> usize {
        self.body_entry
    }

    pub(crate) fn fetch_lens(&self) -> &[u8] {
        &self.fetch_lens[..usize::from(self.span.instructions)]
    }

    pub(crate) fn raw_clocks(&self) -> u32 {
        u32::from(self.raw_clocks)
    }

    /// How many interpreter call-out slots this block carries, of any class.
    ///
    /// TEST-ONLY, and the gate is the point rather than tidiness. Nothing in the budget path reads
    /// this: `compute_iteration_upper` prices the two CLASS splits below, because a port slot and
    /// a memory slot cost wildly different amounts of bus traffic and pricing both at the worst of
    /// the two would inflate every doom port block by eight dword accesses it cannot make; and the
    /// `MAX_BLOCK_CALLOUT_SLOTS` cap is applied during the compile walk, against a local, before a
    /// `CompiledBlock` exists. Its one live caller is `invoke_native_entry`'s trap guard
    /// (`cpu_jit_direct_execution_test.rs`), which refuses to enter a call-out-bearing block
    /// through a path that does not publish `CpuGsw::native_callout`.
    ///
    /// It is a DERIVED value -- `port() + memory()` -- so it can never disagree with the split it
    /// sums, and an assertion comparing the two is vacuous. The place that can genuinely notice a
    /// classless slot is `BlockCache::install`, where the compile walk's independently accumulated
    /// total is still in hand.
    #[cfg(test)]
    pub(crate) fn callout_slots(&self) -> u32 {
        self.callout_slots.total()
    }

    /// Call-out slots whose helper can reach `check_io_permission`, i.e. the port class. The
    /// dispatch-time privilege refusal keys on this rather than on `callout_slots` so a PUSHAD
    /// block still runs for a CPL-3 or V86 guest: PUSHAD probes no TSS, so the reason the port
    /// class is refused there does not apply to it. See the class table in `jit/direct/callout.rs`.
    pub(crate) fn callout_port_slots(&self) -> u32 {
        self.callout_slots.port()
    }

    /// Call-out slots whose helper moves a guest stack frame, i.e. the memory class. Priced at
    /// `CALL_OUT_STACK_FRAME_DWORDS` dword accesses each in `compute_iteration_upper`.
    pub(crate) fn callout_memory_slots(&self) -> u32 {
        self.callout_slots.memory()
    }

    pub(crate) fn weighted_fp_clocks(&self) -> u32 {
        self.weighted_fp_clocks
    }

    pub(crate) fn byte_reads(&self) -> u8 {
        self.byte_reads
    }

    pub(crate) fn word_reads(&self) -> u8 {
        self.word_reads
    }

    pub(crate) fn dword_reads(&self) -> u8 {
        self.dword_reads
    }

    pub(crate) fn byte_stores(&self) -> u8 {
        self.byte_stores
    }

    pub(crate) fn word_stores(&self) -> u8 {
        self.word_stores
    }

    pub(crate) fn dword_stores(&self) -> u8 {
        self.dword_stores
    }

    pub(crate) fn memory_cpl3(&self) -> bool {
        self.memory_cpl3
    }

    pub(crate) fn has_wide_accesses(&self) -> bool {
        self.has_wide_accesses
    }

    pub(crate) fn is_self_loop(&self) -> bool {
        self.self_loop
    }

    pub(crate) fn has_x87(&self) -> bool {
        self.has_x87
    }

    pub(crate) fn x87_entry_top(&self) -> Option<u8> {
        self.has_x87.then_some(self.x87_entry_top)
    }

    /// Edge compatibility, for STATIC successors (Jmp/Jcc/Call/fallthrough) and for the dynamic
    /// RET PIC path alike. It used to be static-only: the dynamic path layered an extra `has_x87`
    /// equality on top of this in both directions, because `emit_completed_dynamic_path` emitted
    /// neither the float-to-integer boundary spill nor the integer-to-float portal-field
    /// selection. It emits both now, so `target_eip` no longer changes WHICH edges link, only how
    /// the cell is written, and this is the only x87 edge predicate there is.
    ///
    /// The has_x87 pair is a real three-case rule now, not a dead clause. It used to read
    /// `self.has_x87 == target.has_x87 && (!self.has_x87 || self.x87_exit_top ==
    /// target.x87_entry_top)`, which made the TOP comparison unreachable for a genuinely mixed
    /// pair: the equality already returned false before the TOP clause could run. Deleting just
    /// the equality would have made that TOP clause live for mixed pairs instead of dead, and
    /// wrong: an integer block's `x87_entry_top`/`x87_exit_top` are not "no x87 state", they are
    /// a snapshot of `cpu.fpu.top()` taken at compile time and stored unconditionally for every
    /// block (see `compile()`, around the `x87_entry_top`/`x87_exit_top` locals), an arbitrary
    /// value with no relationship to anything at link time. Comparing a float block's real
    /// `x87_exit_top` against that snapshot would accept or refuse edges at random.
    fn link_compatible(&self, target: &Self) -> bool {
        // The segment-snapshot half of this predicate moved to the call site with
        // `segment_layout`; both halves must still hold for an edge to link.
        if self.span.key.mode_key != target.span.key.mode_key
            || self.memory_cpl3 != target.memory_cpl3
        {
            return false;
        }
        match (self.has_x87, target.has_x87) {
            // Both integer: neither side carries x87 state, so there is no TOP to reconcile.
            (false, false) => true,
            // Both float: a chained transfer skips the target's own prologue, so the physical
            // x87 cache and packed status/tag word stay resident in host registers across the
            // jump. Every native x87 op the target emitted was compiled against its own entry
            // TOP, so the source's exit TOP must match it exactly, unchanged from before.
            (true, true) => self.x87_exit_top == target.x87_entry_top,
            // Float source, integer target: link. The edge is marked spilling (see
            // `LinkCell::mark_spilling`, set in `try_link_inner`), and the emitted jump flushes
            // the live x87 cache back to `CpuGsw.fpu` before handing control over. The target
            // has no TOP of its own to pin against, so there is no TOP condition here.
            (true, false) => true,
            // Integer source, float target: link, THROUGH THE SHARED PAD. A chained entry jumps
            // to a published address and skips the target's own prologue, and `emit_x87_enter`
            // sits ABOVE `body_offset` (see `emit()`, where body_offset is captured right after
            // the `x87_entry_top.is_some()` enter block), so entering at `body` would leave the
            // target's XMM4-11 cache unloaded and its baked entry TOP unpinned. What makes the
            // edge legal is that an integer source does not publish `body`: both emitters select
            // `BlockPortal::integer_entry`, which for a float target is the shared x87 re-entry
            // pad, and the pad does exactly the prologue's work after guarding the baked TOP
            // against the CPU's live one.
            //
            // The frame induction this arm used to provide by REFUSING is preserved, not
            // abandoned, and it is what the float-to-integer arm above depends on. That crossing
            // reloads RSI from STACK_SAVED_RSI and restores XMM6-11 from the frame, slots only an
            // x87 prologue writes. The pad writes the same slots, so every block that can reach
            // such a crossing was entered either through a prologue or through the pad. Uniform
            // frame length alone would not make that read safe. `try_link_inner` refuses this
            // shape outright when no pad could be built (`LinkRefusal::MissingX87Pad`), which is
            // what keeps the induction total.
            (false, true) => true,
        }
    }
}

/// Stable metadata-slot identity. The low 16 bits select the slot; the high bits prevent a stale
/// copied ID from naming a later occupant after that slot is recycled.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct BlockId(u64);

impl BlockId {
    const INDEX_BITS: u32 = u16::BITS;
    const MAX_GENERATION: u64 = u64::MAX >> Self::INDEX_BITS;

    fn new(index: u16, generation: u64) -> Option<Self> {
        (generation != 0 && generation <= Self::MAX_GENERATION)
            .then_some(Self((generation << Self::INDEX_BITS) | u64::from(index)))
    }

    fn index(self) -> usize {
        usize::from(self.0 as u16)
    }

    #[cfg(feature = "direct-link-refusal-census")]
    fn generation(self) -> u64 {
        self.0 >> Self::INDEX_BITS
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlockState {
    Seen,
    /// Carries the reason it was parked so `classify_unbound_target` can separate the
    /// heat-demoted (recoverable) dormants from the rest. The reason is stamped by the FIRST
    /// park: `dormant()` only rewrites a `Seen` entry, so a second gate firing on an
    /// already-dormant key counts in `DirectStallTally::dormant` but does not restamp the state.
    Dormant(DormantReason),
    Rejected(RejectedSpan),
    Compiled(BlockId),
}

#[derive(Clone, Copy)]
struct HotEntry {
    key: BlockKey,
    id: BlockId,
    generation: u32,
}

/// Result of observing a block entry. A new key is interpreted once, then becomes eligible for
/// compilation on its next observation.
#[derive(Debug)]
pub(crate) enum BlockProbe {
    Interpret,
    Rejected,
    Compile,
    Ready(BlockId),
}

/// Per-physical-page key index for the SMC invalidation choke. `keys` stays
/// SORTED ascending by `key.physical` so `invalidate_physical_range` can
/// binary-search the overlap window instead of walking every key on the page:
/// nascar-586 held ~950 keys on its hot pages and a RIP-sample profile put
/// 40% of its wall in that walk (2026-08-08 campaign, lever 1).
///
/// `max_span` is the longest guest span (bytes) any Compiled/Rejected state
/// rooted on this page has carried since the vec was last emptied; a span key
/// below `write_start - (max_span - 1)` cannot reach the write. Seen/Dormant
/// keys are points. The bound only grows (a stale-high value is correct, just
/// less tight) and resets with the vec. Spans never cross a 4 KiB page (the
/// block builder refuses page-crossing spans), so a page's own window covers
/// every span its keys can root.
#[derive(Default)]
struct PageKeys {
    keys: Vec<BlockKey>,
    max_span: u32,
}

/// Bounded direct-block cache. Hash lookup is authoritative; the direct-mapped table is only a
/// collision-checked accelerator. Capacity pressure clears the entire cache.
pub(crate) struct BlockCache {
    /// Keyed with `PodKeyBuildHasher`, not std's SipHash: a RIP-sample profile of Quake/586
    /// attributed 3.1 percent of wall to hashing this map's three-`u32` key. Its sibling
    /// `physical_keys` below already used the crate's fast hasher for the same reason.
    entries: HashMap<BlockKey, BlockState, PodKeyBuildHasher>,
    physical_keys: HashMap<u32, PageKeys, U32BuildHasher>,
    blocks: Vec<CompiledBlock>,
    /// Parallel to `blocks`, same `BlockId::index()`. Split out of `CompiledBlock` because it
    /// was 116 of that struct's 240 bytes while every hot-path read goes through a `&self`
    /// method that never needed the copy. Entry reads it exactly once.
    segment_layouts: Vec<SegmentLayout>,
    /// Parallel to `blocks`, same `BlockId::index()`: the physical start of each mutable imm32
    /// lane the block's emitted code reads through, `NO_IMM_LANE` for an unused slot. Out of
    /// `CompiledBlock` for the reason its size pin states — nothing here is read on a block entry,
    /// only at the SMC write choke. A recycled slot is refilled by `install`, so a retired
    /// occupant's lanes can never answer for its successor.
    block_imm_lanes: Vec<[u32; MAX_BLOCK_IMM_LANES]>,
    /// G1 lane trial spend marks: the heat epoch in which `lane_trial_spend` last granted this
    /// key its one compile-through-heat attempt (see `lane_trial_enabled` for the mechanism).
    /// Stale epochs are simply overwritten on the next grant, so the map only ever holds one
    /// entry per key that has EVER been hot — thousands on a Build-engine fixture, cleared with
    /// the rest of the cache storage.
    lane_trial_epochs: HashMap<BlockKey, u32>,
    /// Test-only override of `lane_trial_enabled` — the env gate is a process-global `OnceLock`,
    /// so in-process tests of the trial path set this instead of the environment.
    lane_trial_override: Option<bool>,
    block_portals: Vec<Arc<BlockPortal>>,
    link_cells: Vec<[Arc<LinkCell>; 2]>,
    link_sources: HashMap<usize, LinkSource<BlockId>>,
    outbound: Vec<[Option<BlockId>; 2]>,
    dynamic_next_slots: Vec<u8>,
    inbound: HashMap<BlockId, Vec<LinkSource<BlockId>>>,
    waiting: HashMap<LinkTarget, Vec<LinkSource<BlockId>>>,
    linear_blocks: HashMap<LinkTarget, BlockId>,
    decode_dependencies: Box<[Vec<BlockId>]>,
    block_decode_slots: Vec<Vec<u32>>,
    decode_slot_mask: usize,
    block_link_epochs: Vec<u64>,
    link_epoch: u64,
    block_active: Vec<bool>,
    /// Memoised `global_block_upper`, the chain-quota divisor, indexed by `has_x87`. 0 is unset
    /// and cannot collide with a real value, which is at least 1 on every bus. Its inputs are the
    /// persona timing pair and the bus cost dials, and both move only through
    /// `CpuGsw::set_mode`, which reaches `clear()` below. Cleared there ABOVE the empty-cache
    /// early return, so the clear does not depend on an argument about when that return is taken.
    global_block_upper_cache: [u64; 2],
    /// Memoised `iteration_upper`, THIS block's own cost bound, parallel to `blocks` and indexed
    /// by the same `BlockId::index()`. 0 is unset. Unlike the two-entry table above, a false
    /// "unset" here would only cost a recompute, never a wrong answer, so the sentinel needs no
    /// non-collision argument: the value is at least `scaled_core_upper` and a block with zero
    /// raw clocks simply misses forever.
    ///
    /// Its inputs are the block's own immutable metadata plus the same persona timing pair and
    /// bus cost dials `global_block_upper` reads, so it carries the same epoch key. A recycled
    /// slot is zeroed by `install`, so a stale entry cannot outlive its block.
    iteration_upper_cache: Vec<u64>,
    /// The shared x87 re-entry pad, in its OWN executable mapping rather than in the arena.
    /// Deliberately not a block: `reset_storage` sets `arena = None` and frees it, which would
    /// dangle every `integer_entry` published at a float block, and `compact_arena` relocates
    /// arena contents while portals published at the pad must keep one stable address for the
    /// life of the cache. Built once, lazily, on the first float install, and never replaced.
    ///
    /// `None` means no pad: on a host where the executable mapping cannot be made (allocation
    /// failure, or any target outside x86-64 Windows/Linux), `try_link_inner` REFUSES the
    /// integer-into-float edge, so the cell stays on the zero portal and the exit reports
    /// `StaticUnbound` exactly as it did before this mechanism existed.
    x87_pad: Option<super::exec_mem::ExecutableBuffer>,
    /// The shared store-stub pad (one-lookup store design D4), with each stub's offset in it.
    /// Same lifetime contract as `x87_pad` above: its own executable mapping, built once,
    /// lazily, at the first store-bearing compile with the flag on, never replaced. `None`
    /// after a failed build makes that block fall back to the inline (gate-off) emission.
    store_stub_pad: Option<(super::exec_mem::ExecutableBuffer, [usize; STORE_STUB_COUNT])>,
    /// The shared read-resolve stub pad (one-lookup load design D4). Same lifetime contract as
    /// the two pads above, its own executable mapping because reads gate on `map_bases` alone —
    /// a load-only block has NO code-watch tables, so the read pad must not ride the store
    /// pad's build condition. `None` after a failed build makes that block fall back to the
    /// inline (gate-off) read emission.
    read_stub_pad: Option<(super::exec_mem::ExecutableBuffer, [usize; READ_STUB_COUNT])>,
    /// The `jit_cost_dial_epoch()` the cache above was computed under. The CPU cannot see a bus
    /// dial move, so the memo is keyed on the bus's own epoch rather than on an argument about
    /// who writes the dials. Reading one accessor and comparing beats six accessor calls, five
    /// `max`, three multiplies and a division.
    global_block_upper_epoch: u64,
    /// The `jit_cost_dial_epoch()` `iteration_upper_cache` was computed under. Same key, same
    /// reasoning as `global_block_upper_epoch` above.
    iteration_upper_epoch: u64,
    free_block_slots: Vec<u16>,
    next_block_generation: u64,
    live_blocks: usize,
    hot: Box<[Option<HotEntry>]>,
    hot_generation: u32,
    arena: Option<ExecutableArena>,
    entry_cap: usize,
    disabled: bool,
    backend_enabled: bool,
    auto_admit: bool,
    admission_heat: u8,
    /// Reset-coupling counter for the HOISTED heat map (Track C C1a-pre): the map lives on
    /// `CpuGsw`, shared across backends by split borrow, but its lifetime contract is unchanged:
    /// heat drops exactly when THIS cache resets its storage. Every reset (reset_storage and the
    /// empty-cache clear fast path) bumps this; `CpuGsw::sync_smc_heat` observes the bump and
    /// clears the shared map before the next heat access. An inactive backend's cache never
    /// resets, so it can never erase the live backend's demotion evidence.
    heat_resets: u64,
    stats: BlockCacheStats,
    /// See `DirectStallTally`: never drained, never reset.
    stalls: DirectStallTally,
    #[cfg(feature = "direct-link-refusal-census")]
    direct_link_refusal_census: Option<Box<DirectLinkRefusalCensus>>,
    #[cfg(test)]
    defer_short_for_test: bool,
    #[cfg(test)]
    fast_map_enabled_for_test: bool,
}

impl Default for BlockCache {
    fn default() -> Self {
        // Executable arena pressure normally resets compiled code first. Keep a separate, much
        // larger bound for seen and rejected keys so unsupported one-shot code cannot grow the
        // metadata maps without limit during a long-running guest.
        Self::new(DEFAULT_DECODE_SLOT_COUNT)
    }
}

impl BlockCache {
    pub(crate) fn new(decode_slot_count: usize) -> Self {
        Self::with_entry_cap_and_decode_slots(DEFAULT_ENTRY_CAP, decode_slot_count)
    }

    #[cfg(test)]
    fn with_entry_cap(entry_cap: usize) -> Self {
        Self::with_entry_cap_and_decode_slots(entry_cap, DEFAULT_DECODE_SLOT_COUNT)
    }

    fn with_entry_cap_and_decode_slots(entry_cap: usize, decode_slot_count: usize) -> Self {
        assert!(
            decode_slot_count.is_power_of_two(),
            "decode slot count must be a nonzero power of two"
        );
        Self {
            entries: HashMap::default(),
            physical_keys: HashMap::default(),
            blocks: Vec::new(),
            segment_layouts: Vec::new(),
            block_imm_lanes: Vec::new(),
            lane_trial_epochs: HashMap::default(),
            lane_trial_override: None,
            block_portals: Vec::new(),
            link_cells: Vec::new(),
            link_sources: HashMap::new(),
            outbound: Vec::new(),
            global_block_upper_cache: [0; 2],
            iteration_upper_cache: Vec::new(),
            x87_pad: None,
            store_stub_pad: None,
            read_stub_pad: None,
            global_block_upper_epoch: 0,
            iteration_upper_epoch: 0,
            dynamic_next_slots: Vec::new(),
            inbound: HashMap::new(),
            waiting: HashMap::new(),
            linear_blocks: HashMap::new(),
            decode_dependencies: (0..decode_slot_count)
                .map(|_| Vec::new())
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            block_decode_slots: Vec::new(),
            decode_slot_mask: decode_slot_count - 1,
            block_link_epochs: Vec::new(),
            link_epoch: 1,
            block_active: Vec::new(),
            free_block_slots: Vec::new(),
            next_block_generation: 1,
            live_blocks: 0,
            hot: vec![None; HOT_LOOKUP_LEN].into_boxed_slice(),
            hot_generation: 1,
            arena: None,
            entry_cap,
            disabled: false,
            backend_enabled: super::host_supported(),
            auto_admit: false,
            admission_heat: DEFAULT_ADMISSION_HEAT,
            heat_resets: 0,
            stats: BlockCacheStats::default(),
            stalls: DirectStallTally::default(),
            #[cfg(feature = "direct-link-refusal-census")]
            direct_link_refusal_census: direct_link_refusal_census_default(),
            #[cfg(test)]
            defer_short_for_test: false,
            #[cfg(test)]
            fast_map_enabled_for_test: false,
        }
    }

    pub(crate) fn decode_slot_count(&self) -> usize {
        self.decode_dependencies.len()
    }

    pub(crate) fn auto_admit(&self) -> bool {
        self.auto_admit
    }

    pub(crate) fn backend_enabled(&self) -> bool {
        self.backend_enabled
    }

    pub(crate) fn execution_enabled(&self) -> bool {
        self.backend_enabled && self.auto_admit
    }

    #[cfg(test)]
    pub(crate) fn fast_map_enabled(&self) -> bool {
        self.fast_map_enabled_for_test || self.execution_enabled()
    }

    pub(crate) fn admission_heat(&self) -> u8 {
        self.admission_heat
    }

    pub(crate) fn set_backend_enabled(&mut self, on: bool) {
        self.backend_enabled = on && super::host_supported();
    }

    pub(crate) fn set_auto_admit(&mut self, on: bool) {
        self.auto_admit = on;
    }

    #[cfg(test)]
    pub(crate) fn set_admission_heat_for_test(&mut self, heat: u8) {
        self.admission_heat = heat.max(1);
    }

    #[cfg(test)]
    /// Arm the FastMap WITHOUT refreshing `CpuGsw::fast_map_serve_enabled`.
    ///
    /// The long name is the guard rail. This moves an input to `fast_map_population_enabled()`
    /// and so owes that predicate's mirror a refresh, but it cannot pay one -- the mirror lives on
    /// `CpuGsw`. Call `CpuGsw::set_fast_map_enabled_for_test`, which does both. Calling this
    /// directly leaves the mirror stale, which used to be survivable only because every consumer
    /// recomputed the predicate; one consumer now reads the mirror, and a stale one stops a JIT
    /// block from ever compiling.
    pub(crate) fn set_fast_map_enabled_for_test_without_mirror_refresh(&mut self, enabled: bool) {
        self.fast_map_enabled_for_test = enabled;
    }

    pub(crate) fn probe(&mut self, watch: &mut NativeCodeWatch, key: BlockKey) -> BlockProbe {
        if self.disabled {
            return BlockProbe::Rejected;
        }
        let hot_index = key.hot_index();
        let hot_live = |hit: &HotEntry| hit.generation == self.hot_generation && hit.key == key;
        if let Some(hit) = self.hot[hot_index].filter(hot_live) {
            self.stats.hot_hits += 1;
            return BlockProbe::Ready(hit.id);
        }
        match self.entries.get(&key).copied() {
            Some(BlockState::Compiled(id)) => {
                self.stats.hash_hits += 1;
                self.hot[hot_index] = Some(HotEntry {
                    key,
                    id,
                    generation: self.hot_generation,
                });
                BlockProbe::Ready(id)
            }
            Some(BlockState::Seen) => BlockProbe::Compile,
            Some(BlockState::Dormant(_) | BlockState::Rejected(_)) => BlockProbe::Rejected,
            None => {
                self.stats.lookup_misses += 1;
                if self.entries.len() == self.entry_cap {
                    self.reset_storage(watch);
                }
                self.entries.insert(key, BlockState::Seen);
                self.track_physical_key(key);
                BlockProbe::Interpret
            }
        }
    }

    /// Install bytes produced after `probe` returned `Compile`. Strict E2 watch edges land in
    /// `pending_watch_edges` for the caller's fast-map sweep (watched-page-bit design D4).
    pub(crate) fn install(
        &mut self,
        watch: &mut NativeCodeWatch,
        pending_watch_edges: &mut Vec<u32>,
        compilation: &Compilation,
    ) -> Option<BlockId> {
        let span = compilation.span;
        if self.disabled || self.entries.get(&span.key) != Some(&BlockState::Seen) {
            return None;
        }
        let page_len = self
            .arena
            .as_ref()
            .map_or_else(super::exec_mem::host_page_len, ExecutableArena::slot_len);
        let code_len = u32::try_from(compilation.code.len()).ok()?;
        let raw_clocks = u16::try_from(compilation.raw_clocks).ok()?;
        if compilation.code.is_empty()
            || compilation.code.len() > page_len
            || compilation.body_offset >= compilation.code.len()
        {
            return None;
        }
        let (decode_slots, decode_slot_len) = self.compilation_decode_slots(compilation)?;
        if self.arena.as_ref().is_some_and(ExecutableArena::is_full) {
            let capacity = self
                .arena
                .as_ref()
                .map_or(0, ExecutableArena::slot_capacity);
            let can_compact = Self::arena_compaction_can_reclaim(self.live_blocks, capacity);
            if !can_compact || !self.compact_arena() {
                if can_compact {
                    self.stats.arena_compaction_failures += 1;
                }
                self.reset_storage(watch);
                self.entries.insert(span.key, BlockState::Seen);
                self.track_physical_key(span.key);
            }
        }
        if self.arena.is_none() {
            self.arena = ExecutableArena::new();
        }
        let Some(entry) = self
            .arena
            .as_mut()
            .and_then(|arena| arena.install(&compilation.code))
        else {
            self.disabled = true;
            return None;
        };
        let index = self
            .free_block_slots
            .pop()
            .map(usize::from)
            .unwrap_or(self.blocks.len());
        let Some(id) = self.fresh_block_id(index) else {
            self.disabled = true;
            return None;
        };
        // The one place the call-out TOTAL is dropped in favour of the two class counts, so the
        // one place that can notice a slot belonging to neither class -- the shape a fourth
        // `CallOutHelper` takes if it is added without choosing one. `CompiledBlock` then derives
        // the total from the split, and `compute_iteration_upper` prices the split, so a classless
        // slot would be invisible to both.
        debug_assert_eq!(
            compilation.callout_port_slots + compilation.callout_memory_slots,
            compilation.callout_slots,
            "a call-out slot belongs to neither the port class nor the memory class"
        );
        let block = CompiledBlock {
            id,
            span,
            entry: entry as usize,
            body_entry: (entry as usize).checked_add(compilation.body_offset)?,
            code_len,
            fetch_lens: compilation.fetch_lens,
            raw_clocks,
            weighted_fp_clocks: compilation.weighted_fp_clocks,
            byte_reads: compilation.byte_reads,
            word_reads: compilation.word_reads,
            dword_reads: compilation.dword_reads,
            byte_stores: compilation.byte_stores,
            word_stores: compilation.word_stores,
            dword_stores: compilation.dword_stores,
            memory_cpl3: compilation.memory_cpl3,
            has_wide_accesses: compilation.has_wide_accesses,
            self_loop: compilation.self_loop,
            has_x87: compilation.has_x87,
            callout_slots: CallOutSlotCounts::new(
                compilation.callout_port_slots,
                compilation.callout_memory_slots,
            ),
            x87_entry_top: compilation.x87_entry_top,
            x87_exit_top: compilation.x87_exit_top,
            dynamic_successor: compilation.dynamic_successor,
            successors: compilation.successors,
        };
        pending_watch_edges.extend(
            watch
                .acquire_range(span.key.physical, u32::from(span.guest_len))
                .0,
        );
        if index == self.blocks.len() {
            self.blocks.push(block);
            self.segment_layouts.push(compilation.segment_layout);
            self.block_imm_lanes.push(compilation.imm_lanes);
            if index == self.block_portals.len() {
                self.block_portals.push(Arc::new(BlockPortal::new()));
            } else {
                debug_assert!(index < self.block_portals.len());
                self.block_portals[index].clear();
            }
            self.link_cells.push(compilation.link_cells.clone());
            self.outbound.push([None, None]);
            self.dynamic_next_slots.push(0);
            self.block_link_epochs.push(0);
            self.block_active.push(true);
            self.iteration_upper_cache.push(0);
            if index == self.block_decode_slots.len() {
                self.block_decode_slots.push(Vec::new());
            } else {
                debug_assert!(index < self.block_decode_slots.len());
                self.block_decode_slots[index].clear();
            }
        } else {
            debug_assert!(!self.block_active[index]);
            debug_assert!(!self.block_portals[index].visible());
            debug_assert!(self.block_decode_slots[index].is_empty());
            self.blocks[index] = block;
            self.segment_layouts[index] = compilation.segment_layout;
            self.block_imm_lanes[index] = compilation.imm_lanes;
            self.link_cells[index] = compilation.link_cells.clone();
            self.outbound[index] = [None, None];
            self.dynamic_next_slots[index] = 0;
            self.block_link_epochs[index] = 0;
            self.block_active[index] = true;
            // A recycled slot must not serve the retired occupant's cost bound to its successor.
            self.iteration_upper_cache[index] = 0;
        }
        #[cfg(feature = "direct-link-refusal-census")]
        self.register_direct_link_refusal_cells(id, compilation.emitted_static_targets);
        self.register_decode_dependencies(id, &decode_slots[..decode_slot_len]);
        if compilation.dynamic_successor {
            let cell = &compilation.link_cells[0];
            self.link_sources
                .insert(cell.address(), LinkSource { block: id, slot: 0 });
        }
        self.live_blocks += 1;
        self.entries.insert(span.key, BlockState::Compiled(id));
        self.note_page_span(span.key, u32::from(span.guest_len));
        self.hot[span.key.hot_index()] = Some(HotEntry {
            key: span.key,
            id,
            generation: self.hot_generation,
        });
        self.make_link_visible(id);
        Some(id)
    }

    #[cfg(feature = "direct-link-refusal-census")]
    fn register_direct_link_refusal_cells(
        &mut self,
        id: BlockId,
        emitted_static_targets: [Option<LinkTarget>; 2],
    ) {
        let Some(index) = self.active_index(id) else {
            return;
        };
        let block = self.blocks[index];
        let segment_write = block.is_segment_write_block();
        for (slot, target) in emitted_static_targets.into_iter().enumerate() {
            self.link_cells[index][slot].set_direct_link_refusal_census_id(0);
            let Some(target) = target else {
                continue;
            };
            let census_id = self
                .direct_link_refusal_census
                .as_mut()
                .map_or(0, |census| {
                    census.register(
                        block.span.key,
                        id.generation(),
                        slot as u8,
                        target,
                        segment_write,
                    )
                });
            self.link_cells[index][slot].set_direct_link_refusal_census_id(census_id);
        }
    }

    /// Prevent repeated compilation attempts for a block the emitter cannot handle.
    pub(crate) fn reject(
        &mut self,
        watch: &mut NativeCodeWatch,
        pending_watch_edges: &mut Vec<u32>,
        span: RejectedSpan,
    ) {
        if self.entries.get(&span.key) == Some(&BlockState::Seen) {
            pending_watch_edges.extend(
                watch
                    .acquire_range(span.key.physical, u32::from(span.guest_len))
                    .0,
            );
            self.entries.insert(span.key, BlockState::Rejected(span));
            self.note_page_span(span.key, u32::from(span.guest_len));
        }
    }

    /// Keep a non-structural failure on the interpreter until an explicit cache reset or a new
    /// mode/translation key makes another admission attempt meaningful.
    /// `reason` is counted even when the key was NOT in `Seen` and so is not re-parked: the
    /// question the counter answers is "how often did this gate fire", not "how many keys does
    /// the map now hold in each state", and a key already Dormant from an earlier attempt would
    /// otherwise vanish from the tally.
    pub(crate) fn dormant(&mut self, key: BlockKey, reason: DormantReason) {
        self.stalls.dormant[reason as usize] += 1;
        if self.entries.get(&key) == Some(&BlockState::Seen) {
            self.entries.insert(key, BlockState::Dormant(reason));
        }
    }

    /// G1 demotion: park a heat-hot key Dormant AND stamp its entry chunk at the demote epoch.
    /// The stamp is what makes the Dormant recognizably heat-scoped: once it goes stale (a later
    /// epoch), `lift_cold_smc_dormant` re-admits the key. Dormants parked for other reasons
    /// (compile Retry, G4 cover failure) carry no stamp and stay parked as before.
    /// How many storage resets this cache has performed (the hoisted heat map's reset
    /// coupling; see the `heat_resets` field).
    pub(crate) fn heat_resets(&self) -> u64 {
        self.heat_resets
    }

    /// Address of the shared x87 re-entry pad, building it on first use. `None` when the host
    /// cannot provide an executable mapping; every caller must then refuse the crossing rather
    /// than fall back to `body`, which would enter a float block with an unloaded register cache.
    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    fn x87_pad_address(&mut self) -> Option<usize> {
        if self.x87_pad.is_none() {
            self.x87_pad = super::exec_mem::ExecutableBuffer::new(&emit::emit_x87_reentry_pad());
        }
        self.x87_pad.as_ref().map(|pad| pad.entry_ptr() as usize)
    }

    #[cfg(not(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    )))]
    fn x87_pad_address(&mut self) -> Option<usize> {
        None
    }

    /// The pad address WITHOUT building it. Used where a build is impossible (no `&mut self`) and
    /// unnecessary: every visible float portal was published through `publish_x87`, which builds
    /// the pad, so if none exists then no float block is visible and none needs one.
    fn x87_pad_address_if_built(&self) -> Option<usize> {
        self.x87_pad.as_ref().map(|pad| pad.entry_ptr() as usize)
    }

    /// Publish `index`'s portal, routing an integer source through the shared pad when the block
    /// is a float one. The single place that decision is made.
    fn publish_portal(&mut self, index: usize) {
        let body = self.blocks[index].body_ptr();
        if self.blocks[index].has_x87 {
            let pad = self.x87_pad_address();
            self.block_portals[index].publish_x87(body, pad);
        } else {
            self.block_portals[index].publish(body);
        }
    }

    /// Memoised chain-quota divisor for `has_x87` as 0 or 1, valid only under `epoch`. Returns 0
    /// for unset, which cannot collide with a real value: `global_block_upper` is at least
    /// `max_core`, itself at least 1 on every bus including the trait defaults.
    pub(crate) fn global_block_upper_cached(&self, x87_index: usize, epoch: u64) -> u64 {
        if self.global_block_upper_epoch == epoch {
            self.global_block_upper_cache[x87_index]
        } else {
            0
        }
    }

    pub(crate) fn set_global_block_upper_cached(
        &mut self,
        x87_index: usize,
        epoch: u64,
        value: u64,
    ) {
        if self.global_block_upper_epoch != epoch {
            self.global_block_upper_cache = [0; 2];
            self.global_block_upper_epoch = epoch;
        }
        self.global_block_upper_cache[x87_index] = value;
    }

    /// Memoised `iteration_upper` for one block, valid only under `epoch`. Returns 0 for unset,
    /// which the caller treats as a miss and recomputes; see the field for why that sentinel needs
    /// no non-collision argument. Goes through `active_index` so a `BlockId` whose slot has since
    /// been recycled reads as a miss rather than as its successor's bound.
    pub(crate) fn iteration_upper_cached(&self, id: BlockId, epoch: u64) -> u64 {
        if self.iteration_upper_epoch != epoch {
            return 0;
        }
        self.active_index(id)
            .and_then(|index| self.iteration_upper_cache.get(index).copied())
            .unwrap_or(0)
    }

    pub(crate) fn set_iteration_upper_cached(&mut self, id: BlockId, epoch: u64, value: u64) {
        if self.iteration_upper_epoch != epoch {
            self.iteration_upper_cache.fill(0);
            self.iteration_upper_epoch = epoch;
        }
        let Some(index) = self.active_index(id) else {
            return;
        };
        if let Some(slot) = self.iteration_upper_cache.get_mut(index) {
            *slot = value;
        }
    }

    /// G1 lane trial: grant `key` its one compile-through-heat attempt for `epoch`, or refuse
    /// because the knob is off or the attempt was already spent this epoch. Granting stamps, so
    /// a caller that asks is committed to the attempt; asking twice in one epoch demotes.
    pub(crate) fn lane_trial_spend(&mut self, key: BlockKey, epoch: u32) -> bool {
        if !self.lane_trial_override.unwrap_or_else(lane_trial_enabled) {
            return false;
        }
        let granted = self.lane_trial_epochs.insert(key, epoch) != Some(epoch);
        if granted {
            self.stalls.lane_trials += 1;
        }
        granted
    }

    /// A lane trial's compilation installed under a hot span: the mechanism's success half.
    pub(crate) fn note_lane_trial_install(&mut self) {
        self.stalls.lane_trial_installs += 1;
    }

    /// Displacement lanes registered by an install — the disp share of the aggregate
    /// `smc_lane_registrations` the same install site feeds.
    pub(crate) fn note_disp_lane_registrations(&mut self, lanes: u64) {
        self.stalls.disp_lane_registrations += lanes;
    }

    /// The packed first touch screened a slot that no longer had a line by the time the
    /// interpreted arm asked for it. See the field.
    pub(crate) fn note_decode_pack_late_view_miss(&mut self) {
        self.stalls.decode_pack_late_view_miss += 1;
    }

    #[cfg(test)]
    pub(crate) fn set_lane_trial_for_test(&mut self, on: bool) {
        self.lane_trial_override = Some(on);
    }

    pub(crate) fn demote_smc_hot(&mut self, heat: &mut SmcHeatMap, key: BlockKey, epoch: u32) {
        self.dormant(key, DormantReason::SpanHot);
        let _ = heat.bump(key.physical, 1, epoch);
    }

    /// G1 recovery: a heat-demoted Dormant whose entry-chunk stamp has aged out (older epoch)
    /// returns to Seen, so the next probe walks the normal admission path (both heat gates
    /// re-check). Seen rather than a remove keeps the key tracked exactly once in `physical_keys`
    /// (the `retire_key_for_recompile` transition); the stamp is consumed, one recovery per demotion.
    pub(crate) fn lift_cold_smc_dormant(
        &mut self,
        heat: &mut SmcHeatMap,
        key: BlockKey,
        epoch: u32,
    ) {
        if matches!(self.entries.get(&key), Some(BlockState::Dormant(_)))
            && heat.take_stale_stamp(key.physical, epoch)
        {
            self.entries.insert(key, BlockState::Seen);
        }
    }

    /// Retire one descriptor-specialized block while keeping its key in the observed state. The
    /// current encounter falls back to the interpreter; the next encounter recompiles directly
    /// for the then-current segment layout instead of paying another first-seen pass.
    pub(crate) fn retire_key_for_recompile(
        &mut self,
        watch: &mut NativeCodeWatch,
        key: BlockKey,
    ) -> bool {
        let Some(BlockState::Compiled(id)) = self.entries.get(&key).copied() else {
            return false;
        };
        let hot_index = key.hot_index();
        if self.hot[hot_index].is_some_and(|hot| hot.key == key) {
            self.hot[hot_index] = None;
        }
        self.entries.insert(key, BlockState::Seen);
        self.retire_block(watch, id);
        true
    }

    pub(crate) fn clear(&mut self, watch: &mut NativeCodeWatch) {
        // Unconditionally, and above the early return below. The one event that invalidates this
        // cache is a persona change, which arrives here through `CpuGsw::set_mode`, and an empty
        // block cache does not imply an unchanged persona. Clearing it here rather than inside
        // `reset_storage` removes a reachability argument standing between a mode switch and a
        // miscompiled quota.
        self.global_block_upper_cache = [0; 2];
        self.iteration_upper_cache.fill(0);
        // CS reloads and monitor transitions can invalidate code millions of times while the
        // direct cache is unused. Avoid clearing the 65,536-entry hot table when it is already
        // empty.
        if self.entries.is_empty() && self.blocks.is_empty() && self.arena.is_none() {
            if watch.has_resident_pages() {
                watch.clear();
            }
            // A full clear still drops heat: signal the owner of the hoisted map.
            self.heat_resets = self.heat_resets.wrapping_add(1);
            self.disabled = false;
            return;
        }
        self.reset_storage(watch);
        self.disabled = false;
    }

    /// Drop translation-dependent links while retaining physical compiled code. The root dispatch
    /// must validate the block's canonical key before making it visible again.
    pub(crate) fn invalidate_translation(&mut self) {
        for (index, portal) in self.block_portals.iter().enumerate() {
            if self.block_active.get(index) == Some(&true) {
                portal.clear();
            }
        }
        let mut links = 0;
        #[cfg(feature = "direct-link-refusal-census")]
        let mut cleared_cells = Vec::new();
        for sources in self.inbound.values() {
            for source in sources {
                let index = source.block.index();
                if self.active_index(source.block) == Some(index) {
                    let slot = usize::from(source.slot);
                    #[cfg(feature = "direct-link-refusal-census")]
                    if let Some(target) = self.outbound[index][slot] {
                        cleared_cells.push((index, slot, target));
                    }
                    self.link_cells[index][slot].clear();
                    self.outbound[index][slot] = None;
                    links += 1;
                }
            }
        }
        #[cfg(feature = "direct-link-refusal-census")]
        for (index, slot, target) in cleared_cells {
            self.note_direct_link_cleared(index, slot, LinkClearCause::Flushed, target);
        }
        self.inbound.clear();
        self.waiting.clear();
        self.linear_blocks.clear();
        self.stats.unlinks += links;
        self.stalls.links_cleared[LinkClearCause::Flushed as usize] += links;
        self.link_epoch = self.link_epoch.wrapping_add(1);
        if self.link_epoch == 0 {
            self.block_link_epochs.fill(0);
            self.link_epoch = 1;
        }
    }

    /// Hide every block that depends on one displaced decode slot. Logical links stay intact and
    /// will become usable again when root dispatch republishes the matching portals.
    pub(crate) fn suspend_decode_slot(&mut self, slot: usize) -> usize {
        let Some(dependency_len) = self.decode_dependencies.get(slot).map(Vec::len) else {
            return 0;
        };
        self.stats.decode_dependencies_scanned = self
            .stats
            .decode_dependencies_scanned
            .saturating_add(dependency_len as u64);
        let mut hidden = 0;
        for offset in 0..dependency_len {
            let id = self.decode_dependencies[slot][offset];
            let Some(index) = self.active_index(id) else {
                continue;
            };
            if !self.block_portals[index].visible() {
                continue;
            }
            debug_assert_eq!(self.block_link_epochs[index], self.link_epoch);
            self.block_portals[index].clear();
            hidden += 1;
        }
        self.stats.portals_hidden = self.stats.portals_hidden.saturating_add(hidden as u64);
        hidden
    }

    pub(crate) fn is_link_visible(&self, id: BlockId) -> bool {
        self.active_index(id).is_some_and(|index| {
            self.block_link_epochs[index] == self.link_epoch && self.block_portals[index].visible()
        })
    }

    #[cfg(test)]
    pub(crate) fn hide_portal_for_test(&mut self, id: BlockId) -> bool {
        let Some(index) = self.active_index(id) else {
            return false;
        };
        if !self.block_portals[index].visible() {
            return false;
        }
        self.block_portals[index].clear();
        true
    }

    /// Remove direct-cache entries whose translated physical bytes overlap a guest write. Block
    /// IDs and executable pages stay in place until the arena's normal whole-cache reset.
    ///
    /// `lanes` is whether this caller may take the mutable-lane exemption. Only a store path that
    /// has committed the guest's own bytes passes `true`; the value-less callers (a device range,
    /// a page-walk A/D store, a string translate-time invalidation) pass `false` and get the
    /// unchanged behaviour.
    ///
    /// # Why matching a registered lane is a sound runtime classification
    ///
    /// A lane says "this block's compile-time decode found `ADD r32, imm32` whose immediate is
    /// these four bytes". The write choke has no decoder, so it must decide from the address
    /// alone, and what makes that valid is an induction over the block's life rather than a shape
    /// learned once at the site:
    ///
    /// - Base: at install the block's decode of the instruction is current by construction, and
    ///   `watch.acquire_range` covers the block's whole physical span.
    /// - Step: every guest write overlapping that span reaches this function (the watch is what
    ///   `code_write_watched` consults, and a native store into watched code side-exits before it
    ///   commits, so the interpreter replays it through here). Every such write either matches a
    ///   registered lane of THIS block exactly, or retires it below.
    /// - Therefore a block that is still alive has had no non-lane byte of its span written since
    ///   it compiled. Its opcode, ModRM, length and displacement bytes are exactly what the
    ///   compiler decoded, and only its lane bytes may differ — which is precisely what the
    ///   emitted code re-reads from RAM. So "the write is exactly this block's lane" IS the
    ///   current shape of the instruction, not a stale memory of it.
    ///
    /// The remaining ways a live block's bytes could change are all closed elsewhere: a paging
    /// remap gives a different `BlockKey` (physical is part of the key) and lanes are keyed on
    /// physical anyway; an A20 toggle or direct-map change clears the whole cache; a device write
    /// with no reportable range does too; and arena compaction moves host code without touching
    /// block indices, so lanes stay attached to their blocks.
    pub(crate) fn invalidate_physical_range(
        &mut self,
        watch: &mut NativeCodeWatch,
        physical: u32,
        width: u32,
        lanes: bool,
    ) -> RangeInvalidation {
        let mut result = RangeInvalidation::default();
        if width == 0 || self.entries.is_empty() {
            return result;
        }

        let mut invalidated = 0;
        let mut cursor = physical;
        let mut remaining = width;
        while remaining != 0 {
            let page = cursor >> BLOCK_PAGE_SHIFT;
            let page_remaining =
                (1u32 << BLOCK_PAGE_SHIFT) - (cursor & ((1u32 << BLOCK_PAGE_SHIFT) - 1));
            let step = remaining.min(page_remaining);
            if let Some(mut page_keys) = self.physical_keys.remove(&page) {
                // Only keys whose `physical` lies in
                // `[write_start - (max_span - 1), write_end)` can overlap the
                // write: a span rooted below the lower bound ends at or before
                // `write_start` (see `PageKeys`), and Seen/Dormant point keys
                // must sit inside the write itself. Binary-search that window
                // instead of walking the whole page.
                let window_low = physical.saturating_sub(page_keys.max_span.saturating_sub(1));
                let window_high = physical.saturating_add(width);
                let keys = &mut page_keys.keys;
                let window_start = keys.partition_point(|tracked| tracked.physical < window_low);
                let window_end = window_start
                    + keys[window_start..]
                        .partition_point(|tracked| tracked.physical < window_high);
                let mut survivor_count = window_start;
                result.keys_scanned = result
                    .keys_scanned
                    .saturating_add(u32::try_from(window_end - window_start).unwrap_or(u32::MAX));
                for index in window_start..window_end {
                    let key = keys[index];
                    let Some(state) = self.entries.get(&key).copied() else {
                        continue;
                    };
                    let overlaps = match state {
                        BlockState::Seen | BlockState::Dormant(_) => {
                            physical_range_contains(physical, width, key.physical)
                        }
                        BlockState::Rejected(span) => physical_ranges_overlap(
                            physical,
                            width,
                            span.key.physical,
                            u32::from(span.guest_len),
                        ),
                        BlockState::Compiled(id) => self.block(id).is_none_or(|block| {
                            physical_ranges_overlap(
                                physical,
                                width,
                                block.span.key.physical,
                                u32::from(block.span.guest_len),
                            )
                        }),
                    };
                    if !overlaps {
                        keys[survivor_count] = key;
                        survivor_count += 1;
                        continue;
                    }
                    // Fail-closed lane check, per block. Everything that is not the one admitted
                    // shape falls through to the retire below: a wrong width at a lane start, a
                    // write that overlaps lane bytes without starting on one (a straddle or a
                    // partial patch), a write that misses this block's lanes entirely, a block
                    // with no lanes at all, and every write from a caller that cannot pass
                    // `lanes`.
                    if let BlockState::Compiled(id) = state
                        && lanes
                        && let Some(index) = self.active_index(id)
                    {
                        let block_lanes = self.block_imm_lanes[index];
                        if physical != NO_IMM_LANE
                            && width == IMM_LANE_WIDTH
                            && block_lanes.contains(&physical)
                        {
                            result.lane_accepts += 1;
                            keys[survivor_count] = key;
                            survivor_count += 1;
                            continue;
                        }
                        for lane in block_lanes.iter().copied().filter(|lane| {
                            *lane != NO_IMM_LANE
                                && physical_ranges_overlap(physical, width, *lane, IMM_LANE_WIDTH)
                        }) {
                            if physical == lane {
                                result.lane_reject_width += 1;
                            } else {
                                result.lane_reject_address += 1;
                            }
                        }
                    }

                    self.entries.remove(&key);
                    let hot_index = key.hot_index();
                    if self.hot[hot_index].is_some_and(|hot| hot.key == key) {
                        self.hot[hot_index] = None;
                    }
                    match state {
                        BlockState::Rejected(span) => {
                            watch.release_range(span.key.physical, u32::from(span.guest_len));
                        }
                        BlockState::Compiled(id) => self.retire_block(watch, id),
                        BlockState::Seen | BlockState::Dormant(_) => {}
                    }
                    invalidated += 1;
                }
                // Survivors compacted into [window_start, survivor_count);
                // close the kill hole so the untouched tail keeps the sorted
                // order the window search depends on.
                if survivor_count != window_end {
                    keys.drain(survivor_count..window_end);
                }
                if !keys.is_empty() {
                    self.physical_keys.insert(page, page_keys);
                }
            }
            cursor = cursor.wrapping_add(step);
            remaining -= step;
        }
        result.blocks = invalidated;
        result
    }

    pub(crate) fn len(&self) -> usize {
        self.live_blocks
    }

    pub(crate) fn block(&self, id: BlockId) -> Option<CompiledBlock> {
        self.active_index(id)
            .and_then(|index| self.blocks.get(index).copied())
    }

    /// Why a static successor cell is still unbound, asked at the exit that hit it.
    ///
    /// Diagnostic only, and gated at the call site on the barrier census. Two successive audit
    /// hypotheses for the 20.8M static-unbound exits (x87 link refusal, then link churn) were
    /// both refuted by counters that already existed — `x87_pad_bails` and `reject_x87_top` are
    /// flat zero, and the global `invalidate_translation` does not fire in steady state. This
    /// answers the question directly instead of inferring it a third time.
    pub(crate) fn classify_unbound_target(&self, key: BlockKey) -> UnboundTarget {
        match self.entries.get(&key) {
            None => UnboundTarget::Absent,
            Some(BlockState::Seen) => UnboundTarget::Seen,
            Some(BlockState::Dormant(DormantReason::SpanHot)) => UnboundTarget::DormantHeat,
            Some(BlockState::Dormant(_)) => UnboundTarget::DormantOther,
            Some(BlockState::Rejected(_)) => UnboundTarget::Rejected,
            Some(BlockState::Compiled(id)) => {
                if self.active_index(*id).is_some() {
                    UnboundTarget::Compiled
                } else {
                    UnboundTarget::CompiledRetired
                }
            }
        }
    }

    #[cfg(feature = "direct-admission-census")]
    /// Classify the authoritative state behind a `BlockProbe::Rejected` result for the opt-in
    /// admission census. A disabled cache synthesizes that probe result regardless of entry state,
    /// so it has no Dormant or Rejected label.
    pub(crate) fn classify_rejected_probe(&self, key: BlockKey) -> Option<AdmissionDecline> {
        if self.disabled {
            return None;
        }
        match self.entries.get(&key) {
            Some(BlockState::Dormant(_)) => Some(AdmissionDecline::DormantProbe),
            Some(BlockState::Rejected(_)) => Some(AdmissionDecline::RejectedProbe),
            None | Some(BlockState::Seen | BlockState::Compiled(_)) => None,
        }
    }

    pub(crate) fn take_stats(&mut self) -> BlockCacheStats {
        std::mem::take(&mut self.stats)
    }

    #[cfg(feature = "direct-link-refusal-census")]
    pub(crate) fn direct_link_refusal_census_active(&self) -> bool {
        self.direct_link_refusal_census.is_some()
    }

    #[cfg(feature = "direct-link-refusal-census")]
    pub(crate) fn note_direct_link_refusal_exit(&mut self, id: u32) {
        if let Some(census) = self.direct_link_refusal_census.as_mut() {
            census.note_exit(id);
        }
    }

    #[cfg(feature = "direct-link-refusal-census")]
    pub(crate) fn direct_link_refusal_census_snapshot(
        &self,
    ) -> Option<crate::DirectLinkRefusalCensusSnapshot> {
        self.direct_link_refusal_census
            .as_deref()
            .map(DirectLinkRefusalCensus::snapshot)
    }

    #[cfg(feature = "direct-link-refusal-census")]
    pub(crate) fn set_direct_link_refusal_census_enabled(&mut self, enabled: bool) {
        assert!(
            self.blocks.is_empty(),
            "Direct link refusal census cannot be toggled with installed blocks"
        );
        self.direct_link_refusal_census =
            enabled.then(|| Box::new(DirectLinkRefusalCensus::default()));
    }

    #[cfg(feature = "direct-link-refusal-census")]
    fn note_direct_link_refused(
        &mut self,
        source_index: usize,
        slot: usize,
        reason: LinkRefusal,
        target: BlockId,
    ) {
        let id = self.link_cells[source_index][slot].direct_link_refusal_census_id();
        if let Some(census) = self.direct_link_refusal_census.as_mut() {
            census.refused(id, reason, target.generation());
        }
    }

    #[cfg(feature = "direct-link-refusal-census")]
    fn note_direct_link_linked(&mut self, source_index: usize, slot: usize, target: BlockId) {
        let id = self.link_cells[source_index][slot].direct_link_refusal_census_id();
        if let Some(census) = self.direct_link_refusal_census.as_mut() {
            census.linked(id, target.generation());
        }
    }

    #[cfg(feature = "direct-link-refusal-census")]
    fn note_direct_link_cleared(
        &mut self,
        source_index: usize,
        slot: usize,
        cause: LinkClearCause,
        target: BlockId,
    ) {
        let id = self.link_cells[source_index][slot].direct_link_refusal_census_id();
        if let Some(census) = self.direct_link_refusal_census.as_mut() {
            census.cleared(id, cause, target.generation());
        }
    }

    #[cfg(feature = "direct-link-refusal-census")]
    fn close_direct_link_rows(&mut self, index: usize) {
        let ids = self.link_cells[index]
            .each_ref()
            .map(|cell| cell.direct_link_refusal_census_id());
        if let Some(census) = self.direct_link_refusal_census.as_mut() {
            for id in ids {
                census.close(id);
            }
        }
    }

    /// The block's compile-time segment snapshot, which no longer rides the `CompiledBlock`
    /// copy. Returned by value: the three descriptor checks in `run_direct_block` sit between
    /// `&mut self.jit_direct` uses (`retire_key_for_recompile`), so a borrow cannot span them.
    /// One 116-byte copy per entry replaces four.
    ///
    /// Indexed WITHOUT `active_index`, deliberately. Callers reach this holding a possibly
    /// stale `CompiledBlock` copy, and back when the layout was a field of that copy the
    /// descriptor checks still ran (and still attributed their reject counters) against a
    /// retired block's own snapshot. Adding a liveness gate here would move retirement
    /// detection ahead of those counters and silently change reject attribution.
    ///
    /// Reading a reused slot's newer layout is harmless: it can only pick a different reject
    /// counter, never admit a stale block. Entry is gated separately by the generational
    /// re-resolve in `run_direct_block`, which fails for both a retired and a reused id.
    /// `None` means the whole cache was reset out from under the copy, which that re-resolve
    /// would have refused a few lines later anyway.
    pub(crate) fn segment_layout(&self, id: BlockId) -> Option<SegmentLayout> {
        self.segment_layouts.get(id.index()).copied()
    }

    // Takes the id, not the block: this reads `block.id` and nothing else, and passing
    // `CompiledBlock` by value put a full-struct copy on the entry path for one word.
    pub(crate) fn has_linked_successor(&self, id: BlockId) -> bool {
        self.active_index(id)
            .and_then(|index| self.link_cells.get(index))
            .is_some_and(|cells| cells[0].linked() || cells[1].linked())
    }

    /// Bind one observed near-RET target to a target-checked successor cell. Dynamic targets are
    /// deliberately not added to `waiting`: if the target is not link-visible yet, a later RET
    /// observation retries after normal admission has had a chance to compile it.
    pub(crate) fn bind_dynamic_successor(
        &mut self,
        site_cell: usize,
        target_eip: u32,
        target_linear: u32,
        mode_key: u32,
    ) -> bool {
        let Some(source) = self.link_sources.get(&site_cell).copied() else {
            return false;
        };
        if source.slot != 0 {
            return false;
        }
        let Some(source_index) = self.active_index(source.block) else {
            return false;
        };
        if !self.blocks[source_index].dynamic_successor {
            return false;
        }
        let target_key = LinkTarget {
            linear: target_linear,
            mode_key,
        };
        let Some(target) = self.linear_blocks.get(&target_key).copied() else {
            return false;
        };
        if let Some(slot) = self.outbound[source_index]
            .iter()
            .position(|outbound| *outbound == Some(target))
        {
            return self.try_link_inner(source.block, slot as u8, target, Some(target_eip));
        }
        let slot = self.outbound[source_index]
            .iter()
            .position(Option::is_none)
            .unwrap_or_else(|| usize::from(self.dynamic_next_slots[source_index] & 1));
        self.dynamic_next_slots[source_index] = ((slot + 1) & 1) as u8;
        self.try_link_inner(source.block, slot as u8, target, Some(target_eip))
    }

    pub(crate) fn defer_short_enabled(&self) -> bool {
        #[cfg(test)]
        {
            self.defer_short_for_test
        }
        #[cfg(not(test))]
        {
            false
        }
    }

    #[cfg(test)]
    pub(crate) fn set_defer_short_for_test(&mut self, enabled: bool) {
        self.defer_short_for_test = enabled;
    }

    /// Total emitted bytes across live blocks, for tests that assert an
    /// emission-arm size difference (the R15 table-bases A/B).
    #[cfg(test)]
    pub(crate) fn total_live_code_len_for_test(&self) -> u64 {
        self.blocks
            .iter()
            .enumerate()
            .filter(|(index, _)| self.block_active[*index])
            .map(|(_, block)| u64::from(block.code_len))
            .sum()
    }

    pub(crate) fn block_for_trace(
        &self,
        linear: u32,
        physical: u32,
        mode_key: u32,
    ) -> Option<CompiledBlock> {
        let key = BlockKey::new(linear, physical, mode_key);
        if let Some(hit) = self.hot[key.hot_index()]
            && hit.generation == self.hot_generation
            && hit.key == key
        {
            return self.block(hit.id);
        }
        let BlockState::Compiled(id) = self.entries.get(&key).copied()? else {
            return None;
        };
        self.block(id)
    }

    /// Republish a block after root dispatch has revalidated its canonical translation key.
    pub(crate) fn revalidate_translation(&mut self, key: BlockKey) -> Option<CompiledBlock> {
        let BlockState::Compiled(id) = self.entries.get(&key).copied()? else {
            return None;
        };
        let index = self.active_index(id)?;
        self.make_link_visible(id);
        self.blocks.get(index).copied()
    }

    #[cfg(test)]
    pub(crate) fn tracked_len(&self) -> usize {
        self.entries.len()
    }

    fn active_index(&self, id: BlockId) -> Option<usize> {
        let index = id.index();
        (self.block_active.get(index) == Some(&true)
            && self.blocks.get(index).is_some_and(|block| block.id == id))
        .then_some(index)
    }

    fn compilation_decode_slots(
        &self,
        compilation: &Compilation,
    ) -> Option<([u32; MAX_BLOCK_INSTRUCTIONS], usize)> {
        let mut slots = [0u32; MAX_BLOCK_INSTRUCTIONS];
        let mut slot_len = 0;
        let mut linear = compilation.span.key.linear;
        for &fetch_len in compilation
            .fetch_lens
            .iter()
            .take(usize::from(compilation.span.instructions))
        {
            if fetch_len == 0 {
                return None;
            }
            let slot = u32::try_from((linear as usize) & self.decode_slot_mask).ok()?;
            if !slots[..slot_len].contains(&slot) {
                slots[slot_len] = slot;
                slot_len += 1;
            }
            linear = linear.checked_add(u32::from(fetch_len))?;
        }
        (linear.wrapping_sub(compilation.span.key.linear) == u32::from(compilation.span.guest_len))
            .then_some((slots, slot_len))
    }

    fn register_decode_dependencies(&mut self, id: BlockId, slots: &[u32]) {
        let index = self
            .active_index(id)
            .expect("decode dependencies require an active block");
        debug_assert!(self.block_decode_slots[index].is_empty());
        for &slot in slots {
            let slot_index = slot as usize;
            self.decode_dependencies[slot_index].push(id);
            self.block_decode_slots[index].push(slot);
        }
    }

    fn unregister_decode_dependencies(&mut self, id: BlockId, index: usize) {
        for slot in self.block_decode_slots[index].drain(..) {
            self.decode_dependencies[slot as usize].retain(|candidate| *candidate != id);
        }
    }

    fn fresh_block_id(&mut self, index: usize) -> Option<BlockId> {
        let index = u16::try_from(index).ok()?;
        let id = BlockId::new(index, self.next_block_generation)?;
        self.next_block_generation += 1;
        Some(id)
    }

    fn arena_compaction_can_reclaim(live_blocks: usize, slot_capacity: usize) -> bool {
        live_blocks != 0 && live_blocks < slot_capacity
    }

    /// Copy only active native blocks into a new arena. No address is published and no old link
    /// cell is changed until every byte range has been validated and the new prefix is executable.
    fn compact_arena(&mut self) -> bool {
        // One `Instant` pair per COMPACTION, not per block or per install: duke3d-486's worst
        // measured run takes 1,205 of these in 158 s, so the clock read is unmeasurable against
        // the ~7.4 ms body it times. Started before the early bails so a refused compaction is
        // charged nothing (it does no work); the accumulate sits at the success tail.
        let started = std::time::Instant::now();
        let Some(old_arena) = self.arena.as_ref() else {
            return false;
        };
        if self.live_blocks == 0 || self.live_blocks >= old_arena.slot_capacity() {
            return false;
        }
        let Some(mut fresh_arena) = ExecutableArena::new() else {
            return false;
        };

        let mut pending = Vec::with_capacity(self.live_blocks);
        let mut moved_bytes = 0u64;
        for (index, block) in self.blocks.iter().copied().enumerate() {
            if !self.block_active[index] {
                continue;
            }
            if self.active_index(block.id) != Some(index) {
                return false;
            }
            let code_len = block.code_len as usize;
            let Some(body_offset) = block.body_entry.checked_sub(block.entry) else {
                return false;
            };
            if body_offset >= code_len {
                return false;
            }
            let Some(code) = old_arena.sealed_slot_bytes(block.entry_ptr(), code_len) else {
                return false;
            };
            let Some(slot) = fresh_arena.append_unsealed(code) else {
                return false;
            };
            pending.push((index, slot, body_offset));
            moved_bytes = moved_bytes.saturating_add(u64::from(block.code_len));
        }
        if pending.len() != self.live_blocks || !fresh_arena.seal_used_prefix() {
            return false;
        }

        let mut relocated = Vec::with_capacity(pending.len());
        for (index, slot, body_offset) in pending {
            let Some(entry) = fresh_arena
                .sealed_slot_entry(slot)
                .map(|entry| entry as usize)
            else {
                return false;
            };
            let Some(body_entry) = entry.checked_add(body_offset) else {
                return false;
            };
            relocated.push((index, entry, body_entry));
        }
        for (source_index, targets) in self.outbound.iter().enumerate() {
            if !self.block_active[source_index] {
                continue;
            }
            if targets
                .iter()
                .flatten()
                .any(|target| self.active_index(*target).is_none())
            {
                return false;
            }
        }

        let portal_visibility: Vec<_> = self
            .block_portals
            .iter()
            .enumerate()
            .map(|(index, portal)| self.block_active.get(index) == Some(&true) && portal.visible())
            .collect();
        for (index, portal) in self.block_portals.iter().enumerate() {
            if self.block_active.get(index) == Some(&true) {
                portal.clear();
            }
        }
        for block in self
            .blocks
            .iter_mut()
            .zip(&self.block_active)
            .filter_map(|(block, active)| (!active).then_some(block))
        {
            block.entry = 0;
            block.body_entry = 0;
        }
        for (index, entry, body_entry) in relocated {
            self.blocks[index].entry = entry;
            self.blocks[index].body_entry = body_entry;
        }
        // The pad lives outside the arena, so compaction never moves it and this republish only
        // has to restore the relocated `body`. Read without building: a visible float portal
        // implies the pad already exists.
        let pad = self.x87_pad_address_if_built();
        for (index, portal) in self.block_portals.iter().enumerate() {
            if portal_visibility.get(index) == Some(&true) {
                let body = self.blocks[index].body_ptr();
                if self.blocks[index].has_x87 {
                    portal.publish_x87(body, pad);
                } else {
                    portal.publish(body);
                }
            }
        }

        self.arena = Some(fresh_arena);
        self.stats.arena_compactions += 1;
        self.stats.arena_compaction_ns = self
            .stats
            .arena_compaction_ns
            .saturating_add(started.elapsed().as_nanos() as u64);
        self.stats.arena_compaction_live_blocks = self
            .stats
            .arena_compaction_live_blocks
            .saturating_add(self.live_blocks as u64);
        self.stats.arena_compaction_bytes = self
            .stats
            .arena_compaction_bytes
            .saturating_add(moved_bytes);
        true
    }

    fn reset_storage(&mut self, watch: &mut NativeCodeWatch) {
        let links = self
            .outbound
            .iter()
            .flatten()
            .filter(|target| target.is_some())
            .count() as u64;
        for portal in &self.block_portals {
            portal.clear();
        }
        #[cfg(not(feature = "direct-link-refusal-census"))]
        for cells in &self.link_cells {
            cells[0].clear();
            cells[1].clear();
        }
        #[cfg(feature = "direct-link-refusal-census")]
        for index in 0..self.link_cells.len() {
            for slot in 0..2 {
                let target = self.outbound[index][slot].take();
                self.link_cells[index][slot].clear();
                if let Some(target) = target {
                    self.note_direct_link_cleared(index, slot, LinkClearCause::Reset, target);
                }
            }
        }
        #[cfg(feature = "direct-link-refusal-census")]
        for index in 0..self.link_cells.len() {
            self.close_direct_link_rows(index);
        }
        self.stats.unlinks += links;
        self.stalls.links_cleared[LinkClearCause::Reset as usize] += links;
        self.stats.cache_resets += 1;
        self.entries.clear();
        self.physical_keys.clear();
        self.blocks.clear();
        self.segment_layouts.clear();
        self.block_imm_lanes.clear();
        self.lane_trial_epochs.clear();
        self.link_cells.clear();
        self.link_sources.clear();
        self.outbound.clear();
        self.dynamic_next_slots.clear();
        self.inbound.clear();
        self.waiting.clear();
        self.linear_blocks.clear();
        for dependencies in &mut self.decode_dependencies {
            dependencies.clear();
        }
        for slots in &mut self.block_decode_slots {
            slots.clear();
        }
        self.block_link_epochs.clear();
        self.iteration_upper_cache.clear();
        watch.clear();
        // Every storage reset drops heat; the owner of the hoisted map observes this counter.
        self.heat_resets = self.heat_resets.wrapping_add(1);
        self.block_active.clear();
        self.free_block_slots.clear();
        self.live_blocks = 0;
        self.hot_generation = self.hot_generation.wrapping_add(1);
        if self.hot_generation == 0 {
            self.hot.fill(None);
            self.hot_generation = 1;
        }
        self.arena = None;
    }

    fn resolve_successors(&mut self, source: BlockId) {
        let Some(source_index) = self.active_index(source) else {
            return;
        };
        let block = self.blocks[source_index];
        for (slot, successor) in block.successors.into_iter().enumerate() {
            let Some(successor) = successor else {
                continue;
            };
            if let Some(target) = self.linear_blocks.get(&successor).copied()
                && self.try_link(source, slot as u8, target)
            {
                continue;
            }
            self.waiting.entry(successor).or_default().push(LinkSource {
                block: source,
                slot: slot as u8,
            });
        }
    }

    fn resolve_waiting(&mut self, key: LinkTarget, target: BlockId) {
        let Some(waiting) = self.waiting.remove(&key) else {
            return;
        };
        let mut unresolved = Vec::new();
        for source in waiting {
            if !self.try_link(source.block, source.slot, target)
                && self.active_index(source.block).is_some()
            {
                unresolved.push(source);
            }
        }
        if !unresolved.is_empty() {
            self.waiting.insert(key, unresolved);
        }
    }

    fn try_link(&mut self, source: BlockId, slot: u8, target: BlockId) -> bool {
        self.try_link_inner(source, slot, target, None)
    }

    fn try_link_inner(
        &mut self,
        source: BlockId,
        slot: u8,
        target: BlockId,
        target_eip: Option<u32>,
    ) -> bool {
        let Some(source_index) = self.active_index(source) else {
            self.stalls.link_refusals[LinkRefusal::Inactive as usize] += 1;
            return false;
        };
        let slot_index = usize::from(slot);
        let Some(target_index) = self.active_index(target) else {
            self.stalls.link_refusals[LinkRefusal::Inactive as usize] += 1;
            #[cfg(feature = "direct-link-refusal-census")]
            self.note_direct_link_refused(source_index, slot_index, LinkRefusal::Inactive, target);
            return false;
        };
        let source_block = self.blocks[source_index];
        let target_block = self.blocks[target_index];
        // Split out of the single `||` chain the six conditions used to share, so each refusal
        // names itself. The ORDER is the original chain's order and the short-circuit is
        // preserved, which matters: a stale epoch must be reported before the layout compare,
        // because a stale index's `segment_layouts` entry is not meaningful.
        let refusal = if self.block_link_epochs.get(source_index).copied() != Some(self.link_epoch)
            || self.block_link_epochs.get(target_index).copied() != Some(self.link_epoch)
        {
            Some(LinkRefusal::StaleEpoch)
        } else if !self.segment_layouts[source_index]
            .link_compatible(&self.segment_layouts[target_index])
        {
            Some(LinkRefusal::SegmentLayout)
        } else if !source_block.link_compatible(&target_block) {
            Some(LinkRefusal::BlockShape)
        }
        // The dynamic RET PIC path used to layer a strict `has_x87` equality on top of the relaxed
        // rule, in both directions, because it resolves its target at runtime from an arbitrary
        // return address rather than from a compile-time successor shape. Both halves are gone:
        // `emit_completed_dynamic_path` now emits the boundary spill for a float source and
        // selects `integer_entry` for an integer one, which is the whole of what the static path
        // does. `target_eip` no longer changes which edges link, only how the cell is written.
        //
        // An integer source reaching a float target goes through the shared pad. Without one there
        // is no correct address to publish: `body` would enter the target with an unloaded x87
        // register cache. Refusing here leaves the cell on the zero portal, so the exit reports
        // `StaticUnbound` exactly as it did before the pad existed.
        else if !source_block.has_x87 && target_block.has_x87 && self.x87_pad_address().is_none()
        {
            Some(LinkRefusal::MissingX87Pad)
        } else {
            None
        };
        if let Some(refusal) = refusal {
            self.stalls.link_refusals[refusal as usize] += 1;
            #[cfg(feature = "direct-link-refusal-census")]
            self.note_direct_link_refused(source_index, slot_index, refusal, target);
            return false;
        }
        if self.outbound[source_index][slot_index] == Some(target) {
            if let Some(target_eip) = target_eip {
                self.link_cells[source_index][slot_index]
                    .set_dynamic(target_eip, self.block_portals[target_index].as_ref());
            }
            #[cfg(feature = "direct-link-refusal-census")]
            self.note_direct_link_linked(source_index, slot_index, target);
            return true;
        }
        self.unlink_outbound(source, slot, LinkClearCause::Replaced);
        // AFTER `unlink_outbound`, which routes through `LinkCell::clear` and resets this to the
        // never-set sentinel. Setting it earlier leaves every cell at `NO_ENTRY_TOP`, the shared
        // x87 pad then bails on every crossing, and the mechanism is inert while every counter
        // gate still passes. Placed beside `mark_spilling`, which has the same ordering
        // requirement for the same reason.
        if let Some(top) = target_block.x87_entry_top() {
            self.link_cells[source_index][slot_index].set_entry_top(top);
        }
        if source_block.has_x87 && !target_block.has_x87 {
            self.link_cells[source_index][slot_index].mark_spilling();
        }
        if let Some(target_eip) = target_eip {
            self.link_cells[source_index][slot_index]
                .set_dynamic(target_eip, self.block_portals[target_index].as_ref());
        } else {
            self.link_cells[source_index][slot_index]
                .set(self.block_portals[target_index].as_ref());
        }
        self.outbound[source_index][slot_index] = Some(target);
        self.inbound.entry(target).or_default().push(LinkSource {
            block: source,
            slot,
        });
        self.stats.links += 1;
        #[cfg(feature = "direct-link-refusal-census")]
        self.note_direct_link_linked(source_index, slot_index, target);
        true
    }

    /// `cause` is passed by the caller rather than inferred: the same helper serves the
    /// relink-replace path in `try_link_inner` and the retirement walk in `unlink_block`, and
    /// nothing inside the helper can tell those apart.
    fn unlink_outbound(&mut self, source: BlockId, slot: u8, cause: LinkClearCause) {
        let Some(source_index) = self.active_index(source) else {
            return;
        };
        let slot_index = usize::from(slot);
        let Some(target) = self.outbound[source_index][slot_index].take() else {
            self.link_cells[source_index][slot_index].clear();
            return;
        };
        self.link_cells[source_index][slot_index].clear();
        if let Some(inbound) = self.inbound.get_mut(&target) {
            inbound.retain(|link| !(link.block == source && link.slot == slot));
            if inbound.is_empty() {
                self.inbound.remove(&target);
            }
        }
        self.stats.unlinks += 1;
        self.stalls.links_cleared[cause as usize] += 1;
        #[cfg(feature = "direct-link-refusal-census")]
        self.note_direct_link_cleared(source_index, slot_index, cause, target);
    }

    fn unlink_block(&mut self, id: BlockId) {
        let Some(index) = self.active_index(id) else {
            return;
        };
        self.remove_waiting_sources(id);
        let target_key = LinkTarget {
            linear: self.blocks[index].span.key.linear,
            mode_key: self.blocks[index].span.key.mode_key,
        };
        if self.linear_blocks.get(&target_key) == Some(&id) {
            self.linear_blocks.remove(&target_key);
        }
        if let Some(inbound) = self.inbound.remove(&id) {
            for link in inbound {
                let source_index = link.block.index();
                if self.active_index(link.block) == Some(source_index) {
                    let slot = usize::from(link.slot);
                    self.link_cells[source_index][slot].clear();
                    self.outbound[source_index][slot] = None;
                    if let Some(successor) = self.blocks[source_index].successors[slot] {
                        self.waiting.entry(successor).or_default().push(link);
                    }
                    self.stats.unlinks += 1;
                    self.stalls.links_cleared[LinkClearCause::Retired as usize] += 1;
                    #[cfg(feature = "direct-link-refusal-census")]
                    self.note_direct_link_cleared(source_index, slot, LinkClearCause::Retired, id);
                }
            }
        }
        for slot in 0..2 {
            self.unlink_outbound(id, slot, LinkClearCause::Retired);
        }
        self.remove_waiting_sources(id);
    }

    fn remove_waiting_sources(&mut self, id: BlockId) {
        self.waiting.retain(|_, sources| {
            sources.retain(|source| source.block != id);
            !sources.is_empty()
        });
    }

    fn retire_block(&mut self, watch: &mut NativeCodeWatch, id: BlockId) {
        let Some(index) = self.active_index(id) else {
            return;
        };
        let span = self.blocks[index].span;
        self.block_portals[index].clear();
        self.block_link_epochs[index] = 0;
        self.unregister_decode_dependencies(id, index);
        if self.blocks[index].dynamic_successor {
            self.link_sources
                .remove(&self.link_cells[index][0].address());
        }
        self.unlink_block(id);
        #[cfg(feature = "direct-link-refusal-census")]
        self.close_direct_link_rows(index);
        self.block_active[index] = false;
        self.blocks[index].entry = 0;
        self.blocks[index].body_entry = 0;
        self.block_imm_lanes[index] = [NO_IMM_LANE; MAX_BLOCK_IMM_LANES];
        self.free_block_slots
            .push(u16::try_from(index).expect("block slot index must fit its ID"));
        self.live_blocks -= 1;
        watch.release_range(span.key.physical, u32::from(span.guest_len));
    }

    fn track_physical_key(&mut self, key: BlockKey) {
        let page = self
            .physical_keys
            .entry(key.physical >> BLOCK_PAGE_SHIFT)
            .or_default();
        // Sorted insert (ties on `physical` may exist across mode/linear keys;
        // their relative order is irrelevant). Insertion is compile/track-time,
        // orders of magnitude rarer than the store-side window scan this order
        // exists for.
        let at = page
            .keys
            .partition_point(|tracked| tracked.physical <= key.physical);
        page.keys.insert(at, key);
    }

    /// Record that `key`'s page roots a span of `guest_len` bytes, widening the
    /// page's invalidation window bound. Called when a key becomes
    /// Compiled/Rejected; `track_physical_key` has always run first.
    fn note_page_span(&mut self, key: BlockKey, guest_len: u32) {
        if let Some(page) = self
            .physical_keys
            .get_mut(&(key.physical >> BLOCK_PAGE_SHIFT))
        {
            page.max_span = page.max_span.max(guest_len);
        }
    }

    fn make_link_visible(&mut self, id: BlockId) {
        let Some(index) = self.active_index(id) else {
            return;
        };
        if self.block_link_epochs.get(index).copied() == Some(self.link_epoch) {
            if !self.block_portals[index].visible() {
                self.publish_portal(index);
            }
            return;
        }
        self.block_portals[index].clear();
        self.block_link_epochs[index] = self.link_epoch;
        let span = self.blocks[index].span;
        let target = LinkTarget {
            linear: span.key.linear,
            mode_key: span.key.mode_key,
        };
        self.linear_blocks.insert(target, id);
        self.resolve_successors(id);
        self.resolve_waiting(target, id);
        self.publish_portal(index);
    }
}

fn physical_range_contains(start: u32, width: u32, address: u32) -> bool {
    width != 0 && address.wrapping_sub(start) < width
}

fn physical_ranges_overlap(a: u32, a_width: u32, b: u32, b_width: u32) -> bool {
    physical_range_contains(a, a_width, b) || physical_range_contains(b, b_width, a)
}

impl PartialEq for BlockCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for BlockCache {}

impl Drop for BlockCache {
    fn drop(&mut self) {
        for portal in &self.block_portals {
            portal.clear();
        }
        for cells in &self.link_cells {
            cells[0].clear();
            cells[1].clear();
        }
    }
}

impl Clone for BlockCache {
    fn clone(&self) -> Self {
        let mut cache = Self::new(self.decode_slot_count());
        #[cfg(feature = "direct-link-refusal-census")]
        {
            cache.direct_link_refusal_census = None;
        }
        cache.backend_enabled = self.backend_enabled;
        cache.admission_heat = self.admission_heat;
        cache.entry_cap = self.entry_cap;
        #[cfg(test)]
        {
            cache.defer_short_for_test = self.defer_short_for_test;
            cache.fast_map_enabled_for_test = self.fast_map_enabled_for_test;
        }
        cache
    }
}

impl std::fmt::Debug for BlockCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BlockCache {{ {} blocks }}", self.len())
    }
}

// Compilation already owns emitted buffers and link cells. Boxing it would add an allocation to
// every successful compile only to keep the two failure variants small.
#[allow(clippy::large_enum_variant)]
pub(crate) enum CompileOutcome {
    Compiled(Compilation),
    StructuralReject(RejectedSpan),
    Retry,
}

#[cfg(test)]
impl CompileOutcome {
    pub(crate) fn expect(self, message: &str) -> Compilation {
        match self {
            Self::Compiled(compilation) => compilation,
            Self::StructuralReject(_) | Self::Retry => panic!("{message}"),
        }
    }

    pub(crate) fn unwrap(self) -> Compilation {
        self.expect("called CompileOutcome::unwrap() on a non-compiled outcome")
    }

    pub(crate) fn unwrap_or_else(self, fallback: impl FnOnce() -> Compilation) -> Compilation {
        match self {
            Self::Compiled(compilation) => compilation,
            Self::StructuralReject(_) | Self::Retry => fallback(),
        }
    }

    pub(crate) fn is_some(&self) -> bool {
        matches!(self, Self::Compiled(_))
    }

    pub(crate) fn is_none(&self) -> bool {
        !self.is_some()
    }
}

pub(crate) struct Compilation {
    pub span: BlockSpan,
    pub fetch_lens: [u8; MAX_BLOCK_INSTRUCTIONS],
    pub raw_clocks: u32,
    pub weighted_fp_clocks: u32,
    pub byte_reads: u8,
    pub word_reads: u8,
    pub dword_reads: u8,
    pub byte_stores: u8,
    pub word_stores: u8,
    pub dword_stores: u8,
    segment_layout: SegmentLayout,
    pub memory_cpl3: bool,
    pub has_wide_accesses: bool,
    pub self_loop: bool,
    pub has_x87: bool,
    /// How many interpreter call-out slots this block carries, of any class. Bounded by
    /// `MAX_BLOCK_CALLOUT_SLOTS`; the two class splits below are what carries the runtime charge
    /// into `compute_iteration_upper`. See the derivation there.
    pub callout_slots: u8,
    /// The port-class subset (`0xEC`), which the dispatch privilege gate keys on.
    pub callout_port_slots: u8,
    /// The memory-class subset (`0x60`, `0x61`), each of which moves
    /// `CALL_OUT_STACK_FRAME_DWORDS` dwords of guest stack.
    pub callout_memory_slots: u8,
    pub x87_entry_top: u8,
    pub x87_exit_top: u8,
    /// Readable outside this module for the same reason as `successors` below: a terminal that
    /// omits it stays correct in guest state and in block shape while never linking, which no
    /// other assertion can see.
    pub(crate) dynamic_successor: bool,
    /// Readable outside this module so a fixture can pin a terminal's LINK EDGE. A kind missing
    /// from the successor match falls to the fall-through arm, which is a wrong edge rather than
    /// a missing one, and nothing observable in guest state or in the block's shape shows it.
    pub(crate) successors: [Option<LinkTarget>; 2],
    #[cfg(feature = "direct-link-refusal-census")]
    pub(crate) emitted_static_targets: [Option<LinkTarget>; 2],
    link_cells: [Arc<LinkCell>; 2],
    body_offset: usize,
    /// Physical start of each mutable immediate this block's emitted code reads from guest RAM,
    /// `NO_IMM_LANE` for an unused slot. `install` copies these into the cache's per-block lane
    /// array, which is what the SMC write choke matches a patch against.
    imm_lanes: [u32; MAX_BLOCK_IMM_LANES],
    /// How many of `imm_lanes` are DISPLACEMENT lanes (`disp_lane_for`). The write choke never
    /// needs the distinction — a lane is a lane there — but the install site does: the split
    /// between `smc_lane_registrations` and `disp_lane_registrations` is what says which lane
    /// kind an A/B's `smc_lane_accepts` movement belongs to.
    disp_lanes: u8,
    pub code: Vec<u8>,
}

impl Compilation {
    pub(crate) fn imm_lane_count(&self) -> usize {
        self.imm_lanes
            .iter()
            .filter(|lane| **lane != NO_IMM_LANE)
            .count()
    }

    pub(crate) fn disp_lane_count(&self) -> usize {
        usize::from(self.disp_lanes)
    }
}

/// One mutable immediate field: where its four bytes live in guest physical memory, and the host
/// address of those same bytes for the emitted load.
///
/// `physical` is what the SMC write choke matches against, and keying on PHYSICAL rather than
/// linear is what makes linear aliases of the same code free: two linear addresses mapping to one
/// physical page produce one lane address, and a patch through either alias resolves to it.
///
/// `host` is a raw pointer to the same bytes, resolved at compile time from a direct page. It
/// stays valid for the block's whole life: RAM host pointers never move for a given physical page
/// (see `note_direct_data_map_changed`), and every event that could change which host bytes back a
/// physical address — an A20 toggle, a direct-map change, a bus mapping-epoch change that is not
/// data-only — routes through `invalidate_code_caches`, which clears this cache.
#[derive(Clone, Copy)]
pub(crate) struct ImmLane {
    pub(crate) physical: u32,
    pub(crate) host: usize,
}

#[derive(Clone, Copy)]
struct DirectInsn {
    lin: u32,
    len: u8,
    weighted_fp_clocks: u32,
    kind: DirectKind,
}

#[derive(Clone, Copy)]
pub(crate) enum DirectKind {
    MovReg {
        dst: u8,
        src: u8,
        width: MemoryWidth,
    },
    MovRegByte {
        dst: u8,
        src: u8,
    },
    /// `MOV r, imm` with a register destination, both operand sizes. The `width` is the
    /// operand size and it decides how much of the destination the write DEFINES: a Word
    /// form writes bits 15..0 and leaves 31..16 exactly as the interpreter's
    /// `write_gpr_sized(.., Word, ..)` leaves them. Without the field the Word form lowers
    /// as a 32-bit move, which zeroes the upper half rather than preserving it -- `decode`
    /// zero-extends the immediate, so the wrong bits are zeros rather than garbage, which
    /// makes the miscompile quiet on any guest whose upper half happened to be clear.
    MovImm {
        dst: u8,
        imm: u32,
        width: MemoryWidth,
    },
    /// `MOV r16, Sreg` (0x8C, register destination). The selector is baked as a compile-time
    /// constant, which is sound because the block's `SegmentLayout` pins the whole descriptor:
    /// `run_direct_block` rejects any entry whose live copy differs (`cs_matches` for CS,
    /// `data_matches` for the other five) and `SegmentLayout::link_compatible` requires equal
    /// snapshots on both ends of every link, so no chained path reaches this slot under a
    /// different selector.
    ///
    /// For the five DATA segments that pinning is not automatic. `data_matches` SKIPS any
    /// segment outside the block's `used` mask, and that mask is derived from actual memory
    /// accesses — which a `0x8C` slot does not imply. `DirectKind::selector_segment` is what puts
    /// the segment in the mask; without it a block whose only mention of DS is `mov ax, ds` would
    /// bake one selector and then be re-entered under another. Register destination only: a
    /// memory destination is a word store and belongs in `Store` if it ever ranks.
    MovSegToReg {
        dst: u8,
        segment: SegmentIndex,
    },
    /// `MOV Sreg, r16` (0x8E, register source) in REAL MODE or V86, where a segment load is
    /// `base = selector << 4` with no descriptor fetch and no fault path. DS and ES only.
    ///
    /// Emitted inline rather than as a call-out, and the call-out was the first design. Two things
    /// ruled it out: the call-out ABI is `(cpu, prefix_raw_clocks, weighted_fp_clocks)` with all
    /// three arguments taken, so there is no channel to name the segment and the source register,
    /// and `emit_call_out` spills and reloads all eight guest homes around the call, which is
    /// roughly 27 host instructions against this form's eight for an instruction the target
    /// workload runs 17.8 million times.
    ///
    /// Going inline costs one protection the call-out gave for free, and it is a miscompile rather
    /// than a missed lowering: `self_loop` is gated on `callout_slots == 0`, so a call-out slot
    /// disqualified the block from the self-loop shape automatically. A self-loop re-enters the
    /// body natively without the prologue, so a slot BEFORE this write re-executes AFTER it
    /// against a baked base the write invalidated. The compile loop bars the shape explicitly.
    LoadSegReal {
        segment: SegmentIndex,
        src: u8,
    },
    /// `SETcc r8` (0F 90..9F, register destination). The guest condition encoding is x86's own,
    /// so the emitted `setcc` takes it unchanged; `condition()` in the interpreter is the same
    /// truth table the host flags implement. Register form only: a memory destination is a byte
    /// store and has no census row.
    SetCc {
        condition: u8,
        dst: u8,
    },
    /// CBW/CWDE (0x98): widen the accumulator's sign into the next width. `width` is the
    /// interpreter's `operand_size` (the DESTINATION width): Word writes AX from AL (CBW),
    /// Dword writes EAX from AX (CWDE). No flags touched. Accumulator-implicit, so the kind
    /// carries no register index.
    Cwde {
        width: MemoryWidth,
    },
    /// CWD/CDQ (0x99): fill the upper half with the accumulator's sign. `width` is the
    /// accumulator's own width: Word fills DX from AX (CWD), Dword fills EDX from EAX (CDQ). No
    /// flags touched.
    Cdq {
        width: MemoryWidth,
    },
    MovImmByte {
        dst: u8,
        imm: u8,
    },
    Lea {
        dst: u8,
        addr: DirectAddr,
    },
    IncDecReg {
        dst: u8,
        is_dec: bool,
        width: MemoryWidth,
    },
    AluReg {
        op: u8,
        dst: u8,
        src: u8,
        width: MemoryWidth,
    },
    AluImm {
        op: u8,
        dst: u8,
        imm: u32,
        /// Present only for the shapes `imm_lane_for` admits (the reg-destination `0x81 /r`
        /// dword family, no prefixes). When present the emitted form IGNORES `imm` and loads
        /// the four immediate bytes out of guest RAM on every execution, so a guest patch of
        /// those bytes needs no recompile. `imm` still carries the value decoded at compile time
        /// and is what the non-lane form bakes.
        lane: Option<ImmLane>,
        /// The OPERAND width, and it is load-bearing rather than descriptive: at `Word` the
        /// operation runs on the low sixteen bits, sets flags at sixteen bits, and merges its
        /// result into the destination's low half instead of replacing all thirty-two. Before this
        /// field existed the kind hard-coded Dword and `0x83` was kept out of `classify`'s
        /// OperandSize::Word allowlist for exactly that reason; the field is what let it in.
        ///
        /// Only `Byte` is impossible here (`OperandSize` has two variants and the byte group is
        /// `AluByteImm`), and a lane is admitted at `Dword` alone -- `IMM_LANE_WIDTH` is four.
        width: MemoryWidth,
    },
    AluByteImm {
        op: u8,
        dst: u8,
        imm: u8,
    },
    /// The BYTE-LANE register ALU: `op r8, r8` for the whole eight-operation set, both operand
    /// orders (0x00/0x08/../0x38 with a register r/m, and 0x02/0x0A/../0x3A likewise).
    ///
    /// `dst` and `src` are BYTE-register indices, where 4..=7 are AH/CH/DH/BH — the high byte of
    /// the first four 32-bit registers. That is the whole reason this cannot be `AluReg` with
    /// `width: MemoryWidth::Byte`: `AluReg`'s emitter reaches its operands through `home(index)`,
    /// which maps index 5 to the host register holding guest EBP, so a `cmp al, ch` lowered
    /// through it would compare against the wrong register at the wrong width. x86-64 cannot name
    /// AH/CH/DH/BH in an instruction that also carries a REX prefix, so the lane is reached by
    /// shift-and-mask on the way in (`emit_read_store_value` at Byte) and by mask-shift-or on the
    /// way out (`emit_write_gpr8`) — machinery `AluByteImm`, `TestByte` and the byte INC/DEC form
    /// already share.
    ///
    /// A separate variant rather than a `width` field on `AluReg` for that reason and for the
    /// `AluByteImm`/`AluImm` precedent: the byte forms are a different lane, not a narrower one,
    /// and every accessor on this kind (`byte_reads`, `read_segment`, `uses_stack`, ...) wants the
    /// register-only default. `raw_clocks` wants the `_ => 2` default too, and correctly — the
    /// interpreter's `execute_alu_decoded` returns one `Ok(clocks(2))` for all six forms. That
    /// default is PINNED rather than argued, in `direct_timing_cases`.
    ///
    /// # What this reaches, stated correctly
    ///
    /// **32-bit byte ALU, and only that.** The slice's first write-up justified this kind as
    /// "ubiquitous in DOS-era software — text-mode tools, byte blitters, character loops",
    /// explicitly discounting both benchmarks. That describes a population the arm CANNOT REACH,
    /// and an adversarial review caught it. Two independent gates put 16-bit code out of range:
    /// `try_direct_continuation` returns `Interpret` at every `!d` boundary, so no block is ever
    /// compiled in a 16-bit code segment on any persona; and a `66`-prefixed byte ALU is refused
    /// because none of `0x00`/`0x02`/…/`0x38`/`0x3A` is in `classify`'s `OperandSize::Word`
    /// allowlist — which `sixteen_bit_byte_alu_register_form_is_still_a_barrier` locks in.
    ///
    /// So the honest claim is narrower and still worth having: this closes a byte-lane hole in
    /// 32-bit protected-mode coverage that had been open since the ALU block was written, and it
    /// is a PRECONDITION for the DOS-era population rather than a delivery of it. It becomes that
    /// only if 16-bit blocks are ever admitted (Phase 4's persona-generality work), at which point
    /// the allowlist has to take these opcodes too.
    ///
    /// **The allowlist doctrine is inconsistent here and that is a known tension, not an
    /// oversight.** ALU form 4 (`0x04..=0x3c`), `0x88`, `0x8a`, `0xb0..=0xb7`, `0xc6`, `0xf6` and
    /// `0x84` are all admitted at Word on the stated grounds that a byte form's width is a
    /// property of the FORM and the prefix cannot leak past `classify`. Forms 0 and 2 satisfy that
    /// argument identically — they carry a literal byte lane and no `operand_width` — so the only
    /// thing keeping them out is the campaign's standing rule against unmeasured admissions. Left
    /// out deliberately: admitting them would be a formation change with no census row to
    /// attribute it to, and it belongs to whichever slice opens the 16-bit region as a whole.
    AluRegByte {
        op: u8,
        dst: u8,
        src: u8,
    },
    AluMemSource {
        op: u8,
        dst: u8,
        width: MemoryWidth,
        addr: DirectAddr,
    },
    AluMemDest {
        op: u8,
        source: StoreSource,
        width: MemoryWidth,
        addr: DirectAddr,
    },
    Test {
        a: u8,
        b: u8,
    },
    TestByte {
        a: u8,
        b: u8,
    },
    Imul {
        dst: u8,
        src: u8,
    },
    /// IMUL r32, r/m32, imm — the THREE-operand form, REGISTER source only (0x69 with a full-width
    /// immediate, 0x6B with a sign-extended imm8). `dst = src * imm`, and `dst` may equal `src`.
    ///
    /// One variant for both opcodes because the interpreter reaches them through two arms that are
    /// character-for-character identical past the immediate `decode` already sign-extended
    /// (`execute_extended.rs`, the 0x69 and 0x6b arms): same `imul_truncated`, same
    /// `write_gpr_sized`, same `clocks(14)`. The opcode difference is entirely a decode-time
    /// question about how many immediate bytes to fetch, and `decode` has already answered it into
    /// `insn.imm`.
    ///
    /// Separate from `Imul` rather than an `Option<u32>` source on it, for the reason `ImulMem`'s
    /// comment gives about the same choice: the two charge DIFFERENT clocks (14 here against
    /// `Imul`'s 9), so a shared discriminant would put the charge behind a guard inside one
    /// `raw_clocks` arm where writing the bare pattern silently picks the wrong one.
    ///
    /// No `width` field: 0x69 and 0x6b are absent from `classify`'s `OperandSize::Word` allowlist,
    /// so a 66-prefixed encoding never reaches the arm that builds this and the source is always a
    /// dword. The MEMORY form is absent too — see the classify arm for why it is a missed lowering
    /// rather than a hazard.
    ImulImm {
        dst: u8,
        src: u8,
        imm: u32,
    },
    /// BT r/m32, r32, REGISTER form only (0F A3 with mod == 0b11).
    ///
    /// Writes CF alone and architecturally preserves SF, ZF, PF, AF and OF, which is why it
    /// publishes through `emit_set_cf_only` rather than through the usual capture-and-publish.
    ///
    /// The MEMORY form is deliberately absent and is not an oversight: for a memory operand the
    /// interpreter adjusts the effective address by the bit index at RUNTIME (`bit_string_op`
    /// with `register_index = true`), and a `DirectAddr` is a static address expression that
    /// cannot express that.
    Bt {
        rm: u8,
        index: u8,
    },
    /// IMUL r32, r/m32, MEMORY form (0x0FAF). Same flag contract and the same clocks(9) as
    /// `Imul`, because the interpreter reaches both through one `imul_truncated` call and one
    /// `clocks(9)`; the only difference is where the source comes from.
    ///
    /// No `width` field, for the reason `NegReg` and `MulReg` have none: the `OperandSize::Word`
    /// gate excludes 0x0faf, so a 66-prefixed IMUL can never reach the classify arm and the source
    /// is always a dword.
    ///
    /// Deliberately a separate variant rather than a source enum on `Imul`. `Imul` defaults
    /// CORRECTLY to zero or None in `dword_reads`, `has_dword_read` and `read_segment` precisely
    /// because it is register-only. Widening it would turn each of those into a guarded pattern
    /// where writing the bare `Self::Imul { .. }` over-declares the register form's traffic, with
    /// no compiler assistance either way. Two discriminants keep every one of those arms a
    /// visible, testable edit.
    ImulMem {
        dst: u8,
        addr: DirectAddr,
    },
    /// NEG r/m32, register form (0xF7 /3). No width field on purpose; see the classify arm.
    NegReg {
        dst: u8,
    },
    /// ROL and ROR r/m32, register form (0xC1 and 0xD1, sub-opcodes /0 and /1). `count` is the RAW
    /// decoded immediate; the emitter applies the architectural five-bit mask, exactly as `Shift`
    /// does.
    ///
    /// `op` is the guest ModRM `reg` field, 0 for ROL and 1 for ROR, and it is passed STRAIGHT
    /// through to `shift_r32_imm8`'s own `/op` slot. Host group 2 numbers its sub-opcodes the same
    /// way the guest does, so the field needs no translation table and a widening to a third
    /// sub-opcode would need one -- /2 and /3 are RCL and RCR, which the classify arm refuses
    /// because they take the incoming CF as a rotate INPUT.
    ///
    /// No `width` field, and refusing rather than adding one is a deliberate boundary: `classify`
    /// returns None for both sub-opcodes at `OperandSize::Word`, because this kind's emitter is
    /// `shift_r32_imm8` and a 66-prefixed rotate routed through it would rotate 32 bits where the
    /// guest rotates 16 -- wrong result AND wrong CF, since the bit rotated across the boundary
    /// comes from bit 31 instead of bit 15. Neither the duke3d-586 nor the 16-bit census measures
    /// a Word row for either sub-opcode, so a second emitter lane would be an unmeasured
    /// admission.
    ///
    /// Deliberately NOT folded into `Shift`. That variant's emitter falls through to an
    /// arithmetic shift right, so a rotate routed through it would be silently emitted as SAR.
    /// It also differs in the flag contract that matters here: a shift
    /// leaves AF, and OF above count 1, architecturally UNDEFINED, which is the only reason
    /// `emit_shift` may publish a possibly stale RBP to eflags. A rotate PRESERVES SF, ZF, PF and
    /// AF, so it must not.
    RotateReg {
        op: u8,
        dst: u8,
        count: u8,
    },
    /// MUL r/m32, register form (0xF7 /4). Unsigned, and the only DirectKind whose destination is
    /// implicit: it writes guest EAX and EDX regardless of `src`. No width field, for the same
    /// reason NegReg has none; see the classify arm.
    MulReg {
        src: u8,
    },
    /// IMUL r/m32, one-operand SIGNED multiply, MEMORY form (0xF7 /5). Writes guest EAX and EDX
    /// regardless of the address, the same implicit destination `MulReg` has.
    ///
    /// **No `raw_clocks` field, and that is deliberate rather than an omission.** The interpreter's
    /// whole group-3 arm returns `Ok(clocks(2))` for every sub-opcode and both operand forms, which
    /// IS the `raw_clocks` default below, so carrying a field here would invent a charge. This is
    /// the exact inverse of `ImulMem`, where 0x0FAF is charged `clocks(9)` and the default
    /// undercharges by 7, so a reader who has seen only that variant will be tempted to "fix" this
    /// one. `MulReg` already depends on the same default.
    ///
    /// No `width` field either, for the reason `MulReg` has none: the `OperandSize::Word` gate
    /// excludes 0xF7, so a 66-prefixed form cannot reach the classify arm.
    ImulMemAcc {
        addr: DirectAddr,
    },
    /// IMUL r/m32, one-operand SIGNED multiply, REGISTER form (0xF7 /5) -- the sibling
    /// `ImulMemAcc` is named after. Same implicit EDX:EAX destination, and no `raw_clocks` and no
    /// `width` field for exactly the reasons that variant gives.
    ImulRegAcc {
        src: u8,
    },
    /// DIV (0xF7 /6) and IDIV (0xF7 /7) r/m32, REGISTER form; `signed` selects IDIV.
    ///
    /// The only lowered kind whose instruction can raise #DE, and it never raises one on the
    /// HOST: `emit_div_reg` guards a superset of the interpreter's fault set and side-exits with
    /// the instruction UN-STARTED, so the interpreter re-executes it and faults by its own rules.
    /// Read that function before touching this. Carries no `raw_clocks` field: group 3 charges
    /// `clocks(2)` for every sub-opcode, which is the `_ => 2` default -- the `ImulMemAcc`
    /// situation, not the `ImulMem` one.
    ///
    /// ONE variant with a flag where the encoder keeps `div_r32` and `idiv_r64` apart, because
    /// here the flag reaches a `match` whose two arms are separately emitted and separately
    /// tested; there, it would have been one character inside a shared body.
    DivReg {
        src: u8,
        signed: bool,
    },
    TestImmReg {
        dst: u8,
        imm: u32,
        width: MemoryWidth,
    },
    TestImmMem {
        imm: u32,
        width: MemoryWidth,
        addr: DirectAddr,
    },
    /// SHL/SHR/SAL/SAR r/m, imm8, REGISTER form (0xC1 /4../7 and 0xD1 /4../7), plus SHL r8, imm8
    /// (0xC0 /4). `count` is the RAW decoded immediate; the emitter applies the architectural
    /// five-bit mask.
    ///
    /// `width` is the operand size and reaches the emitter at all three widths. It exists for the
    /// reason `LoadExtend::dst_width` does -- a shift is a REGISTER-DESTINATION write, so a
    /// Dword-only lowering of a 66-prefixed form clobbers the destination's high 16 bits where the
    /// interpreter's `write_operand_sized(.., Word, ..)` merges into the low 16. That is a
    /// miscompile, not a missed lowering, which is why the Word allowlist refused 0xC1 until this
    /// field existed.
    ///
    /// **At Byte the field is not `operand_width` and must never become it.** 0xC0 is a byte
    /// opcode whose width is fixed by the OPCODE, not by the operand-size prefix: an unprefixed
    /// 0xC0 in a 32-bit segment decodes with `OperandSize::Dword`, which the duke3d-586 census
    /// reports verbatim on the `0xC0 /4 register dword` row. The classify arm therefore hard-codes
    /// `MemoryWidth::Byte`, the way 0xC6's arm does, and passing `operand_width` there would
    /// silently emit a 32-bit shift of the whole home register.
    ///
    /// **`dst` is a BYTE-register index at Byte width**, where 4..7 name AH/CH/DH/BH rather than
    /// the homes of EBP/ESI/EDI. `emit_shift`'s Byte arm therefore goes through
    /// `emit_read_store_value`/`emit_write_gpr8` instead of `home(dst)`, exactly as
    /// `emit_inc_dec_reg8` does, and reading this field with `home()` at Byte would shift the
    /// wrong register by 32 bits.
    ///
    /// **Every flag rule is the host's, at BOTH widths, and that is the whole argument for the
    /// Word lane rather than a masked Dword one.** `CpuGsw::shift_rotate` computes CF from the
    /// last bit shifted out of a `width`-wide operand, OF only at a masked count of exactly 1, and
    /// SF/ZF/PF from the `width`-wide result. A 16-bit host shift does all four against its own
    /// 16 bits, so `66 C1 /op` reproduces `BusWidth::Word` instruction for instruction; a 32-bit
    /// host shift over a zero-extended operand would take CF from bit 31, SF from bit 31, and for
    /// SAR would shift in zeros where the guest shifts in bit 15.
    ///
    /// No `raw_clocks` arm and none is owed: the interpreter's whole group-2 arm returns
    /// `Ok(clocks(2))` without consulting `operand_size`, which IS the `_ => 2` default. This is
    /// the `ImulMemAcc` situation rather than the `ImulMem` one -- adding an arm here would invent
    /// a charge, as the Phase 5 call-out double-charge did in the other direction.
    Shift {
        op: u8,
        dst: u8,
        count: u8,
        width: MemoryWidth,
    },
    /// Group-2 shift by CL (0xD3 /4../7), register destination. SHIFTS ONLY -- the imm8 arm also
    /// admits ROL (/0) and ROR (/1) but routes them to `RotateReg`, because rotates do not define
    /// PF, ZF, SF or AF and `emit_shift_cl` merges the shift mask.
    ///
    /// A separate variant rather than a `ShiftCount` field on `Shift`: `Shift` carries its count
    /// as a decoded immediate (`count: u8`), while this form's count comes from CL at emission
    /// time, not a literal, so the two do not share a representation.
    ShiftCl {
        op: u8,
        dst: u8,
    },
    DoubleShiftReg {
        left: bool,
        dst: u8,
        src: u8,
        count: ShiftCount,
    },
    DoubleShiftMem {
        left: bool,
        src: u8,
        count: ShiftCount,
        addr: DirectAddr,
    },
    Load {
        dst: u8,
        width: MemoryWidth,
        addr: DirectAddr,
        raw_clocks: u8,
    },
    /// MOVZX/MOVSX r16/r32, r/m8 or r/m16, MEMORY form (0x0FB6, 0x0FB7, 0x0FBE, 0x0FBF).
    ///
    /// `width` is the SOURCE width and is only ever Byte or Word. This differs from `Load`, where
    /// the source and destination widths are the same. Any shared code that reads this field must
    /// treat it as the memory access width, never as the write-back width -- that is `dst_width`.
    ///
    /// `dst_width` is the DESTINATION width and is only ever Word or Dword. It is the operand
    /// size, so the two fields are independent: `66 0F B6` is a Byte source into a Word
    /// destination. Dword defines all 32 bits of the destination, which is the whole point of the
    /// instruction; Word defines only the low 16 and PRESERVES the high 16, because the
    /// interpreter's `write_gpr_sized(.., Word, ..)` is `write_gpr16`. Carrying it as a field
    /// rather than assuming Dword is what admits the 66-prefixed forms at all: with a hard-coded
    /// 32-bit write-back they are a miscompile, not a missed lowering.
    ///
    /// Deliberately NOT a flag on `Load`: `Load`'s emitter is a plain move, and an extending load
    /// (zero/sign-extend into the destination) needs different emitted code, not a conditional
    /// branch inside the same arm.
    LoadExtend {
        dst: u8,
        width: MemoryWidth,
        dst_width: MemoryWidth,
        signed: bool,
        addr: DirectAddr,
        raw_clocks: u8,
    },
    /// MOVZX/MOVSX r16/r32, r8 or r16, REGISTER form (0x0FB6, 0x0FB7, 0x0FBE, 0x0FBF, mod == 3).
    ///
    /// `width` is the SOURCE width and is only ever Byte or Word; `dst_width` is the destination
    /// width and is only ever Word or Dword. Both mean exactly what `LoadExtend`'s do.
    ///
    /// `src` at Byte width is a BYTE-REGISTER index, so 4 to 7 mean AH, CH, DH and BH, the high
    /// byte of `home(src - 4)`. It is NOT a home index and shared code must never use it as one.
    /// At Word width it is an ordinary register index. This matches what the interpreter's
    /// `read_gpr8` does with the same field, which is why `emit_read_store_value` can be reused
    /// verbatim rather than the lane arithmetic re-derived here.
    ///
    /// Separate from `LoadExtend` because that variant carries a `DirectAddr` and the whole
    /// memory registration set. A register form needs none of it and must not be handed a
    /// fabricated address.
    MovExtendReg {
        dst: u8,
        src: u8,
        width: MemoryWidth,
        dst_width: MemoryWidth,
        signed: bool,
    },
    Store {
        source: StoreSource,
        width: MemoryWidth,
        addr: DirectAddr,
        raw_clocks: u8,
    },
    RmwIncDec {
        is_dec: bool,
        width: MemoryWidth,
        addr: DirectAddr,
    },
    Push {
        source: StoreSource,
    },
    /// PUSH on a 16-bit stack (SS.B = 0) at Word operand size: two bytes written at
    /// `(SP - 2) & 0xFFFF`, and only SP advances, preserving ESP[31:16].
    ///
    /// A SEPARATE variant rather than a width field on `Push`, because `Push`'s emitter
    /// hard-codes `MemoryWidth::Dword` and `iadd_imm(esp, -4)`. The two widths it stands for are
    /// ORTHOGONAL: SS.B picks the stack-pointer width and `operand_size` picks how many bytes
    /// move (386 PRM 16.2, restated at `memory.rs:1218`). This variant is the (SS.B = 0, Word)
    /// cell only; the compile loop refuses the other two new cells.
    Push16 {
        source: StoreSource,
    },
    Pop {
        dst: u8,
    },
    /// POP on a 16-bit stack (SS.B = 0) at Word operand size: two bytes read at `SP & 0xFFFF`,
    /// only SP advances (preserving ESP[31:16]), and the destination is MERGED into rather than
    /// replaced, exactly as `write_gpr_sized(index, Word, ..)` does.
    ///
    /// Separate variant for the same reason as `Push16`: `Pop`'s emitter hard-codes the 32-bit
    /// width, the +4 advance AND a full 32-bit destination write.
    Pop16 {
        dst: u8,
    },
    Leave,
    /// LEAVE (0xC9): `ESP <- EBP` then `POP EBP`. Fieldless because the instruction has no
    /// operands and its 16-bit operand-size form never reaches the classifier (the
    /// OperandSize::Word gate rejects 0xc9 before the opcode arms). It cannot be spelled as
    /// `Pop { dst: 5 }` plus a register move, because `raw_clocks`, `read_segment` and the
    /// dword-read membership all key on the variant.
    ///
    /// There is deliberately no `Leave16`. The 16-bit STACK form (SS.B = 0) moves only BP into
    /// SP and preserves ESP[31:16], which the emitted full-width move would destroy; it is
    /// refused by the stack-width admission matrix in `compile_with_instruction_limit`, which
    /// sends any `uses_stack()` kind that is not an admitted (width, size) pair to `Retry`.
    Call {
        return_delta: u32,
        target_delta: u32,
    },
    /// CALL rel16 on a 16-bit stack at Word operand size: the return IP is pushed as two bytes
    /// at `(SP - 2) & 0xFFFF`, only SP advances, and the target wraps at 64K.
    ///
    /// A terminal, so it carries three obligations the 16-bit push and pop did not:
    /// `is_terminal`, the static successor record, and `static_control_target`. The last is what
    /// routes the target through `control_target_limit`'s Word clamp; without it this kind is
    /// admitted with an unmasked target, which is the exact miscompile that clamp exists for.
    Call16 {
        return_delta: u32,
        target_delta: u32,
    },
    Jmp {
        target_delta: u32,
    },
    Ret {
        release: u16,
    },
    /// RET near on a 16-bit stack at Word operand size: two bytes read at `SP & 0xFFFF`, the
    /// CS limit checked BEFORE any stack release, then SP alone advances by `2 + release`.
    ///
    /// A terminal with a DYNAMIC successor, so besides `is_terminal` it owes both
    /// `dynamic_successor` and an explicit `[None, None]` successor pair. Missing the second
    /// consumes link cell 0 for a static edge that the return path then cannot use, which halves
    /// the return PIC without changing any guest state.
    Ret16 {
        release: u16,
    },
    Jcc {
        condition: u8,
        taken_delta: u32,
    },
    /// NOP (0x90, architecturally XCHG (E)AX, (E)AX). Emits ZERO bytes.
    ///
    /// It exists only so block growth continues through it. The interpreter's arms are
    /// `Ok(clocks(3))` with no state change at all (`execute.rs`, and the hot-cached path in
    /// `run.rs`), and neither reads `operand_size`, so there is nothing to emit at either width.
    ///
    /// Fieldless for the same reason `Leave` is: the instruction has no operands. Unlike `Leave`
    /// it is not a stack user and not a terminal, so it rides every default arm in this impl
    /// except one. `raw_clocks` MUST carry an explicit arm; see the note there.
    ///
    /// Zero emit is not a special case. `emit_shift` and `emit_rotate_right_reg` already return
    /// early at count 0 and write nothing, and the per-slot accounting in `emit_block` is driven
    /// by the slot list rather than by emitted bytes.
    Nop,
    /// CLD (0xFC) / STD (0xFD). DF is bit 10 of EFLAGS and sits OUTSIDE the lazy arithmetic
    /// descriptor: `set_flag`'s ARITH mask is CF|PF|AF|ZF|SF|OF, so a DF write goes straight to
    /// `set_flag_live` and a DF read falls through to live eflags. Nothing here touches
    /// `pending_flags`, which is why this needs no flag-descriptor dance.
    DirectionFlag {
        set: bool,
    },
    /// PUSH r/m32, MEMORY form (0xFF /6): one dword read at `addr`, one dword write at
    /// `SS:[ESP-4]`, then `ESP -= 4`.
    ///
    /// The first kind with two accesses at DIFFERENT addresses. Both refuse every page kind but
    /// plain RAM, which is what keeps `emit_mode13_read_completion` out of this slot. That
    /// completion increments the dynamic mode-13 read count as soon as the read resolves, while
    /// the STORE guards can still side exit afterwards, and a side exit reports the dynamic
    /// counters against a static snapshot taken before the slot. `run.rs`'s
    /// `dword_reads - exit.mode13_dword_reads` would then go negative: a debug panic, and in
    /// release a wrap that is saturating-multiplied into the bus charge.
    ///
    /// The register form (mod == 3) is deliberately absent. It is architecturally `PUSH r32`,
    /// whose clock charge would have to be checked against 0x50..0x57 rather than assumed, and
    /// the attribution census measures zero occurrences of it.
    ///
    /// `raw_clocks` carries NO arm: the interpreter's group-5 arm 6 returns `clocks(2)`, which is
    /// already the `_ => 2` default. This is the `ImulMemAcc` situation, not the LEAVE one. The
    /// charge is still pinned in a fixture, because "correctly rides the default" and "nobody
    /// checked" look identical in a diff.
    PushMem {
        addr: DirectAddr,
    },
    /// JMP r/m32, MEMORY form (0xFF /4): one dword read at `addr`, and EIP becomes the value
    /// read. Nothing else changes: no stack, no push, no flags.
    ///
    /// The register form is `JmpReg`, below. It was absent for two reasons that have both since
    /// been settled: the attribution census measured zero occurrences of it (the duke3d-486
    /// census now reads 11,718,562 static exits, 11,736,700 interpreted executions, and 586 reads
    /// 32.8M/32.8M -- its fourth-largest rejected row), and its clock charge was unverified
    /// (`execute_extended.rs` group-5 arm 4 returns `clocks(7)` unconditionally, for the register
    /// and memory operand alike, so it charges what this kind charges).
    ///
    /// `raw_clocks` carries an explicit 7, joining the `Call`/`Call16`/`Jmp` arm: the
    /// interpreter's group-5 arm 4 returns `clocks(7)`, and the `_ => 2` default would undercharge
    /// by 5.
    ///
    /// The first non-`Ret` kind with a DYNAMIC successor. That makes the `successors` and
    /// `dynamic_successor` registrations mutually exclusive on this block by an invariant of
    /// `LinkCell`, not by convention: `LinkCell::clear` resets the portal, `entry_top` and
    /// spilling, but not `target_eip`, and a static `set` writes only the portal. So a cell that
    /// was once dynamically bound to some EIP and is later statically rebound still carries the
    /// old `target_eip`, and any later jump landing on that stale value transfers natively into
    /// whatever block the static rebind pointed at, the wrong one. Recording a static successor
    /// for this kind alongside its dynamic one would set up exactly that trap the next time the
    /// cell's static edge is retargeted.
    JmpMem {
        addr: DirectAddr,
    },
    /// JMP r/m32, REGISTER form (`0xFF /4`, mod == 3): EIP becomes the register's value and
    /// nothing else changes. `CallReg` minus the push, or `JmpMem` minus the memory read -- it
    /// touches no memory at all, which makes it the simplest dynamic-successor kind in the set.
    ///
    /// Four properties, each inherited rather than invented, and each pinned by a mutation:
    ///
    /// * **Dword only, gated in `classify`.** `0xff` is on the `OperandSize::Word` allowlist, so a
    ///   `66 FF /4` in 32-bit code reaches the arm at Word size, where the interpreter reads two
    ///   bytes and masks EIP to 16 bits. Nothing downstream refuses that: `uses_stack` is false
    ///   for a jump so the stack-width admission matrix never sees this kind, and
    ///   `static_control_target` is `None` for a dynamic target so the Word control clamp never
    ///   sees it either. The `/4` arm's existing `insn.operand_size != OperandSize::Dword` gate is
    ///   the ONLY thing standing between this kind and a miscompile, and it is shared with the
    ///   memory form rather than duplicated. The residual `0xFF /4` register WORD census row
    ///   (78,585 exits) stays refused by it, deliberately.
    /// * **`raw_clocks` 7**, joining the `Call`/`Call16`/`Jmp`/`JmpMem`/`CallReg`/`CallMem` arm.
    ///   `execute_extended.rs` group-5 arm 4 reads its target through `read_operand_sized`, which
    ///   serves both operand forms, and returns `Ok(clocks(7))` without branching on the shape. So
    ///   the register form charges exactly what the memory form charges, and the `_ => 2` default
    ///   would undercharge every one of them by 5.
    /// * **No static successor.** `[None, None]` plus `dynamic_successor`, the shape `Ret`,
    ///   `JmpMem` and `CallReg` share. The mutual-exclusion invariant on `JmpMem`'s doc comment
    ///   applies unchanged: a `LinkCell` that was dynamically bound and is later statically
    ///   rebound keeps its stale `target_eip`, and a later jump landing on that value transfers
    ///   natively into the wrong block. Recording a static successor here would arm that trap.
    /// * **The CS-limit check precedes the slot's completion accounting.** There is no guest byte
    ///   to leave untouched -- this kind mutates only EIP -- so the atomicity question is entirely
    ///   about WHICH EIP the side exit resumes at. Publishing the limit exit after `completed`
    ///   and `completed_raw` advance would hand the interpreter a state in which the jump already
    ///   retired, and it would re-enter past the instruction that faulted with its 7 clocks
    ///   already charged.
    JmpReg {
        dst: u8,
    },
    /// CALL r32, REGISTER form (0xFF /2, mod == 3): the target is read from a GPR BEFORE the
    /// return EIP is pushed. One dword store to SS:[ESP-4], ESP falls by 4, then EIP becomes the
    /// register's value. Dynamic successor, the same `[None, None]` + `dynamic_successor` shape
    /// as `Ret` and `JmpMem`; the mutual-exclusion invariant documented on `JmpMem`'s doc comment
    /// (a cell cannot carry both a static successor and a dynamic binding) applies here unchanged.
    ///
    /// `raw_clocks` carries an explicit 7, joining the `Call`/`Call16`/`Jmp`/`JmpMem` arm: the
    /// interpreter's group-5 arm 2 returns `clocks(7)`, and the `_ => 2` default would undercharge
    /// by 5.
    ///
    /// No width variant, unlike `Call`/`Call16`. `uses_stack` routes this kind into the
    /// stack-width admission matrix, and that matrix has no `CallReg16` mapping arm, so a Word
    /// form (a 66-prefixed `FF /2` in 32-bit code) falls to the matrix's catch-all `Retry` rather
    /// than being silently pushed as a four-byte dword. The PushMem precedent: nothing here needs
    /// to gate the width itself.
    ///
    /// The register is admitted unconditionally, ESP included: the emit arm reloads the target
    /// from its home register BEFORE the ESP adjust, which delivers the architecturally correct
    /// pre-push value for `call esp` the same as for every other register.
    CallReg {
        dst: u8,
        return_delta: u32,
    },
    /// CALL r/m32, MEMORY form (0xFF /2, mod != 3): one dword read at `addr` for the target, one
    /// dword store of the return EIP to SS:[ESP-4], ESP falls by 4, then EIP becomes the value
    /// read. Doom's largest remaining rejected census row (1,847,385 attributed exits,
    /// 3,076,346 interpreted executions per timedemo); quake has none at any width.
    ///
    /// Two addresses in two different segments, exactly `PushMem`'s shape, and the emit arm shares
    /// its mechanism down to the parking slot. What is new is the combination: this is the first
    /// kind that both WRITES memory and takes a dynamic successor.
    ///
    /// Three properties are load-bearing and each is inherited from a different sibling:
    ///
    /// * from `PushMem`, the source read is RAM-ONLY and emits no read-completion. A mode-13
    ///   source would bump the dynamic read count before the STORE guards below it can still side
    ///   exit, and `run.rs`'s `dword_reads - exit.mode13_dword_reads` would underflow. Refusing
    ///   the aperture makes that unreachable rather than merely mis-ordered.
    /// * from `CallReg`, the CS-limit check on the target runs BEFORE any mutation. The
    ///   interpreter pushes first and faults only on the next fetch, so a native side exit must
    ///   leave every guest byte untouched and let the interpreter reproduce push-then-fault.
    /// * from `JmpMem`, the operand read happens BEFORE the push, matching the interpreter's own
    ///   order (`execute_extended.rs` group 5 arm 2 reads the operand, then pushes). It matters
    ///   for `call dword [esp+N]`, where the push would otherwise move the address being read.
    ///
    /// `raw_clocks` carries an explicit 7 with the rest of the group-5 control transfers.
    ///
    /// The Dword gate lives in `classify`, as it does for `JmpMem`: at Word size the interpreter
    /// reads TWO bytes and masks EIP to 16 bits. `uses_stack` additionally routes the kind through
    /// the stack-width matrix, which has no `CallMem16` arm, so the two refusals are independent.
    CallMem {
        addr: DirectAddr,
        return_delta: u32,
    },
    X87 {
        insn: NativeX87Insn,
        addr: Option<DirectAddr>,
    },
    /// An interpreter CALL-OUT slot: the block spills, routes exactly ONE instruction through a
    /// Rust helper that reaches the bus, reloads, and keeps running. Phase 5 carries exactly one
    /// opcode here, `0xEC` (IN AL,DX), and the helper is named by the variant so a second opcode
    /// cannot be added without an emitter arm of its own.
    ///
    /// `raw_clocks` carries an explicit `=> 0` arm, and the rule is the opposite of what this
    /// comment claimed when the slice shipped: the call-out's ENTIRE charge is the RUNTIME lane
    /// (the helper's return value, added at the call site), so the STATIC lane must see zero.
    /// Omitting the arm does not express that -- it selects the `_ => 2` default and charges 2 on
    /// top of the runtime 12. `X87` carries its own `=> 0` for exactly this reason.
    ///
    /// The `completed_raw == raw_clocks` assertion at the end of `emit` is BLIND to this class
    /// either way: it sums the same accessor it checks against, so it agrees with itself whatever
    /// the arm returns. A single-slot differential is nearly blind too, because at the 586 dial
    /// the two-clock error floors away. Only accumulation separates them; that is what
    /// `cpu_jit_callout_matrix_test.rs` does across one to four slots.
    ///
    /// The static bound the budget needs lives in `Compilation::callout_slots` instead, folded
    /// into `compute_iteration_upper` (see the derivation there).
    CallOut {
        helper: CallOutHelper,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemoryWidth {
    Byte,
    Word,
    Dword,
    /// An x87 m64 access. NOT twice a Dword in the way it is guarded: the interpreter's
    /// `read_qword`/`write_qword` (`fpu_exec.rs:720-740`) issue two independently 4-aligned
    /// dword bus transactions rather than one 8-aligned qword transaction, so native must
    /// require only 4-alignment (plus a no-page-cross check) to admit the same population the
    /// interpreter admits. See `alignment_bytes` below, which is why `bytes()` (8, the size)
    /// and `alignment_bytes()` (4, the guard) diverge for this variant alone.
    Qword,
    /// An x87 m80 access: ten bytes as ONE guarded region, not two.
    ///
    /// The interpreter issues three transactions (`write_extended80`: two dwords through
    /// `write_qword`, then a word at +8), and the obvious native shape is two independently
    /// guarded pointers. That shape is wrong for a reason that has nothing to do with
    /// correctness of the bytes: the first access's dynamic bus counters are incremented before
    /// the second pointer's page guard can side exit, so a ten-byte store straddling a page
    /// would charge two dword writes, exit, and let the interpreter charge all three again.
    /// Guarding the whole span once makes the access all-or-nothing, which is the same property
    /// every other width here has.
    ///
    /// `alignment_bytes()` is 4, inherited from Qword for the same reason and not by copying.
    /// The first eight bytes ARE Qword's two dword transactions, and the interpreter's dword
    /// write only takes the direct-page path when it is 4-aligned; at 2-aligned it falls onto
    /// the slow bus path and is charged clocks the native store does not pay. Admitting a merely
    /// 2-aligned m80 therefore diverges on bus timing rather than on bytes, which is what the
    /// m80 differential fixture caught when this was first written as 2. Ten-byte compiler
    /// temporaries that land 2-aligned stay on the interpreter, the same population cut FST m64
    /// already lives with.
    Tbyte,
}

/// The `MemoryWidth` of one x87 memory access, and the SINGLE source of truth for it.
///
/// Every consumer routes through here: `word_reads`, `dword_reads`, `word_stores`,
/// `dword_stores`, `has_dword_read`, `has_dword_store` and the emitter.
/// That is not tidiness. If the accessors tested `access.width` inline while only the emitter
/// called this, then breaking this function would move the emitted guard and the emitted dynamic
/// counter while leaving the static registration correct, and the registration test would pass
/// through the mutation.
///
/// The unknown arm PANICS rather than defaulting to Dword. A silent default is what would let a
/// future 8-byte access (Tier 2: FLD/FST m64, the m80 forms) be charged as a dword, and the
/// static-versus-dynamic disagreement that produces underflows `ram_word_reads` in `run.rs`
/// rather than failing a test.
fn x87_memory_width(access: NativeX87MemoryAccess) -> MemoryWidth {
    match access.width {
        2 => MemoryWidth::Word,
        4 => MemoryWidth::Dword,
        8 => MemoryWidth::Qword,
        10 => MemoryWidth::Tbyte,
        other => unreachable!("x87 memory access width {other} has no MemoryWidth"),
    }
}

/// True when `kind` is an x87 slot whose memory access runs in `direction` at `width`.
fn x87_memory_access_is(
    kind: DirectKind,
    direction: NativeX87MemoryDirection,
    width: MemoryWidth,
) -> bool {
    let DirectKind::X87 { insn, .. } = kind else {
        return false;
    };
    let Some(access) = insn.metadata().memory else {
        return false;
    };
    access.direction == direction && x87_memory_width(access) == width
}

impl MemoryWidth {
    pub(crate) const fn bytes(self) -> u32 {
        match self {
            Self::Byte => 1,
            Self::Word => 2,
            Self::Dword => 4,
            Self::Qword => 8,
            Self::Tbyte => 10,
        }
    }

    /// The guard's alignment requirement, distinct from `bytes()` for Qword AND Tbyte. Byte,
    /// Word and Dword self-align (`alignment_bytes() == bytes()`), so for those three the two
    /// names are interchangeable and a caller that reaches for the wrong one still emits the
    /// byte-identical guard.
    ///
    /// For the two wide widths they diverge on purpose: the interpreter reads an m64 as two
    /// independently 4-aligned dword transactions (`fpu_exec.rs:720-740`), not one 8-aligned
    /// qword transaction, so requiring 8-byte alignment natively would refuse a large population
    /// of legitimately-4-aligned doubles that DOS compilers emit. Tbyte's first eight bytes ARE
    /// those two transactions, so it inherits the same 4, and the divergence is wider still
    /// (4 against 10). `emit_wide_page_guard` is the site that must read this method rather than
    /// `bytes()` for the alignment mask -- and, because both wide widths have
    /// `alignment_bytes() < bytes()`, the site whose page-crossing compare is live for both.
    pub(crate) const fn alignment_bytes(self) -> u32 {
        match self {
            Self::Byte => 1,
            Self::Word => 2,
            // Tbyte joins the four-byte group rather than getting its own arm: its first eight
            // bytes are literally Qword's two dword transactions, so the requirement is the
            // same one, not a coincidence. See the variant's comment.
            Self::Dword | Self::Qword | Self::Tbyte => 4,
        }
    }

    /// The mask an alignment test ANDs against a linear address: nonzero means the access does
    /// not meet this width's alignment requirement. The `MemoryWidth` counterpart of
    /// `BusWidth::misaligned_at`, which cannot be reused here because the two carve Qword and
    /// Tbyte differently -- see `alignment_bytes` above for why those two ask for 4.
    ///
    /// Read this at a site that asks the GUARD's question, "did the call-site alignment test
    /// consider this misaligned". A site asking whether the access is one natural transaction of
    /// its full width wants `bytes() - 1` instead, and the two Mode 13h refusals in `load_fast`
    /// and `store_fast` are exactly that: a smaller mask refuses fewer accesses, so substituting
    /// this method there would WEAKEN a deliberately conservative aperture gate.
    pub(crate) const fn alignment_mask(self) -> u32 {
        self.alignment_bytes() - 1
    }

    /// Extra byte cycles a misaligned access of this width owes the bus, beyond the single wide
    /// cycle the block already charged statically. The interpreter charges all `bytes()` of them
    /// (`BusWidth::charge_direct_ram_split` loops `0..width.bytes()`) because it never charged a
    /// wide cycle first, so the JIT owes exactly one fewer.
    ///
    /// NOT the alignment mask, which it happens to equal for the three self-aligning widths.
    /// `split_extra_bytes` is the name this quantity already carries at the lane that receives it
    /// (`frame.rs`, `native_exit.rs`, `run.rs`), so it keeps that name here rather than gaining a
    /// fourth one.
    pub(crate) const fn split_extra_bytes(self) -> u32 {
        self.bytes() - 1
    }

    pub(crate) const fn needs_alignment_guard(self) -> bool {
        !matches!(self, Self::Byte)
    }
}

#[derive(Clone, Copy)]
pub(crate) enum StoreSource {
    /// PUSHFD's operand: the materialized EFLAGS, masked to what the persona pushes. The mask is
    /// resolved in `stack_width_kind` (which has the CPU) rather than in `classify` (which does
    /// not), and `u32::MAX` is the unresolved placeholder classify emits.
    Flags {
        mask: u32,
    },
    Reg(u8),
    Imm(u32),
    /// A segment register's SELECTOR, for `PUSH Sreg`. Resolved to an `Imm` at the top of
    /// `emit_store`, which is where the `SegmentLayout` is in hand; `classify` cannot do it
    /// because it has no CPU, and `stack_width_kind` must not, because resolving early would
    /// throw away the segment identity that `selector_segment` needs to pin the descriptor.
    Selector(SegmentIndex),
    EipDelta(u32),
}

#[derive(Clone, Copy)]
pub(crate) enum ShiftCount {
    Immediate(u8),
    Cl,
}

#[derive(Clone, Copy)]
pub(crate) struct DirectAddr {
    pub(crate) segment: SegmentIndex,
    pub(crate) base: Option<u8>,
    pub(crate) index: Option<u8>,
    pub(crate) scale: u8,
    pub(crate) disp: u32,
    /// Present only for the shapes `disp_lane_for` admits (the `0x8A MOV r8, [..disp32..]`
    /// family). When present the emitted effective address IGNORES `disp` and loads the four
    /// displacement bytes out of guest RAM on every execution, so a guest patch of those bytes
    /// needs no recompile. `disp` still carries the value decoded at compile time and is what
    /// the non-lane form bakes.
    ///
    /// It rides on the ADDRESS rather than on `DirectKind::Load` because the displacement's one
    /// consumer is `emit_effective_address` and every memory emitter reaches it through
    /// `emit_segmented_linear_address` — including the one-lookup fast paths — so a single seam
    /// serves them all, and a later Store/AluMemSource admission is a `disp_lane_for` widening
    /// rather than new plumbing. The lane arm forms the address through EAX alone, so it is
    /// safe whatever a caller has staged in the other scratch registers.
    pub(crate) disp_lane: Option<ImmLane>,
}

impl DirectKind {
    pub(crate) fn raw_clocks(self) -> u32 {
        match self {
            // NOP joins Jcc at 3. Both interpreter arms charge `clocks(3)` and neither consults
            // `operand_size`. Without this arm it rides the `_ => 2` default and undercharges
            // every lowered NOP by one core clock, which NO other assertion in the tree can see:
            // the same gap shipped twice this campaign and was caught only by a mutation.
            Self::Jcc { .. } | Self::Nop => 3,
            // Both widths charge the same: the interpreter returns clocks(4) for 0x58..=0x5f
            // irrespective of operand size. Unlike Push, which correctly rides the `_ => 2`
            // default, an omitted arm here undercharges every pop by 2 core clocks and no test
            // would fail. LEAVE joins them: it is `ESP <- EBP` then POP and the interpreter
            // charges the same clocks(4) for the 0xc9 arm.
            Self::Pop { .. } | Self::Pop16 { .. } | Self::Leave => 4,
            // 0F A3 returns clocks(6) irrespective of operand size. Without this arm it rides
            // the `_ => 2` default and undercharges every BT by 4.
            Self::Bt { .. } => 6,
            Self::Call { .. }
            | Self::Call16 { .. }
            | Self::Jmp { .. }
            | Self::JmpMem { .. }
            | Self::JmpReg { .. }
            | Self::CallReg { .. }
            | Self::CallMem { .. } => 7,
            // Both widths charge the same: 0xc2 and 0xc3 return clocks(10) irrespective of
            // operand size. An omitted arm here falls to `_ => 2` and undercharges by 8.
            Self::Ret { .. } | Self::Ret16 { .. } => 10,
            Self::DoubleShiftReg { .. } | Self::DoubleShiftMem { .. } => 3,
            // PUSHFD is clocks(3) where PUSH r32 is clocks(2), so it cannot ride the `_ => 2`
            // default the other push forms use.
            Self::Push {
                source: StoreSource::Flags { .. },
            }
            | Self::Push16 {
                source: StoreSource::Flags { .. },
            } => 3,
            Self::Load { raw_clocks, .. }
            | Self::LoadExtend { raw_clocks, .. }
            | Self::Store { raw_clocks, .. } => u32::from(raw_clocks),
            // All four MOVZX/MOVSX interpreter arms return clocks(3) for BOTH operand forms
            // (execute.rs, and the hot-cached path in run.rs charges the same), against a default
            // of 2. The memory forms carry it as a field because Load and Store do; the register
            // form has no other field worth carrying, so it is a constant arm.
            Self::MovExtendReg { .. } => 3,
            // The interpreter's 0x0f90..=0x0f9f arm returns clocks(4) for both operand forms
            // against a default of 2, so this cannot ride the `_ => 2` arm below.
            Self::SetCc { .. } => 4,
            // 0x98 (CBW/CWDE) returns clocks(3) for both operand forms (execute.rs); 0x99
            // (CWD/CDQ) returns clocks(2) for both, which is what the `_ => 2` default already
            // gives, so Cdq deliberately has no arm here.
            Self::Cwde { .. } => 3,
            Self::X87 { .. } => 0,
            // ZERO, and it MUST carry an explicit arm for exactly the reason `X87` above does: a
            // call-out's whole charge arrives at RUNTIME, through the helper's return value and
            // the lane add at the call site. The `_ => 2` default is not a harmless approximation
            // here, it is a DOUBLE CHARGE -- the static 2 lands on top of the runtime 12 and every
            // native `IN AL,DX` costs 14 raw where the interpreter costs 12.
            //
            // It shipped, and the way it hid is worth recording. Nothing in the emitter can see
            // it: `completed_raw` sums this same function, so the end-of-`emit` assertion agrees
            // with itself whichever value the arm returns. And a single-slot differential rounds
            // it away -- at the 586 dial a three-slot block charges 18 raw against the
            // interpreter's 16, and both floor to the same scaled clock. It takes ACCUMULATION to
            // separate them, which is what the Task 2 matrix does (`call_out_charge_matches_the_
            // interpreter_across_slot_counts`, one to four slots).
            Self::CallOut { .. } => 0,
            // Matches the interpreter's clocks(7) for 0x8E (`execute.rs`, the arm that ends in
            // `load_segment_arming_ss_shadow`). The `_ => 2` default would under-charge by 5, and
            // the CallOut note above is the precedent for why that is invisible from inside the
            // emitter: `completed_raw` sums this same function, so the end-of-emit assertion
            // agrees with itself whatever this returns. Only an interpreter differential that
            // ACCUMULATES across slot counts separates a wrong arm from a right one.
            Self::LoadSegReal { .. } => 7,
            // Matches the interpreter's clocks(9) for 0x0FAF at execute_extended.rs. The default
            // arm below returns 2, which would under-charge this instruction by 7. Both operand
            // forms share the arm because the interpreter charges them from one `Ok(clocks(9))`.
            Self::Imul { .. } | Self::ImulMem { .. } => 9,
            // The THREE-operand IMUL charges clocks(14), not the two-operand form's clocks(9)
            // (execute_extended.rs, the 0x69 and 0x6b arms), so it cannot share the arm above and
            // it cannot ride the `_ => 2` default either -- that would under-charge it by TWELVE
            // raw clocks, the largest single-arm error this table could carry. The Phase 5
            // call-out double-charge is the precedent for why an omitted arm here is invisible to
            // the emitter's own `completed_raw` assertion: that assertion sums this same
            // accessor, so it agrees with itself whatever the arm returns.
            Self::ImulImm { .. } => 14,
            _ => 2,
        }
    }

    fn weighted_fp_clocks(self, persona: CpuPersona) -> u32 {
        match self {
            Self::X87 { insn, .. } => u32::try_from(insn.metadata().weighted_fp_clocks(persona))
                .expect("one x87 instruction's weighted clocks fit u32"),
            _ => 0,
        }
    }

    pub(crate) fn byte_reads(self) -> u8 {
        u8::from(matches!(
            self,
            Self::Load {
                width: MemoryWidth::Byte,
                ..
            } | Self::LoadExtend {
                width: MemoryWidth::Byte,
                ..
            } | Self::AluMemDest {
                width: MemoryWidth::Byte,
                ..
            } | Self::TestImmMem {
                width: MemoryWidth::Byte,
                ..
            }
        ))
    }

    pub(crate) fn word_reads(self) -> u8 {
        u8::from(
            matches!(
                self,
                Self::Load {
                    width: MemoryWidth::Word,
                    ..
                } | Self::LoadExtend {
                    width: MemoryWidth::Word,
                    ..
                } | Self::AluMemSource {
                    width: MemoryWidth::Word,
                    ..
                } | Self::AluMemDest {
                    width: MemoryWidth::Word,
                    ..
                } | Self::RmwIncDec {
                    width: MemoryWidth::Word,
                    ..
                } | Self::Pop16 { .. }
                    | Self::Ret16 { .. }
                    | Self::TestImmMem {
                        width: MemoryWidth::Word,
                        ..
                    }
            ) || x87_memory_access_is(self, NativeX87MemoryDirection::Read, MemoryWidth::Word),
        )
    }

    pub(crate) fn dword_reads(self) -> u8 {
        // An x87 Qword read is TWO dword bus transactions, not one (`read_qword` issues two
        // independent 4-aligned dword reads: fpu_exec.rs:720-740), so it adds 2 here rather than
        // matching alongside the Dword arm below, which would undercount by half. The two terms
        // are mutually exclusive (a kind is never both a Dword and a Qword access), so plain
        // addition cannot double count.
        u8::from(
            matches!(
                self,
                Self::Load {
                    width: MemoryWidth::Dword,
                    ..
                } | Self::AluMemSource {
                    width: MemoryWidth::Dword,
                    ..
                } | Self::AluMemDest {
                    width: MemoryWidth::Dword,
                    ..
                } | Self::RmwIncDec {
                    width: MemoryWidth::Dword,
                    ..
                } | Self::DoubleShiftMem { .. }
                    | Self::ImulMem { .. }
                    | Self::ImulMemAcc { .. }
                    | Self::TestImmMem {
                        width: MemoryWidth::Dword,
                        ..
                    }
                    | Self::Pop { .. }
                    | Self::Leave
                    | Self::Ret { .. }
                    | Self::PushMem { .. }
                    | Self::JmpMem { .. }
                    | Self::CallMem { .. }
            ) || x87_memory_access_is(self, NativeX87MemoryDirection::Read, MemoryWidth::Dword),
        ) + 2 * u8::from(x87_memory_access_is(
            self,
            NativeX87MemoryDirection::Read,
            MemoryWidth::Qword,
        ))
    }

    pub(crate) fn byte_stores(self) -> u8 {
        u8::from(
            matches!(
                self,
                Self::Store {
                    width: MemoryWidth::Byte,
                    ..
                }
            ) || matches!(
                self,
                Self::AluMemDest {
                    op: 0..=6,
                    width: MemoryWidth::Byte,
                    ..
                }
            ),
        )
    }

    pub(crate) fn word_stores(self) -> u8 {
        u8::from(
            matches!(
                self,
                Self::Store {
                    width: MemoryWidth::Word,
                    ..
                } | Self::RmwIncDec {
                    width: MemoryWidth::Word,
                    ..
                } | Self::Push16 { .. }
                    | Self::Call16 { .. }
            ) || matches!(
                self,
                Self::AluMemDest {
                    op: 0..=6,
                    width: MemoryWidth::Word,
                    ..
                }
            ) || x87_memory_access_is(self, NativeX87MemoryDirection::Write, MemoryWidth::Word)
                // An m80 write ends in a word at +8 (`write_extended80`), on top of the two
                // dwords `dword_stores` counts below. Both terms are needed: registering only the
                // dword pair against an emitted completion that also bumps the word lane is the
                // static-versus-dynamic disagreement that underflows `ram_word_writes`.
                || x87_memory_access_is(self, NativeX87MemoryDirection::Write, MemoryWidth::Tbyte),
        )
    }

    pub(crate) fn dword_stores(self) -> u8 {
        // Mirrors `dword_reads`'s Qword handling: `write_qword` is two independent 4-aligned
        // dword writes, so the Qword term adds 2 rather than joining the Dword arm's single
        // count. The two terms stay mutually exclusive for the same reason.
        u8::from(
            matches!(
                self,
                Self::Store {
                    width: MemoryWidth::Dword,
                    ..
                } | Self::RmwIncDec {
                    width: MemoryWidth::Dword,
                    ..
                } | Self::DoubleShiftMem { .. }
                    | Self::Push { .. }
                    | Self::Call { .. }
                    | Self::CallReg { .. }
                    | Self::PushMem { .. }
                    | Self::CallMem { .. }
            ) || matches!(
                self,
                Self::AluMemDest {
                    op: 0..=6,
                    width: MemoryWidth::Dword,
                    ..
                }
            ) || x87_memory_access_is(self, NativeX87MemoryDirection::Write, MemoryWidth::Dword),
        ) + 2 * u8::from(
            x87_memory_access_is(self, NativeX87MemoryDirection::Write, MemoryWidth::Qword)
                // The m80 write's first eight bytes are the same two dword transactions a Qword
                // write issues; its trailing word is registered by `word_stores`.
                || x87_memory_access_is(self, NativeX87MemoryDirection::Write, MemoryWidth::Tbyte),
        )
    }

    /// A correctness site, not bookkeeping. Defaulting a memory kind to `None` here makes
    /// `kind_segment_access_supported` trivially true AND keeps the segment out of the block's
    /// `SegmentLayout` mask, and `data_matches` SKIPS unused segments, so a cached block would
    /// keep matching after a guest DS reload and read through a STALE BASE.
    fn read_segment(self) -> Option<SegmentIndex> {
        match self {
            Self::Load { addr, .. }
            | Self::LoadExtend { addr, .. }
            | Self::ImulMem { addr, .. }
            | Self::ImulMemAcc { addr, .. }
            | Self::AluMemSource { addr, .. }
            | Self::AluMemDest { addr, .. }
            | Self::DoubleShiftMem { addr, .. }
            | Self::TestImmMem { addr, .. }
            | Self::RmwIncDec { addr, .. }
            | Self::PushMem { addr, .. }
            | Self::JmpMem { addr, .. }
            | Self::CallMem { addr, .. } => Some(addr.segment),
            Self::X87 {
                insn,
                addr: Some(addr),
            } if matches!(
                insn.metadata().memory,
                Some(access) if access.direction == NativeX87MemoryDirection::Read
            ) =>
            {
                Some(addr.segment)
            }
            Self::Pop { .. }
            | Self::Pop16 { .. }
            | Self::Leave
            | Self::Ret { .. }
            | Self::Ret16 { .. } => Some(SegmentIndex::Ss),
            _ => None,
        }
    }

    /// Segments this kind reads the SELECTOR of without touching memory through them.
    ///
    /// Separate from `read_segment` because the two want different things from
    /// `SegmentLayout::capture`: a read wants the descriptor pinned AND validated as accessible,
    /// while a selector read wants it pinned only. Folding this into `read_segment` would refuse
    /// to compile `mov ax, fs` whenever FS is null — a legal instruction with a legal answer.
    fn selector_segment(self) -> Option<SegmentIndex> {
        match self {
            // CS is excluded on purpose rather than by omission: it is not in `SegmentLayout.data`
            // at all, it rides the separate `cs` field, and `cs_matches` pins it for every block
            // unconditionally. Putting it in the mask would be inert at best.
            Self::MovSegToReg { segment, .. } if segment != SegmentIndex::Cs => Some(segment),
            // `PUSH Sreg`. Reporting it here is what makes the lowering safe, not bookkeeping: the
            // emitted code bakes the selector as a constant, so without this a block whose only
            // mention of DS is `push ds` leaves DS out of `used`, `data_matches` skips it, and the
            // block keeps matching after the guest reloads DS -- pushing the old selector forever.
            // `PUSH CS` (admitted 2026-08-08) is deliberately EXCLUDED by the guard, exactly like
            // `MovSegToReg` above: CS is not in `SegmentLayout.data`, `SegmentLayout::selector`
            // reads the separate `cs` field, and `cs_matches` pins it for every block
            // unconditionally, so reporting it here would be inert at best.
            Self::Push {
                source: StoreSource::Selector(segment),
            }
            | Self::Push16 {
                source: StoreSource::Selector(segment),
            } if segment != SegmentIndex::Cs => Some(segment),
            _ => None,
        }
    }

    /// Every segment whose descriptor this kind BAKES, as a bitmask: the union of the three
    /// accessors above.
    ///
    /// One definition, because the question has three answers and the next reader will only
    /// remember two. `MovSegToReg` is the case that proves it: it bakes DS's selector as a
    /// compile-time constant and reports through `selector_segment` ALONE, with `read_segment` and
    /// `write_segment` both `None`. Anything that asks "does this slot depend on segment S" by
    /// consulting the read and write accessors gets the wrong answer for it, and the wrong answer
    /// is a stale baked value rather than a fault.
    ///
    /// This is deliberately NOT the same question `kind_segment_access_supported` asks. That one
    /// runs over the ACCESSED set, because a selector read asserts nothing about whether memory
    /// can be reached through the segment: folding the two together would refuse to compile
    /// `mov ax, fs` whenever FS is null, which is a legal instruction with a legal answer.
    fn pinned_segments(self) -> u8 {
        let bit = |segment: Option<SegmentIndex>| segment.map_or(0, segment_bit);
        bit(self.read_segment()) | bit(self.write_segment()) | bit(self.selector_segment())
    }

    /// The segment REGISTER this kind overwrites, which is a different question from all three
    /// accessors above and must not be folded into any of them.
    ///
    /// `write_segment` names the segment a slot stores THROUGH, and it feeds
    /// `segment_access_supported(.., write = true)` -- "can this descriptor be written through",
    /// not "is this register being replaced". Reporting the load there would ask the wrong
    /// question and, via `pinned_segments`, drag the segment into `used`, making `data_matches`
    /// compare an entry value the slot never reads and retiring the block on every entry-DS
    /// change. `LoadSegReal` bakes NOTHING, so it belongs in none of the pinned set.
    ///
    /// Consumed only by the compile walk's dirty-segment rule.
    fn written_segment(self) -> Option<SegmentIndex> {
        match self {
            Self::LoadSegReal { segment, .. } => Some(segment),
            _ => None,
        }
    }

    fn write_segment(self) -> Option<SegmentIndex> {
        match self {
            Self::Store { addr, .. }
            | Self::RmwIncDec { addr, .. }
            | Self::DoubleShiftMem { addr, .. } => Some(addr.segment),
            Self::AluMemDest {
                op: 0..=6, addr, ..
            } => Some(addr.segment),
            Self::X87 {
                insn,
                addr: Some(addr),
            } if matches!(
                insn.metadata().memory,
                Some(access) if access.direction == NativeX87MemoryDirection::Write
            ) =>
            {
                Some(addr.segment)
            }
            Self::Push { .. }
            | Self::Push16 { .. }
            | Self::Call { .. }
            | Self::Call16 { .. }
            | Self::CallReg { .. }
            | Self::PushMem { .. }
            | Self::CallMem { .. } => Some(SegmentIndex::Ss),
            _ => None,
        }
    }

    // Routed through the counting accessors rather than re-matching the shape list here: an x87
    // Qword access counts 2 dword reads (or stores) there, and re-deriving that from a separate
    // `matches!` list would let this predicate silently disagree with `dword_reads`/
    // `dword_stores` the moment a future width changes one and not the other. Mirrors
    // `has_word_access` below, which is already written this way.
    fn has_dword_read(self) -> bool {
        self.dword_reads() != 0
    }

    fn has_dword_store(self) -> bool {
        self.dword_stores() != 0
    }

    fn has_word_access(self) -> bool {
        self.word_reads() != 0 || self.word_stores() != 0
    }

    /// `PushMem` being here is LOAD-BEARING rather than bookkeeping. `0xff` is in the
    /// `OperandSize::Word` allowlist in `classify.rs`, so a 66-prefixed `FF /6` in 32-bit code
    /// decodes as Word and reaches the classifier arm. Pushing two bytes while decrementing ESP
    /// by four is a miscompile. What refuses it is the stack-width admission matrix in the
    /// compile loop, and that matrix is only consulted for kinds this predicate accepts.
    fn uses_stack(self) -> bool {
        matches!(
            self,
            Self::Push { .. }
                | Self::Push16 { .. }
                | Self::Pop { .. }
                | Self::Pop16 { .. }
                | Self::Leave
                | Self::Call { .. }
                | Self::Call16 { .. }
                | Self::Ret { .. }
                | Self::Ret16 { .. }
                | Self::PushMem { .. }
                | Self::CallReg { .. }
                | Self::CallMem { .. }
        )
    }

    fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Call { .. }
                | Self::Call16 { .. }
                | Self::Jmp { .. }
                | Self::JmpMem { .. }
                | Self::JmpReg { .. }
                | Self::Ret { .. }
                | Self::Ret16 { .. }
                | Self::Jcc { .. }
                | Self::CallReg { .. }
                | Self::CallMem { .. }
        )
    }

    fn is_x87(self) -> bool {
        matches!(self, Self::X87 { .. })
    }

    fn is_call_out(self) -> bool {
        matches!(self, Self::CallOut { .. })
    }

    fn call_out_helper(self) -> Option<CallOutHelper> {
        match self {
            Self::CallOut { helper } => Some(helper),
            _ => None,
        }
    }

    fn is_memory_alu(self) -> bool {
        matches!(
            self,
            Self::AluMemDest { .. } | Self::DoubleShiftMem { .. } | Self::TestImmMem { .. }
        )
    }
}

const fn segment_bit(segment: SegmentIndex) -> u8 {
    match segment {
        SegmentIndex::Es => 1 << 0,
        SegmentIndex::Cs => 1 << 1,
        SegmentIndex::Ss => 1 << 2,
        SegmentIndex::Ds => 1 << 3,
        SegmentIndex::Fs => 1 << 4,
        SegmentIndex::Gs => 1 << 5,
    }
}

const fn segment_index(segment: SegmentIndex) -> usize {
    match segment {
        SegmentIndex::Es => 0,
        SegmentIndex::Cs => 1,
        SegmentIndex::Ss => 2,
        SegmentIndex::Ds => 3,
        SegmentIndex::Fs => 4,
        SegmentIndex::Gs => 5,
    }
}

mod frame;
pub(crate) use frame::*;
mod table_slots;
pub(crate) use table_slots::*;
// The compile-walk -> emitter handoff structs, moved out for the source-line ceiling.
mod emit_input;
use emit_input::*;

/// Whether this backend supports `prefixes` for an instruction decoded at `operand_size` in a
/// code segment whose default size is `d`.
///
/// Two prefixes are supported: the operand-size override, and an explicit segment override naming
/// one of the five DATA segments. LOCK, REP/REPNE and the address-size override are still refused.
///
/// Whether the operand-size override is present for a given `operand_size` depends on the segment
/// width, because `decode` computes `operand_size = default_32 XOR operand_size_override`. Deriving
/// the expected override from `d` keeps this exact in both widths.
///
/// Under `d == true` this is byte-identical to the hard-coded form it replaced: Dword expects no
/// override, Word expects one. Under `d == false` the mapping INVERTS, and the old form rejected
/// BOTH arms, so every 16-bit instruction was refused here as `PrefixesUnsupported` no matter what
/// the classifier could lower. Nothing 16-bit reaches this today (`key_for` refuses on `!d`), so
/// this is a precondition for that work rather than a behaviour change.
///
/// ## The segment override (rejected-row campaign, slice 6)
///
/// The override needs NO new address machinery, and that is why the admission is a gate change
/// rather than an emitter one. `decode`'s `parse_addressing_mode` folds `segment_override` into
/// `AddrMode.segment` before the instruction leaves the decoder, `classify::direct_addr` copies
/// that field verbatim into `DirectAddr`, and `DirectKind::read_segment` / `write_segment` return
/// `addr.segment` for every memory kind. So the override already selects the base, the limit
/// compare and the `SegmentLayout` pin at all three stages. The four `DirectAddr` producers in the
/// tree are exactly: `direct_addr` (override folded by decode), the `0xa0..=0xa3` moffs arms
/// (which read `segment_override` themselves), and `stack_addr` / `frame_addr` (hard-coded SS,
/// matching the hard-coded `Some(SegmentIndex::Ss)` those kinds return) — so there is no path on
/// which a lowered access can disagree with the interpreter about which segment it uses.
///
/// Census evidence for the admitted set (`.bench/results/rejected-rows-20260802/slice5`): SS
/// carries 19,552,517 doom exits across two rows — 97.63% of doom's whole rejected class — ES 718
/// more across seven, and GS 1,170 on quake. DS and FS measure zero on both fixtures but share the
/// mechanism exactly: the five data segments are handled uniformly by `SEGMENT_ORDER`,
/// `segment_bit`, `SegmentLayout.data` and `segment_access_supported`, with no per-segment arm
/// anywhere, so splitting them would be a distinction the code does not make.
///
/// **CS is refused, explicitly rather than by omission**, and it is the only refusal here that
/// costs measured exits (12,674 doom, on `0xFF /4` `jmp dword [cs:m]`; zero on quake). Two reasons,
/// neither of which applies to the data segments. First, a CS-override WRITE is already
/// unreachable — `segment_access_supported` refuses `write` to a code segment — so admitting CS
/// would admit reads only, which is a narrower mechanism than the one this gate now expresses.
/// Second, CS is the one segment this backend homes TWICE: `SegmentLayout` keeps it in the `cs`
/// field pinned unconditionally by `cs_matches` AND at index 1 of `data`, and
/// `DirectKind::selector_segment` already excludes it deliberately on the strength of that split.
/// A CS-override memory kind would be the first thing to read `data[1]` through `descriptor`, i.e.
/// to depend on the two homes agreeing, for 0.06% of the class. It stays a barrier.
fn prefixes_supported_for(prefixes: Prefixes, operand_size: OperandSize, d: bool) -> bool {
    if prefixes.segment_override == Some(SegmentIndex::Cs) {
        return false;
    }
    prefixes
        == Prefixes {
            operand_size_override: (operand_size == OperandSize::Dword) != d,
            // Carried through rather than defaulted: this is the admission. Every other field
            // stays at its default, so LOCK, REP/REPNE and the address-size override still refuse.
            // The address-size override in particular MUST keep refusing — `MemoryEmitContext`'s
            // `address_wrap` is a BLOCK property derived from CS.D alone, and its doc comment says
            // so; admitting a per-instruction address size would falsify that field for every
            // other slot in the block.
            segment_override: prefixes.segment_override,
            ..Prefixes::default()
        }
}

/// The backend's OWN answer to "may this instruction sit in the middle of a block", for the
/// opcodes where the interpreter's answer (`DecodedInsn::continuable`, decided by
/// `block_continuable` in decode.rs) is not a statement about this backend at all.
///
/// ## Why the two questions differ
///
/// `block_continuable` decides whether the INTERPRETER may chain an instruction into a
/// `run_budgeted` straight-line run. The compile walk has always consulted it as if it were a
/// statement about emittability, which it never was — the rejected-row campaign's Slice 5 census
/// named the arm and found it holding the largest `non_continuable` rows on both fixtures. Its
/// refusals fall into three kinds, and only the first is a property this backend must inherit:
///
///  * **Semantic.** IRET (0xCF), the far transfers (0x9A/0xEA/0xCA/0xCB/0xFF /3 and /5), INT/INTO
///    (0xCC-0xCE) and HLT (0xF4) load CS, dispatch through the IDT or stop the machine. A block
///    cannot run past them and `classify` refuses every one independently, so admitting them here
///    would move a row between census arms and lower nothing.
///  * **Device-visible.** The port forms. A write always sets `io_touched`, so the run must end at
///    the boundary after it; the IN forms are admitted by `block_continuable` itself on the
///    Approximate personas and reach this backend through a call-out slot that reproduces that
///    boundary (`jit/direct/callout.rs`). OUT is a genuine policy refusal and NOT admitted here —
///    see the audit note at the end.
///  * **Classification artifact.** `route_group` sorts three-operand IMUL (0x69/0x6B) into
///    `DecodeGroup::Misc`, the "heterogeneous one-off single-byte block", purely because of the
///    opcode neighbourhood it shares with the BCD adjusts, INS/OUTS, AAM/AAD, SALC/XLAT and HLT.
///    `block_continuable` then refuses the whole group bar TEST AL/AX,imm. Nothing about a signed
///    multiply of a register by an immediate touches a port, changes CS, alters system state or
///    can halt: it is a pure ALU form, strictly simpler than the `DecodeGroup::Alu` members
///    `block_straight_line` admits wholesale, and its own `execute_extended.rs` arm is a read, a
///    multiply and a register write. `block_continuable`'s comment already makes exactly this
///    argument for TEST AL,imm ("a decode-classification artifact of the odd opcode neighborhood
///    they share with the BCD/string/HLT one-offs, not a semantic property"); this is the same
///    claim about the same bucket.
///
/// ## Why the fix lives HERE and not in `block_continuable`
///
/// Admitting 0x69/0x6B upstream would also lengthen the INTERPRETER's straight-line runs, which
/// moves run boundaries, and run boundaries are where the machine services device events. That is
/// a guest-TIMING change on the Approximate personas — legitimate under the state-exact contract,
/// but it would have to be measured against the doom and quake oracles as its own slice. This
/// predicate changes only which instructions a compiled block may contain, so the interpreter's
/// batch structure is byte-identical by construction and the oracles cannot move for this reason.
///
/// ## What this does NOT do
///
/// It does not admit anything. `classify` remains the sole emittability authority: an opcode that
/// passes here and has no classify arm stops the block at `PlannedInsn::HardBoundary` exactly as
/// before, one census arm to the left. The two changes are only useful together, which is why the
/// classify arm and this predicate landed in one commit.
///
/// ## The OUT audit, recorded rather than acted on
///
/// `0xEE` OUT DX,AL is quake's largest `non_continuable` row (1,198,302 static exits) and it is a
/// POLICY refusal, not a semantic one — the call-out mechanism that serves `0xEC` IN would serve
/// it, and every FastMap invalidation clears entries in place rather than reallocating, so the
/// bases a running block baked stay valid across whatever a VGA port write triggers. (As of the
/// aperture-scoping slice a VGA port write no longer triggers `invalidate_all` at all; it triggers
/// `invalidate_vga_pages`, which also clears in place. The conclusion is unchanged, but do not
/// re-open this on the old mechanism's name.)
/// What stops it is not reachable from this predicate: `CpuBus::write_io` reaches every device's
/// write path, and the call-out contract's "no WATCHED guest memory access while a block is live"
/// proof would have to be re-established over all of them, not argued for one. Against that, the
/// benefit is bounded by the same `io_touched` that motivates the interpreter's refusal — a write
/// always sets it, so an admitted OUT would end the native run at the very next boundary and buy
/// only the block's prefix plus its own compilation. It stays refused, and the reason is written
/// down so the next slice re-opens it on the device-write proof rather than on the census rank.
const fn jit_admits_non_continuable(opcode: u16) -> bool {
    matches!(opcode, 0x69 | 0x6b)
}

/// Admission level for 16-bit code segments. **DEFAULT 1 since the 486 measurement**; it used to
/// be 0, and the doc comment used to say "this exists to price a lever, not to ship".
///
///   0  refuse every 16-bit code segment (the old default, still the off switch)
///   1  admit 16-bit (CS.D = 0) code segments backed by ordinary RAM
///   2  additionally admit the 0xC0000..0x100000 option-ROM + BIOS window
///
/// Level 2 deliberately stops at 0xC0000 and leaves 0xA0000..0xC0000 (VGA memory) refused: that
/// half of the window is a device aperture with read side effects, and it is the half the original
/// guard was really about.
///
/// **Level 2 is measured WASTE and should not be used.** On a PoP boot it produces 531 extra
/// compile attempts and ZERO extra installs, because `install`'s page-cover check wants a RAM
/// direct page and ROM is not one. The admission gate and the installer disagree about the same
/// window. Fix or retire it; do not set it hoping for BIOS coverage.
///
/// What flipping the default to 1 buys and costs, measured on a quiet box, min-of-N:
///
///   * PoP-486, a real-mode game: coverage 1.03% -> 74.47%, 9.68 native insns/entry, wall NEUTRAL,
///     framebuffer bit-identical over 4e9 cycles.
///   * quake-586: **+4.14% slower**. Its 16-bit code is 55% of entries at 2.431 insns/entry, i.e.
///     DOS/BIOS/extender glue in blocks too short to amortise a dispatcher entry.
///
/// That split is a WORKLOAD SHAPE, not a persona: real-mode game loops win, a 32-bit game's 16-bit
/// glue loses.
///
/// Defaulted ON at parity deliberately, and the reasoning is pre-release reasoning: there is no
/// version out, so a default is a development posture rather than a promise to anyone. On costs a
/// measured 4% on one workload and buys exposure of the 16-bit path to every fixture, every gate
/// run and every future slice, which is how the remaining coverage work gets found and how each
/// lowering lands as upside instead of paying down a deficit. Revisit the trade before a release,
/// not before then. Closing the quake gap is the next objective.
pub(crate) fn sixteen_bit_admission_level() -> u8 {
    static LEVEL: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *LEVEL.get_or_init(|| {
        std::env::var("IZARRAVM_JIT16")
            .ok()
            .and_then(|value| value.parse::<u8>().ok())
            .unwrap_or(1)
    })
}

/// Whether a hot SMC chunk may spend one compile-through-heat "lane trial" per key per heat
/// epoch (`IZARRAVM_SMC_LANE_TRIAL`, ON for every value except exactly "0"). Per KEY, not per
/// chunk, deliberately: N entry points inside one 16-byte chunk buy N trials per epoch, which
/// stays bounded and lets each entry's own lane coverage decide its fate.
///
/// DEFAULT ON SINCE THE DISP LANES LANDED, and the flip is the same measurement that once
/// turned it off, repeated on the other side of its stated precondition. 2026-08-08
/// (duke586-lanetrial-{0,1}.json), imm lanes only: 146,956 trials, 61,216 installs, rt -5.5%
/// (0.2600 -> 0.2456) — Build's patch bursts mix lane-shaped 0x81 writes with disp-field 0x8A
/// rewrites in the same chunks, so trial installs died to the writes the lanes could not
/// absorb, and the doc said "re-measure after displacement lanes exist". 2026-08-09
/// (duke586-displane3-{0,1,trial}.json), heat-gated disp lanes shipped: trial-off is INERT on
/// duke3d-586 (rt 0.2443 vs 0.2445 off-arm, because the kill that writes a disp lane's heat
/// record also heats the chunk toward this very gate, so the laned recompile mostly never
/// installs), and trial-on is rt 0.2801, +14.6%, with 446,503 disp lanes registered and
/// narrow kills down 0.9M. The lanes and the trial are ONE mechanism: the trial is how a laned
/// block gets past the heat gate, the lanes are why its install survives. doom-486/586 take
/// ZERO disp lanes (heat-gated admission) and measured neutral in the same sitting.
///
/// WHY THE TRIAL EXISTS. G1's admission gates and the mutable-lane mechanism deadlock against
/// each other on a fixture whose patch loop never pauses: the gate refuses to compile while the
/// chunk is hot, so no block exists, so no lanes register, so every patch narrow-kills decode
/// lines and re-stamps the heat, forever. Duke3d spends 44.8% of its dispatcher seams exiting
/// into exactly this state (dev_docs/2026-08-08-dispatch-tier-next.md). Doom never hit the
/// deadlock only because its blocks compiled BEFORE the heat crossed the threshold.
///
/// The trial breaks the cycle with a bounded probe: one compilation per key per heat epoch is
/// allowed THROUGH the hot gate; it installs only if it registered at least one mutable lane.
/// From there the mechanism self-selects. If the lanes cover the guest's patches, the writes
/// become `lane_accepts`, contribute no heat, the chunk cools at the next epoch, and admission
/// normalizes. If they do not, the next patch kills the block, the key re-parks Dormant exactly
/// as before, and the trial cannot re-fire until the epoch turns — worst case one extra compile
/// and install per key per epoch.
/// Whether `imm_lane_for` admits the whole `0x81 /r` reg dword family (`IZARRAVM_LANE_FAMILY`,
/// on for every value except exactly "0") or only the original `/0 ADD` shape. The off arm
/// exists for one-binary A/B measurement, the same contract as the JIT16 pair: both arms ship
/// in one executable so a comparison carries no build-to-build variance.
pub(crate) fn lane_family_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| !matches!(std::env::var("IZARRAVM_LANE_FAMILY").as_deref(), Ok("0")))
}

pub(crate) fn lane_trial_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| !matches!(std::env::var("IZARRAVM_SMC_LANE_TRIAL").as_deref(), Ok("0")))
}

/// Whether `disp_lane_for` admits the `0x8A` displacement-lane family (`IZARRAVM_DISP_LANES`,
/// on for every value except exactly "0"). The off arm exists for one-binary A/B measurement,
/// the same contract as `IZARRAVM_LANE_FAMILY`.
pub(crate) fn disp_lanes_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| !matches!(std::env::var("IZARRAVM_DISP_LANES").as_deref(), Ok("0")))
}

/// Whether `classify` admits the 2026-08-09 group-2 rows -- `0xC1`/`0xD1` **`/0` ROL** and
/// **`0xC0 /4` SHL r8**, both register forms.
///
/// **DEFAULT OFF, MEASURED NET-NEGATIVE.** `IZARRAVM_ROTATE_ROWS` must be SET, to anything other
/// than exactly "0", to admit them. The pre-flip `IZARRAVM_SMC_LANE_TRIAL` contract: the mechanism,
/// its tests and its mutation record all ship, switched off, because the thing that refuted it is
/// a property of one fixture's SMC behaviour rather than of the lowering.
///
/// WHY IT EXISTS. The duke3d-586 re-census (`.bench/results/duke586-census-20260809.json`) ranks
/// `0xC1 /0` first by BOTH currencies -- 260,659,304 runtime hits, the hottest interpreted
/// instruction in the trace, and 111,123,374 static unbound exits, the largest refused-row seam --
/// with `0xC0 /4` second at 32,839,852 and 31,743,121. On paper this is the top of the list.
///
/// WHY IT IS OFF. Interleaved A/B/B/A on duke3d-586, one binary, quiet box. Off arm rt 0.3298
/// (legs 0.3283, 0.3313); on arm rt 0.3184 (legs 0.3111, 0.3257). **Delta -3.44%**, and native
/// coverage DROPPED on the admitting arm, 0.7480 -> 0.7264. Admitting the hottest row made the
/// backend cover LESS.
///
/// THE MECHANISM, which the counters name outright: `smc_lane_accepts` collapsed 55.57M -> 25.45M,
/// narrow kills rose 45.25M -> 49.55M, `heat_hot` 357k -> 373k. Duke patches the COUNT BYTE of its
/// group-2 shifts (the SMC shape table's `0xC1 /0,/4,/5` `imm_len=1` rows, ~1.9M events). Before
/// the slice those ROLs were hard boundaries, so no compiled block ever spanned the patched byte
/// and the patch cost nothing. After it, blocks span the byte -- and each 1-byte count patch now
/// kills a block that ALSO carries live `0x81` imm lanes and `0x8A` displacement lanes, taking
/// their accepts down with it. That is the lane-trial iteration-1 mixing failure reborn one level
/// up: not lane-shaped writes mixed with unlaned ones inside a chunk, but a lane-shaped BLOCK
/// extended across a patch shape no lane class covers. The lowering is correct at every count and
/// every flag; the ADMISSION is net-negative on the only fixture that carries the runtime mass.
///
/// **RE-TEST TRIGGER: a one-byte mutable-imm lane class covering the `imm_len=1` patch shapes
/// (`0xC1`, `0xC0`, `0x80`).** `IMM_LANE_WIDTH` is four and `imm_lane_for`'s accept rule is written
/// against that width, so a 1-byte lane is its own width class rather than a widened match. Once
/// duke's count-byte patches become `lane_accepts` instead of narrow kills, this A/B is measuring
/// something different and must be run again.
///
/// THE DESIGN COST THE NEXT SLICE MUST BUDGET FOR, because it is not a lane-plumbing detail. A
/// laned count is loaded at RUNTIME, and `emit_rotate_reg`'s whole correctness argument is a
/// COMPILE-TIME split on the count: 0 emits nothing, 1 captures `CF|OF` and publishes the shadow,
/// 2..31 captures CF alone and goes through `emit_set_cf_only`. A runtime count cannot pick a
/// capture mask at emission, so the lane form is forced onto the CL-shaped emission whose flag
/// update is runtime-conditional -- and the count-0 case is not "some flags" but "no flag moves
/// and no descriptor is created or destroyed", which a conservative publish gets WRONG rather than
/// approximately right. So the lane-form rotate needs either a genuinely conditional runtime flag
/// path (the three-way branch `emit_shift_cl` already declined) or a guard that admits the lane
/// only when the patched count byte's value range excludes 0 and 1. Price that before pricing the
/// lane.
///
/// THE ALTERNATIVE WORTH PRICING FIRST, because it may not need the lane work at all: admit
/// `0xC1 /0` **only at sites whose count byte has no heat record** -- the disp-lane heat gate
/// INVERTED, admitting never-patched sites instead of hot ones. See `disp_lane_for` for the
/// pattern and for how a heat record is probed at classify time. Duke's ~1.9M count-byte patch
/// events are concentrated on a small number of sites; the 260M runtime hits are not necessarily
/// on the same ones, and the unpatched share is exactly the part of the row that carries no
/// block-kill risk. If that share is most of the mass, this is a much smaller slice than the lane.
///
/// **Read at the CLASSIFY admission point, not at emission**, so the off arm reproduces the
/// pre-slice refusal exactly: `classify` returns None, the compile walk breaks, and the row lands
/// back in the census as an ordinary `hard_boundary` unbound exit rather than as some new refusal
/// kind that would not be comparable with the census this slice was ranked against. That is also
/// what makes the shipped default a true pre-slice world and not merely a quiet one.
///
/// **The knob covers THIS SLICE ONLY.** `0xC1 /1` and `0xD1 /1` ROR at Dword were lowered by the
/// 2026-07-26 slice and are deliberately outside it; so are `/4..=7`. Sweeping them in would make
/// every future A/B price two slices as one.
///
/// Both arms ship in one executable, the `IZARRAVM_LANE_FAMILY` and `IZARRAVM_DISP_LANES` contract:
/// this box has measured 6% wall variance between builds of identical source, which is larger than
/// the effect, so a cross-build comparison would not be evidence.
pub(crate) fn rotate_rows_enabled() -> bool {
    #[cfg(test)]
    if let Some(forced) = ROTATE_ROWS_OVERRIDE.with(std::cell::Cell::get) {
        return forced;
    }
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(
        || matches!(std::env::var("IZARRAVM_ROTATE_ROWS").as_deref(), Ok(value) if value != "0"),
    )
}

// Per-THREAD, because the shipped knob is a process-wide `OnceLock` and the fixtures have to run
// both arms in one process. Thread-local rather than a global is what keeps the parallel test
// harness honest: one test's arm selection cannot reach another's compile.
//
// Since the flip to default-OFF this is not a convenience: every positive fixture for these two
// rows MUST force the on arm through it, or it would test the refusal and call it a lowering.
#[cfg(test)]
thread_local! {
    static ROTATE_ROWS_OVERRIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Force the group-2 admission arm on this thread for the length of a fixture; `None` restores the
/// ambient `IZARRAVM_ROTATE_ROWS` reading.
#[cfg(test)]
pub(crate) fn set_rotate_rows_for_test(forced: Option<bool>) {
    ROTATE_ROWS_OVERRIDE.with(|cell| cell.set(forced));
}

/// Seed for `JitState::word_at_486`, read once per process from `IZARRAVM_JIT16_486`.
///
/// **DEFAULT ON since the 486 measurement.** Set `IZARRAVM_JIT16_486=0` to refuse.
///
/// Separate from `IZARRAVM_JIT16` on purpose: that one selects WHICH memory a 16-bit code segment
/// may live in, and this one selects WHICH PERSONAS lower Word operands at all. They compose, and
/// keeping them independent is what let the two halves be measured apart — an
/// `IZARRAVM_JIT16=0` arm isolates this flag's 32-bit half (66-prefixed word ops) exactly, because
/// `try_direct_continuation` then refuses every 16-bit boundary before a key is built.
///
/// The design that introduced this said to DELETE the knob when the default flipped, so a
/// temporary switch could not become permanent surface. It stays, deliberately, for two reasons
/// the design did not know yet: the flip ships a measured ~4% regression on quake-586, so an
/// escape hatch is worth its surface until coverage work closes that; and the differential tests
/// that cover the refusing arm at I486 have no other way to reach it once the lift is
/// unconditional. Delete it when quake-586 is back at parity, not before.
pub(crate) fn word_at_486_default() -> bool {
    static LEVEL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *LEVEL.get_or_init(|| !matches!(std::env::var("IZARRAVM_JIT16_486").as_deref(), Ok("0")))
}

/// Whether the Direct backend lowers `OperandSize::Word` operands on this CPU.
///
/// ONE predicate for what used to be three copies of `persona != I586`: the compile walk's Word
/// refusal, `key_for_phys`'s 16-bit-segment refusal, and the census suffix scan's copy of the
/// first. They have to move together. The compile walk and `key_for_phys` are COUPLED by
/// construction — `key_for_phys` refuses the key precisely BECAUSE the walk would reject the first
/// slot and install a rejected span for zero yield — so lifting either alone is wrong in a
/// different direction: the walk alone is inert, the key alone is pure churn. The census copy is
/// the one that would silently re-open a seventh divergence between the two walks.
///
/// The 16-bit half rests on an identity worth stating, because it is what lets one predicate serve
/// both questions: `operand_size` follows CS.D opcode-independently, so in a CS.D = 0 segment
/// EVERY instruction decodes at `Word`. "May a 16-bit segment be keyed" and "are Word operands
/// admitted" are therefore the same question asked at two points.
///
/// The admitted set is I486 and I586, never the 386 class. Interpreted 386 already runs above 15x
/// real time, so there is no throughput problem for the JIT to solve there, and admitting it would
/// widen the blast radius of every Word lowering to a persona nobody benchmarks. `key_for_phys`
/// already refuses every persona below I486 a few lines up, so the 386 class is doubly excluded;
/// spelling it out here means a future 386 enablement cannot silently inherit Word admission.
///
/// Be honest about that arm: it is UNREACHABLE today and therefore UNTESTABLE. Flipping
/// `I386 => false` to `true` fails nothing, because `key_for_phys`'s own persona check runs first
/// and the compile walk is reached only through it. It is defence in depth against a future edit
/// to that check, not a live guard, and no test should be written that pretends otherwise.
fn word_operands_admitted(cpu: &CpuGsw) -> bool {
    match cpu.persona() {
        CpuPersona::I586 => true,
        CpuPersona::I486 => cpu.jit_direct.word_at_486,
        CpuPersona::I386 => false,
    }
}

/// May the Direct backend key a block at all, on this host and this persona? The screen
/// `key_for_phys` opened with, lifted to a function of `mode` alone so `CpuGsw` can cache it
/// (`JitState::native_keys_admitted`) and so the cache and the thing it caches are ONE expression
/// — the same discipline `fast_map_population_enabled` and its serve gate keep.
///
/// This does NOT subsume `word_operands_admitted`, and the two must not be merged. That predicate
/// answers a per-BLOCK question (does this segment's operand size survive the compile walk?) and
/// its coupling contract with the compile walk is documented on it; this one answers a per-CPU
/// question that the walk never re-asks. `key_for_phys` still consults both, in that order, so
/// the two walks keep answering identically.
pub(crate) fn native_keys_admitted(mode: GswMode) -> bool {
    super::host_supported() && matches!(mode.persona(), CpuPersona::I486 | CpuPersona::I586)
}

pub(crate) fn key_for(cpu: &CpuGsw, lin: u32, d: bool) -> Option<BlockKey> {
    let physical = cpu.decode_cache.line_phys_start(lin, d)?;
    key_for_phys(cpu, lin, d, physical)
}

/// `key_for` for a caller that already holds the line's physical start (a `DecodeLineView` taken
/// this iteration). Identical decision: the only thing `key_for` reads off the decode cache is
/// that one field, and `line_phys_start` would return exactly this value for the same key.
pub(crate) fn key_for_phys(cpu: &CpuGsw, lin: u32, d: bool, physical: u32) -> Option<BlockKey> {
    // Hoisted screen, read from the cache instead of recomputed. See
    // `JitState::native_keys_admitted` for why the cached answer cannot be stale; the assert
    // below is the enforcement, not the argument.
    debug_assert_eq!(
        cpu.jit_direct.native_keys_admitted,
        native_keys_admitted(cpu.mode()),
        "native_keys_admitted cache is stale relative to the host/persona screen; a mode mutator \
         is missing a refresh_native_key_admission() call"
    );
    if !cpu.jit_direct.native_keys_admitted {
        return None;
    }
    // A 16-bit code segment is admitted wherever `word_operands_admitted` says Word operands are
    // lowered, which since the 486 measurement is I486 and I586 BY DEFAULT. Every instruction in
    // such a segment decodes at `OperandSize::Word` (the size follows CS.D, not the opcode), so
    // where the policy refuses, the whole population would reach `classify`, fail on its FIRST
    // slot, and install a rejected span plus a physical-page watch for every hot 16-bit boundary.
    // Refusing the key here instead keeps that persona byte-identical by construction.
    //
    // The 16-bit population is real mode, V86 and 16-bit protected mode. V86 is deliberately IN,
    // and 16-bit V86 BLOCKS EXIST in the shipped configuration -- an earlier revision of this
    // comment said "no 16-bit block exists on any persona today", which was true while
    // `try_direct_continuation` refused every `!d` boundary and stopped being true when
    // `IZARRAVM_JIT16` defaulted to 1. The V86 conclusion now rests on per-opcode gates:
    //
    //   * The PORT opcodes (0xEC and family): two gates, either sufficient. `classify`'s
    //     Word-size allowlist excludes them, so in a CS.D = 0 segment they stay barriers; and
    //     `run_direct_block` refuses to ENTER a call-out-bearing block whenever
    //     `is_v86_mode() || CPL > IOPL`, so the 0xEC call-out slot cannot execute in V86 even
    //     compiled into a 32-bit block.
    //   * PUSHF: its PUSHFD arm is refused by `stack_width_kind` in V86 (`StoreSource::Flags`,
    //     IOPL check), and its Word form is off the allowlist.
    //   * POPF, CLI, STI, INT, IRET: no `classify` arm at any size. That absence is now PINNED by
    //     `v86_sensitive_opcodes_stay_word_barriers` (cpu_jit_compile_outcome_test.rs), because
    //     an absence defended by nothing is exactly what a coverage campaign widens by accident.
    //
    // V86 blocks stay key-separated by mode-key bit 2.
    if !d && !word_operands_admitted(cpu) {
        return None;
    }
    if lin.wrapping_sub(0x000f_f000) < 0x400 {
        return None;
    }
    // The first direct slice has no page-kind guard in emitted code. Keep video and ROM code on
    // the interpreter until the shared fast map can prove a page is ordinary RAM.
    //
    // The spike's level 2 lifts the ROM half of that window only (see
    // `sixteen_bit_admission_level`): 0xC0000 and up is option ROM and the system BIOS, which is
    // read-only storage with no side effects, while 0xA0000..0xC0000 is the VGA aperture the
    // guard is really for and stays refused at every level.
    if (0x000a_0000..0x0010_0000).contains(&physical)
        && !(physical >= 0x000c_0000 && cpu.jit_direct.sixteen_bit_level >= 2)
    {
        return None;
    }
    Some(BlockKey::new(lin, physical, cpu.jit_mode_key()))
}

pub(crate) fn compile(cpu: &mut CpuGsw, entry_lin: u32, d: bool) -> CompileOutcome {
    compile_with_page_len(cpu, entry_lin, d, super::exec_mem::host_page_len())
}

fn compile_with_page_len(
    cpu: &mut CpuGsw,
    entry_lin: u32,
    d: bool,
    page_len: usize,
) -> CompileOutcome {
    let full = match compile_with_instruction_limit(cpu, entry_lin, d, MAX_BLOCK_INSTRUCTIONS) {
        CompileOutcome::Compiled(compilation) => compilation,
        other => return other,
    };
    if full.code.len() <= page_len {
        return CompileOutcome::Compiled(full);
    }

    // Shorter candidates use the same fallthrough exit, so emitted size increases with the
    // instruction count. Find the longest prefix that fits one arena page. Two-instruction
    // nonterminal prefixes remain interpreter-only.
    let mut lower = 3usize;
    let mut upper = usize::from(full.span.instructions).saturating_sub(1);
    let mut best = None;
    while lower <= upper {
        let midpoint = lower + (upper - lower) / 2;
        let candidate = match compile_with_instruction_limit(cpu, entry_lin, d, midpoint) {
            CompileOutcome::Compiled(compilation) => compilation,
            CompileOutcome::StructuralReject(_) | CompileOutcome::Retry => {
                return CompileOutcome::Retry;
            }
        };
        if candidate.code.len() <= page_len {
            best = Some(candidate);
            lower = midpoint + 1;
        } else {
            upper = midpoint - 1;
        }
    }
    best.map_or(CompileOutcome::Retry, CompileOutcome::Compiled)
}

#[cfg(test)]
pub(crate) fn compile_with_page_len_for_test(
    cpu: &mut CpuGsw,
    entry_lin: u32,
    d: bool,
    page_len: usize,
) -> CompileOutcome {
    compile_with_page_len(cpu, entry_lin, d, page_len)
}

#[derive(Clone, Copy)]
enum CompileStop {
    Structural(RejectedSpan),
    Retry,
    Boundary,
}

#[derive(Clone, Copy)]
enum PlannedInsn {
    Native(DirectKind),
    HardBoundary,
}

struct DirectUnitPlanner;

impl DirectUnitPlanner {
    fn classify(insn: &DecodedInsn, lin: u32, entry_lin: u32) -> PlannedInsn {
        match classify::classify(insn, lin, entry_lin) {
            Some(kind) => PlannedInsn::Native(kind),
            None => PlannedInsn::HardBoundary,
        }
    }
}

/// What PUSHFD actually stores: the low 16 flags always, plus the persona's writable high bits.
/// RF and VM are masked to zero by construction because they are outside this set. Mirrors the
/// interpreter's 0x9C arm exactly.
fn pushf_mask(persona: CpuPersona) -> u32 {
    let high = match persona {
        CpuPersona::I386 => 0,
        CpuPersona::I486 => crate::FLAG_AC,
        CpuPersona::I586 => crate::FLAG_AC | crate::FLAG_ID,
    };
    0xffff | high
}

fn stack_width_kind(
    cpu: &CpuGsw,
    kind: DirectKind,
    operand_size: OperandSize,
) -> Option<DirectKind> {
    // PUSHFD checks IOPL in V86 and can raise #GP there (`check_v86_iopl`, execute.rs 0x9C).
    // The emitted form has no fault path, so refuse it outright when compiling in V86. That is
    // sound rather than merely cautious: V86 is bit 2 of `jit_mode_key`, so a block compiled
    // outside V86 can never later run inside it -- the entry mode-key check rejects first.
    //
    // The persona mask is resolved HERE for the same reason: `classify` has no `&CpuGsw`.
    let kind = match kind {
        DirectKind::Push {
            source: StoreSource::Flags { .. },
        } => {
            if cpu.is_v86_mode() {
                return None;
            }
            DirectKind::Push {
                source: StoreSource::Flags {
                    mask: pushf_mask(cpu.persona()),
                },
            }
        }
        // The segment load is emitted as `base = selector << 4` with no descriptor fetch, which is
        // what a segment load IS in real mode and V86 (`load_segment_real`) and is nothing like
        // what it is in protected mode: a GDT/LDT fetch with type, privilege and present checks
        // that can raise #GP or #NP (`load_protected_segment`). Admitting one in protected mode
        // would compute a real-mode base for a descriptor-table segment and skip every check.
        //
        // The refusal has to be HERE and not in the mode key. The key stops a block compiled in
        // one mode from being ENTERED in another; it says nothing about which mode the block was
        // compiled in. What the key then adds is that this refusal is sufficient rather than
        // merely necessary: a block admitted under real mode can never later run under protected.
        // V86 is admitted deliberately -- `.bench/prince_c` runs V86 under JEMMEX, and V86 takes
        // the real-mode path in `load_segment_checked` before the protected-mode branch is even
        // considered.
        DirectKind::LoadSegReal { .. } if cpu.is_protected_mode() && !cpu.is_v86_mode() => {
            return None;
        }
        other => other,
    };
    match (kind, cpu.stack_is_32bit(), operand_size) {
        (kind, _, _) if !kind.uses_stack() => Some(kind),
        (kind, true, OperandSize::Dword) => Some(kind),
        (DirectKind::Push { source }, false, OperandSize::Word) => {
            Some(DirectKind::Push16 { source })
        }
        (DirectKind::Pop { dst }, false, OperandSize::Word) => Some(DirectKind::Pop16 { dst }),
        (DirectKind::Ret { release }, false, OperandSize::Word) => {
            Some(DirectKind::Ret16 { release })
        }
        (
            DirectKind::Call {
                return_delta,
                target_delta,
            },
            false,
            OperandSize::Word,
        ) => Some(DirectKind::Call16 {
            return_delta,
            target_delta,
        }),
        _ => None,
    }
}

/// Admit a mutable imm32 lane for one slot, or refuse.
///
/// The admitted shape is the register-destination `0x81 /r` family (every ALU-group member, ADD
/// through CMP) and the checks are deliberately over-determined, each pinning a different
/// property of the encoding:
///
/// - `opcode == 0x81` with a `DirectKind::AluImm` kind: the ALU group with a 32-bit immediate,
///   and `AluImm` is produced only from `DecodedOperand::Reg`, so the ModRM mode is 3.
/// - `op` is carried through unchanged: the lane emit arm feeds the SAME `emit_alu_preloaded`
///   dispatch the baked form uses, so every group member emits its own correct operation (see
///   the op-binding comment in the body).
/// - `OperandSize::Dword` plus `Prefixes::default()`: no operand-size override, no address-size
///   override, no segment override, no REP, and no LOCK. A LOCK'd patch is refused here rather
///   than relied on being impossible.
/// - `disp_len == 0`, `imm_len == 4`, `len == 6`: the decoder's own record of what it consumed.
///   Together these are what puts the immediate at instruction offset 2 and nowhere else, so the
///   lane address is `physical + 2` by construction rather than by assumption about the encoding.
///
/// The lane is refused (and the slot keeps its baked immediate, correct as ever) when the block
/// already holds `MAX_BLOCK_IMM_LANES`, or when no direct page can supply a host pointer for the
/// immediate's bytes. The second is the page-kind guard: only a page the bus hands out as a direct
/// mapping can be read this way, so device apertures and unmapped pages never produce a lane.
fn imm_lane_for(
    cpu: &CpuGsw,
    insn: &DecodedInsn,
    kind: DirectKind,
    physical: u32,
    lanes_used: usize,
) -> Option<(DirectKind, ImmLane)> {
    if lanes_used >= MAX_BLOCK_IMM_LANES {
        return None;
    }
    // The family A/B arm: `op != 0` shapes are the 2026-08-08 widening, refusable at runtime so
    // one binary measures both arms (see `lane_family_enabled`).
    if !lane_family_enabled()
        && !matches!(
            kind,
            DirectKind::AluImm {
                op: 0,
                width: MemoryWidth::Dword,
                ..
            }
        )
    {
        return None;
    }
    // `width: MemoryWidth::Dword` is matched rather than ignored. `IMM_LANE_WIDTH` is four and the
    // lane patches the field whole; a Word `AluImm` reads only two of those bytes, so admitting one
    // here would name a four-byte lane for a two-byte read. The `OperandSize::Dword` test below
    // already implies it for every kind `classify` produces, which is exactly why matching it here
    // costs nothing and closes the hole if that ever stops being true.
    //
    // `op` is bound rather than matched: every ALU op the kind carries is lane-safe, because the
    // lane emit arm routes through the SAME `emit_alu_preloaded` the baked form uses, and that
    // helper already dispatches the whole op set at Dword — carry ops (/2 ADC, /3 SBB) to
    // `emit_carry_alu_preloaded`, CMP (/7) to the non-writing path. The original `op: 0` match
    // was Doom's shape (its renderer patches `ADD r32, imm32` immediates); the duke3d SMC shape
    // census (duke586-smc-shapes-20260808.txt) measured 31.7M of its 37.2M imm-field patch events
    // on `0x81 /3, /5, /2, /0` — same kind, other ops — so the narrow match was leaving 85% of
    // the lane-shaped patch volume killing blocks.
    let DirectKind::AluImm {
        op,
        dst,
        imm,
        width: MemoryWidth::Dword,
        ..
    } = kind
    else {
        return None;
    };
    if insn.opcode != 0x81
        || insn.operand_size != OperandSize::Dword
        || insn.prefixes != Prefixes::default()
        || insn.disp_len != 0
        || insn.imm_len != 4
        || insn.len != 6
    {
        return None;
    }
    let lane = physical.checked_add(2)?;
    // The instruction is already known page-local in physical (`physical_page_local` in the
    // compile loop), so its immediate cannot straddle a page and one host pointer covers all four
    // bytes.
    let host = cpu.direct_host_bytes(lane, IMM_LANE_WIDTH)?;
    let lane = ImmLane {
        physical: lane,
        host,
    };
    Some((
        DirectKind::AluImm {
            op,
            dst,
            imm,
            lane: Some(lane),
            width: MemoryWidth::Dword,
        },
        lane,
    ))
}

/// The displacement twin of `imm_lane_for`: `0x8A MOV r8, [..disp32..]`, every ModRM memory
/// form, no prefixes — GATED ON MEASURED PATCH HISTORY. The admitted field is the
/// instruction's disp32, which duke3d-586's SMC trace measured at 17M of its 19.3M disp-patch
/// events (dev_docs/2026-08-09-disp-lanes-design.md); each one today either kills the covering
/// block or keeps its chunk's G1 heat stamped.
///
/// THE HEAT GATE IS THE SLICE'S LOAD-BEARING DECISION, and it was reached by refutation twice
/// over. The lane form costs two host instructions per EXECUTION whether or not the field is
/// ever patched. Iteration 1 admitted the whole family unconditionally: duke +8.2%, but the
/// 2026-08-09 formal gate FAILED — doom-486 paired RTF 0.978, doom-586 0.975 — because doom's
/// renderer executes `[base+disp32]` texture/colormap byte loads constantly and patches none
/// of them. Iteration 2 tried the shape cut (bare `[disp32]` only): doom recovered but duke's
/// win VANISHED (rt 0.2706 vs 0.2697, 3.4k lanes vs 233k) — Build patches the indexed forms
/// too, so no static shape separates the populations. What separates them is BEHAVIOR:
/// `SmcHeatMap::has_record_range` over the disp field's bytes is true only after the field
/// took a heat-charged kill, so a never-patched load compiles baked and untaxed forever, and a
/// patched one converges to the lane form one kill after its first patch (the kill bumps the
/// record, the recompile sees it). Lane-absorbed patches deliberately do not refresh records,
/// and a record consumed by `lift_cold_smc_dormant` recovery self-heals the same way: one more
/// kill, one more recompile.
///
/// The probe reads the heat accelerator WITHOUT `sync_smc_heat` (this is a `&CpuGsw` path); a
/// stale read across a cache reset can at worst bake one block that a later recompile lanes,
/// or lane one block that did not need it — admission tuning, never correctness.
///
/// `disp_len == 4` plus the default-prefix test confines this to 32-bit addressing: a CS.D=0
/// segment cannot reach a four-byte displacement without a `0x67` prefix, so a lane and
/// `AddressWrap::Word` can never co-occur and the loaded field needs no sign-extension — the
/// four guest bytes ARE the architectural displacement. With `imm_len == 0` those bytes are
/// the instruction's last four, so the lane start is `physical + len - 4` (offset 2 on the
/// mod-0 rm-5 form, 3 on the SIB forms, more under mod 2 — the SIB fixture pins this).
///
/// Only `DirectKind::Load` may carry a lane, and that is a REGISTER-PRESSURE contract, not
/// taste: the lane arm of `emit_effective_address` stages the displacement through EAX alone,
/// which is safe for every caller, but widening admission to a kind whose emitter resolves the
/// address AFTER staging other live state would still deserve its own review — and its own
/// census row, per the standing rule against unmeasured admissions.
fn disp_lane_for(
    cpu: &CpuGsw,
    insn: &DecodedInsn,
    kind: DirectKind,
    physical: u32,
    lanes_used: usize,
) -> Option<(DirectKind, ImmLane)> {
    if lanes_used >= MAX_BLOCK_IMM_LANES || !disp_lanes_enabled() {
        return None;
    }
    let DirectKind::Load {
        dst,
        width,
        addr,
        raw_clocks,
    } = kind
    else {
        return None;
    };
    if insn.opcode != 0x8a
        || insn.prefixes != Prefixes::default()
        || insn.disp_len != 4
        || insn.imm_len != 0
    {
        return None;
    }
    let lane = physical.checked_add(u32::from(insn.len).checked_sub(4)?)?;
    if !cpu
        .jit_direct
        .smc_heat
        .has_record_range(lane, IMM_LANE_WIDTH)
    {
        return None;
    }
    // Page-local in physical for the same reason as `imm_lane_for`: the compile loop only
    // reaches this after `physical_page_local`, so one host pointer covers all four bytes.
    let host = cpu.direct_host_bytes(lane, IMM_LANE_WIDTH)?;
    let lane = ImmLane {
        physical: lane,
        host,
    };
    Some((
        DirectKind::Load {
            dst,
            width,
            addr: DirectAddr {
                disp_lane: Some(lane),
                ..addr
            },
            raw_clocks,
        },
        lane,
    ))
}

fn compile_with_instruction_limit(
    cpu: &mut CpuGsw,
    entry_lin: u32,
    d: bool,
    instruction_limit: usize,
) -> CompileOutcome {
    let Some(key) = key_for(cpu, entry_lin, d) else {
        return CompileOutcome::Retry;
    };
    // B.3: "a compile walk started here", recorded before anything can refuse the block, so a walk
    // that stops on its first slot still counts as tried. `key.linear()` and not `entry_lin` on
    // purpose -- `classify_unbound_exit` reports the same canonicalized value, and the whole point
    // of this map is that it joins against the dormant-heat histogram on an identical key.
    #[cfg(feature = "barrier-census-closure")]
    if cpu.jit_direct.barrier_census_active() {
        cpu.jit_direct
            .note_lane_probe(key.linear(), census::lane_probe::WALKED);
    }
    let cs = cpu.registers.cs();
    let entry_eip = entry_lin.wrapping_sub(cs.base);
    let mut slots = Vec::with_capacity(MAX_BLOCK_INSTRUCTIONS);
    let mut fetch_lens = [0u8; MAX_BLOCK_INSTRUCTIONS];
    let mut lin = entry_lin;
    let mut raw_clocks = 0u32;
    let mut weighted_fp_clocks = 0u32;
    let mut byte_reads = 0u8;
    let mut word_reads = 0u8;
    let mut dword_reads = 0u8;
    let mut byte_stores = 0u8;
    let mut word_stores = 0u8;
    let mut dword_stores = 0u8;
    let mut read_segments = 0u8;
    let mut write_segments = 0u8;
    let mut pinned_segments = 0u8;
    // Segments this block has already overwritten, and how many such writes it carries. The mask
    // ends slots that would bake a stale value; the count bars the two block shapes that would
    // re-enter or leave the block without a segment check (see `self_loop` and `successors`).
    let mut dirty_segments = 0u8;
    let mut segment_writes = 0usize;
    let mut has_wide_accesses = false;
    let mut stack_accesses = 0u8;
    let mut x87_slots = 0u8;
    let mut callout_slots = 0u8;
    let mut callout_port_slots = 0u8;
    let mut callout_memory_slots = 0u8;
    let x87_entry_top = cpu.fpu.top();
    let mut x87_exit_top = x87_entry_top;
    let mut memory_alu_slots = 0u8;
    let mut imm_lanes = [NO_IMM_LANE; MAX_BLOCK_IMM_LANES];
    let mut imm_lane_count = 0usize;
    let mut disp_lane_count = 0u8;
    let mut stop = CompileStop::Boundary;

    while slots.len() < instruction_limit.min(MAX_BLOCK_INSTRUCTIONS) {
        if x87_slots != 0 && slots.len() == MAX_X87_BLOCK_INSTRUCTIONS {
            break;
        }
        if memory_alu_slots != 0 && slots.len() == MAX_MEMORY_ALU_BLOCK_INSTRUCTIONS {
            break;
        }
        let Some(insn) = cpu.decode_cache.get(lin, d) else {
            stop = CompileStop::Retry;
            break;
        };
        let insn_len = u32::from(insn.len);
        if insn_len == 0 {
            stop = CompileStop::Retry;
            break;
        }
        let Some(next) = lin.checked_add(insn_len) else {
            stop = CompileStop::Retry;
            break;
        };
        let slot_eip = lin.wrapping_sub(cs.base);
        if slot_eip
            .checked_add(insn_len - 1)
            .is_none_or(|last| last > cs.limit)
        {
            stop = CompileStop::Retry;
            break;
        }
        if entry_lin >> BLOCK_PAGE_SHIFT != next.wrapping_sub(1) >> BLOCK_PAGE_SHIFT {
            stop = CompileStop::Retry;
            break;
        }
        let Some(expected_phys) = key.physical.checked_add(lin.wrapping_sub(entry_lin)) else {
            stop = CompileStop::Retry;
            break;
        };
        let physical_page_local = expected_phys
            .checked_add(insn_len - 1)
            .is_some_and(|last| key.physical >> BLOCK_PAGE_SHIFT == last >> BLOCK_PAGE_SHIFT);
        if !physical_page_local || cpu.decode_cache.line_phys_start(lin, d) != Some(expected_phys) {
            stop = CompileStop::Retry;
            break;
        }
        let structural_span = RejectedSpan::new(key, next.wrapping_sub(entry_lin) as usize);
        // The only prefix this backend supports is the operand-size override, and whether it is
        // PRESENT for a given `operand_size` depends on the code segment's default size, because
        // `decode` computes `operand_size = default_32 XOR operand_size_override`. Deriving the
        // expected override from `d` rather than hard-coding CS.D = 1 keeps this exact in both
        // segment widths.
        //
        // Under CS.D = 1 this is byte-identical to the previous form: Dword expects no override
        // and Word expects one. Under CS.D = 0 the mapping inverts, and the old form rejected
        // BOTH arms, so every 16-bit instruction died here as PrefixesUnsupported regardless of
        // what the classifier could lower. That is why this has to be fixed before any of the
        // 16-bit admission work can produce a single native instruction.
        let prefixes_supported = prefixes_supported_for(insn.prefixes, insn.operand_size, d);
        let continuable = insn.continuable || jit_admits_non_continuable(insn.opcode);
        if !prefixes_supported || !continuable {
            // Attributed since the completeness slice. The two conditions are split rather than
            // folded, because they are different work: a prefix refusal names a prefix the
            // backend does not emit (the row's `prefix_mask` says which, and an explicit segment
            // override is the common one), while a non-continuable shape is `block_continuable`'s
            // decision inherited wholesale from the interpreter's batching rules. Prefix wins the
            // tie, matching the `||` this arm is written with.
            if instruction_limit >= MAX_BLOCK_INSTRUCTIONS
                && cpu.jit_direct.barrier_census_enabled()
            {
                let reason = if prefixes_supported {
                    BarrierStop::NonContinuable
                } else {
                    BarrierStop::PrefixUnsupported
                };
                record_structural_barrier(
                    cpu,
                    &insn,
                    reason,
                    key,
                    entry_lin,
                    d,
                    SuffixSeed {
                        scan_start: next,
                        prefix_instructions: slots.len(),
                        stack_accesses,
                        memory_alu_slots,
                        callout_slots,
                        x87_slots,
                        dirty_segments,
                        model_dirty: true,
                    },
                );
            }
            stop = structural_span.map_or(CompileStop::Retry, CompileStop::Structural);
            break;
        }
        // Quake's 586 renderer benefits from native word operations. Doom's 486 self-patching
        // renderer recompiles the wider blocks often enough to lose throughput, so keep word
        // instructions as precise interpreter barriers in that mode.
        // Follow-up (dev_docs/specs/2026-07-15-smc-hardening-design.md, G1): with heat demotion
        // landed, A/B re-enabling 486 word ops - heat should now bound the churn this defends.
        if insn.operand_size == OperandSize::Word && !word_operands_admitted(cpu) {
            // Attributed since the completeness slice, and expected to read ZERO on both shipped
            // fixtures because they run 586. Instrumented anyway: "this arm is dead on the
            // corpus" is a measurement worth having, and "nothing records it" is not the same
            // statement. If a 486 persona ever benches, this row set is the whole Word population.
            if instruction_limit >= MAX_BLOCK_INSTRUCTIONS
                && cpu.jit_direct.barrier_census_enabled()
            {
                record_structural_barrier(
                    cpu,
                    &insn,
                    BarrierStop::WordPersona,
                    key,
                    entry_lin,
                    d,
                    SuffixSeed {
                        scan_start: next,
                        prefix_instructions: slots.len(),
                        stack_accesses,
                        memory_alu_slots,
                        callout_slots,
                        x87_slots,
                        dirty_segments,
                        model_dirty: true,
                    },
                );
            }
            stop = structural_span.map_or(CompileStop::Retry, CompileStop::Structural);
            break;
        }
        let kind = match DirectUnitPlanner::classify(&insn, lin, entry_lin) {
            PlannedInsn::Native(kind) => kind,
            PlannedInsn::HardBoundary => {
                if instruction_limit >= MAX_BLOCK_INSTRUCTIONS
                    && cpu.jit_direct.barrier_census_enabled()
                {
                    record_structural_barrier(
                        cpu,
                        &insn,
                        BarrierStop::HardBoundary,
                        key,
                        entry_lin,
                        d,
                        SuffixSeed {
                            scan_start: next,
                            prefix_instructions: slots.len(),
                            stack_accesses,
                            memory_alu_slots,
                            callout_slots,
                            x87_slots,
                            dirty_segments,
                            model_dirty: true,
                        },
                    );
                }
                stop = structural_span.map_or(CompileStop::Retry, CompileStop::Structural);
                break;
            }
        };
        // The stack-width admission matrix, and it is FIRST on purpose: everything below this
        // point reads the kind, and the access accessors in particular give different answers
        // for `Push` and `Push16` (a dword store against a word store). Anything that read the
        // pre-mapping kind would silently account the wrong width.
        //
        // SS.B picks the STACK POINTER width and `operand_size` picks how many bytes move; the
        // two are orthogonal (386 PRM 16.2, restated at `memory.rs:1218`). Four cells:
        //
        //   SS.B=1 + Dword  admit as `Push`   the shipped 32-bit form
        //   SS.B=1 + Word   STOP              a 2-byte push with a 32-bit SP. `Push` would move
        //                                     four bytes and decrement four, so admitting it
        //                                     here is a miscompile, not a missed lowering.
        //                                     Reachable TODAY through a 66-prefixed push in
        //                                     32-bit code, which the prefix gate accepts.
        //   SS.B=0 + Word   admit as `Push16` the new form
        //   SS.B=0 + Dword  STOP              four bytes on a 16-bit SP, not built yet
        //
        // This REPLACES the old `uses_stack() && !stack_is_32bit()` stop rather than joining it.
        // Left in place, that stop would have refused every `Push16`, because they exist only
        // when the stack is 16-bit. The slice would have done nothing and a counter-identity
        // gate would have passed while certifying the mechanism's own absence.
        //
        // `classify` cannot make this decision: it has no `cpu`, and SS.B is CPU state. Deciding
        // it here is safe against block reuse because `jit_mode_key` already carries SS.B, so a
        // block compiled for one stack width can never be entered with the other.
        let Some(kind) = stack_width_kind(cpu, kind, insn.operand_size) else {
            stop = CompileStop::Retry;
            break;
        };
        // A Word-size relative branch masks its target to 16 bits: `relative_jump` computes
        // `(eip + rel) & operand_size.mask()`. The emitted form bakes an unmasked delta, so it is
        // only correct where that mask is a no-op. Clamping the limit to 0xFFFF for Word makes
        // the existing check express exactly that condition, since `x <= a && x <= 0xFFFF` is
        // `x <= min(a, 0xFFFF)`.
        //
        // Nothing reaches this at Word today, because the allowlist in `classify` admits no
        // control transfer at Word size. It is a precondition for that allowlist opening, and it
        // is separated so the two are attributable independently.
        //
        // In real mode `cs.limit` is already 0xFFFF, so the clamp is a no-op there and the mask
        // was never observable. The case it exists for is a 66-prefixed branch in 32-bit code,
        // where the limit is typically 0xFFFFFFFF and the interpreter would wrap while the
        // emitted form would not. It covers Jmp and Call as well as Jcc, because
        // `static_control_target_within_limit` matches all three.
        let control_limit = control_target_limit(insn.operand_size, cs.limit);
        let control_target_ok = static_control_target_within_limit(kind, entry_eip, control_limit);
        // Mechanism count for the Word control-transfer path, split by what the clamp decided.
        // Byte identity cannot gate this slice on its own: if the pinned corpus carries no
        // 66-prefixed branch the changed path is never reached, every anchor holds, and the run
        // proves nothing. These two say whether it was reached at all.
        //
        // Counted only on the full-length pass. `compile_with_page_len` re-enters this function
        // once per step of a binary search whenever the emitted block overflows a host page, and
        // a shorter prefix may not reach the branch at all, so counting every pass would both
        // multiply the total and let admitted and refused flip for one block.
        // The addressing mechanism gate. Zero on a corpus with no 16-bit code IS the inertness
        // claim for that work, stated positively rather than as an absence of movement.
        if instruction_limit >= MAX_BLOCK_INSTRUCTIONS && insn.address_size == AddressSize::Word {
            cpu.perf.jit_direct_word_address_slots += 1;
        }
        if instruction_limit >= MAX_BLOCK_INSTRUCTIONS
            && insn.operand_size == OperandSize::Word
            && static_control_target(kind).is_some()
        {
            if control_target_ok {
                cpu.perf.jit_direct_word_control_admitted += 1;
            } else {
                cpu.perf.jit_direct_word_control_refused += 1;
            }
        }
        if !control_target_ok || !kind_segment_access_supported(cpu, kind) {
            stop = CompileStop::Retry;
            break;
        }
        // The dirty-segment rule. Every base and selector a block uses is a compile-time
        // immediate, so once a slot overwrites a segment register every later slot that bakes
        // anything from that segment would bake a stale value. Those become the block's end.
        //
        // `pinned_segments` is the test rather than `read_segment | write_segment`, and that is
        // the whole point of it existing: `MovSegToReg` bakes a SELECTOR and reports through
        // `selector_segment` alone, so the two-accessor spelling would let `mov ds,ax / mov bx,ds`
        // answer with the selector from compile time.
        //
        // `Boundary` and not `Retry`: the prefix before the write is perfectly good code and
        // should be kept. Retry would throw the whole block away and re-walk it to the same place.
        //
        // ABOVE every accumulator, not merely above the dirty one. A slot barred here never joins
        // the block, so letting it reach `read_segments` or `pinned_segments` first would pin a
        // segment the block does not use: extra `data_matches` comparisons and a retirement every
        // time that unrelated segment moves, or a spurious Retry out of `segment_access_supported`
        // for a descriptor nothing in the block reaches.
        if kind.pinned_segments() & dirty_segments != 0 {
            // Censused since the dirty-stop slice, and it is the only `Boundary` arm that is.
            // `CompileStop::Boundary` is five-way ambiguous (this rule, the walk's initializer,
            // the two block caps and a terminal slot), so the recording has to sit AT the rule
            // rather than being recovered from `stop` afterwards.
            //
            // Before this, admitting `MOV DS,r16` looked like it removed 18.4M census hits while
            // the census showed nothing gained. It had not removed them, it had moved them here,
            // where nothing was recording. That also reconciles the campaign's refuted
            // relocation item: no row grew because the work left the censused population
            // entirely.
            if instruction_limit >= MAX_BLOCK_INSTRUCTIONS
                && cpu.jit_direct.barrier_census_enabled()
            {
                record_structural_barrier(
                    cpu,
                    &insn,
                    BarrierStop::DirtySegment,
                    key,
                    entry_lin,
                    d,
                    SuffixSeed {
                        scan_start: next,
                        prefix_instructions: slots.len(),
                        stack_accesses,
                        memory_alu_slots,
                        callout_slots,
                        x87_slots,
                        dirty_segments,
                        // The arm whose suffix prices the dirty rule's own removal, so it is the
                        // one arm that must not apply it.
                        model_dirty: false,
                    },
                );
            }
            stop = CompileStop::Boundary;
            break;
        }
        if kind.is_x87()
            && (x87_slots == MAX_X87_SLOTS || slots.len() >= MAX_X87_BLOCK_INSTRUCTIONS)
        {
            stop = CompileStop::Retry;
            break;
        }
        // x87 and call-out slots do not share a block, in either order. The call-out hands the
        // helper the block's raw-clock prefix so the device sees the right guest-time offset
        // (jit/direct/callout.rs), and an x87 slot's contribution to that prefix is not raw
        // clocks at all: it is `weighted_fp_clocks`, which only becomes clocks through
        // `scale_weighted_fp_clocks` and its own `fp_rem` carry. Mixing them would need a second
        // carry previewed across the call for no fixture that wants it. Refusing is a missed
        // lowering; admitting would be a silently wrong device timestamp.
        if (kind.is_x87() && callout_slots != 0) || (kind.is_call_out() && x87_slots != 0) {
            stop = CompileStop::Retry;
            break;
        }
        if kind.is_call_out() && callout_slots == MAX_BLOCK_CALLOUT_SLOTS {
            stop = CompileStop::Retry;
            break;
        }
        if kind.is_memory_alu()
            && (memory_alu_slots == MAX_MEMORY_ALU_SLOTS
                || slots.len() >= MAX_MEMORY_ALU_BLOCK_INSTRUCTIONS)
        {
            stop = CompileStop::Retry;
            break;
        }
        if kind.uses_stack() && stack_accesses == MAX_BLOCK_STACK_ACCESSES {
            stop = CompileStop::Retry;
            break;
        }
        let slot_weighted_fp_clocks = kind.weighted_fp_clocks(cpu.persona());
        let Some(next_raw_clocks) = raw_clocks.checked_add(kind.raw_clocks()) else {
            stop = CompileStop::Retry;
            break;
        };
        let Some(next_weighted_fp_clocks) = weighted_fp_clocks.checked_add(slot_weighted_fp_clocks)
        else {
            stop = CompileStop::Retry;
            break;
        };
        let Some(next_byte_reads) = byte_reads.checked_add(kind.byte_reads()) else {
            stop = CompileStop::Retry;
            break;
        };
        let Some(next_word_reads) = word_reads.checked_add(kind.word_reads()) else {
            stop = CompileStop::Retry;
            break;
        };
        let Some(next_dword_reads) = dword_reads.checked_add(kind.dword_reads()) else {
            stop = CompileStop::Retry;
            break;
        };
        let Some(next_byte_stores) = byte_stores.checked_add(kind.byte_stores()) else {
            stop = CompileStop::Retry;
            break;
        };
        let Some(next_word_stores) = word_stores.checked_add(kind.word_stores()) else {
            stop = CompileStop::Retry;
            break;
        };
        let Some(next_dword_stores) = dword_stores.checked_add(kind.dword_stores()) else {
            stop = CompileStop::Retry;
            break;
        };
        stack_accesses += u8::from(kind.uses_stack());
        x87_slots += u8::from(kind.is_x87());
        callout_slots += u8::from(kind.is_call_out());
        if let Some(helper) = kind.call_out_helper() {
            callout_port_slots += u8::from(helper.probes_io_permission());
            callout_memory_slots += u8::from(helper.moves_a_stack_frame());
        }
        if let DirectKind::X87 { insn, .. } = kind {
            x87_exit_top = insn.advance_top(x87_exit_top);
        }
        memory_alu_slots += u8::from(kind.is_memory_alu());
        raw_clocks = next_raw_clocks;
        weighted_fp_clocks = next_weighted_fp_clocks;
        byte_reads = next_byte_reads;
        word_reads = next_word_reads;
        dword_reads = next_dword_reads;
        byte_stores = next_byte_stores;
        word_stores = next_word_stores;
        dword_stores = next_dword_stores;
        // `read_segments` and `write_segments` stay separate because the accessibility check needs
        // to know WHICH of the two a slot wants. `pinned_segments` is the other question, asked
        // once here so that a kind which bakes a descriptor without accessing memory through it
        // cannot be wired into some consumers and not others.
        if let Some(segment) = kind.read_segment() {
            read_segments |= segment_bit(segment);
        }
        if let Some(segment) = kind.write_segment() {
            write_segments |= segment_bit(segment);
        }
        pinned_segments |= kind.pinned_segments();
        // AFTER the test above, never before, or the write would bar itself.
        if let Some(segment) = kind.written_segment() {
            dirty_segments |= segment_bit(segment);
            segment_writes += 1;
        }
        has_wide_accesses |=
            kind.has_word_access() || kind.has_dword_read() || kind.has_dword_store();
        // Attached HERE, at the last point before the slot is committed, not next to `classify`.
        // Every `break` above abandons the slot, and a lane recorded for an instruction that never
        // joined the block would name bytes outside the block's span -- an address the write choke
        // could never match, but also a lane the block does not actually read through.
        // The two lane matchers are mutually exclusive by kind (`AluImm` vs `Load`), so at most
        // one fires per slot and both draw on the one `MAX_BLOCK_IMM_LANES` budget.
        #[cfg(feature = "barrier-census-closure")]
        let mut lane_probe_bits = 0u8;
        let kind = match imm_lane_for(cpu, &insn, kind, expected_phys, imm_lane_count) {
            Some((kind, lane)) => {
                imm_lanes[imm_lane_count] = lane.physical;
                imm_lane_count += 1;
                #[cfg(feature = "barrier-census-closure")]
                {
                    lane_probe_bits |= census::lane_probe::IMM;
                }
                kind
            }
            None => match disp_lane_for(cpu, &insn, kind, expected_phys, imm_lane_count) {
                Some((kind, lane)) => {
                    imm_lanes[imm_lane_count] = lane.physical;
                    imm_lane_count += 1;
                    disp_lane_count += 1;
                    #[cfg(feature = "barrier-census-closure")]
                    {
                        lane_probe_bits |= census::lane_probe::DISP;
                    }
                    kind
                }
                None => kind,
            },
        };
        // B.3's lane-match export. Gated at the CALL SITE per the census contract, and only when a
        // matcher actually fired: the `WALKED` bit is recorded once at the top of the walk, so the
        // common no-lane slot pays a compare against zero and nothing else.
        #[cfg(feature = "barrier-census-closure")]
        if lane_probe_bits != 0 && cpu.jit_direct.barrier_census_active() {
            cpu.jit_direct
                .note_lane_probe(key.linear(), lane_probe_bits);
        }
        fetch_lens[slots.len()] = insn.len;
        slots.push(DirectInsn {
            lin,
            len: insn.len,
            weighted_fp_clocks: slot_weighted_fp_clocks,
            kind,
        });
        lin = next;
        if kind.is_terminal() {
            break;
        }
    }
    if slots.is_empty()
        || (slots.len() < 3 && !slots.last().is_some_and(|slot| slot.kind.is_terminal()))
    {
        return match stop {
            CompileStop::Structural(span) => CompileOutcome::StructuralReject(span),
            CompileStop::Retry | CompileStop::Boundary => CompileOutcome::Retry,
        };
    }
    let Some(last) = slots.last() else {
        return CompileOutcome::Retry;
    };
    let guest_len = last
        .lin
        .wrapping_add(u32::from(last.len))
        .wrapping_sub(entry_lin) as usize;
    let Some(span) = BlockSpan::new(key, guest_len, slots.len()) else {
        return CompileOutcome::Retry;
    };
    let Some(segment_layout) =
        SegmentLayout::capture(cpu, read_segments, write_segments, pinned_segments)
    else {
        return CompileOutcome::Retry;
    };
    // A self-loop block accounts by MULTIPLYING its whole static accounting by the iteration
    // count at exit, so nothing inside the loop body may deposit into the runtime lanes per
    // iteration. A call-out does exactly that (it adds the helper's runtime clocks at the call
    // site), so the two shapes are incompatible: the loop-back would keep the deposits while the
    // exit multiplied the static total, double-counting one and dropping the other. Refusing the
    // self-loop SHAPE (not the block) leaves the block compiled and correct, just re-entered per
    // iteration like every non-loop block.
    //
    // A segment write bars the shape for a different and more serious reason, and the reason it
    // needs its own term is that it USED to be covered by accident: the write was going to be a
    // call-out, and `callout_slots == 0` would have disqualified the block for free. Emitting it
    // inline removes that cover. A self-loop re-enters the body natively through a bare `jnz`
    // back to `body_offset`, not through the prologue, so a slot BEFORE the write runs again
    // AFTER it against a base the write invalidated. The dirty rule is a straight-line walk and a
    // back-edge makes "before the write" also "after the write":
    //
    //     L: mov al, [si]   ; DS-relative, baked base
    //        mov ds, bx     ; DS := BX
    //        dec cx
    //        jnz L          ; iteration 2 reads through the OLD base
    //
    // Silent wrong address, no fault, no counter. Barring the shape leaves the block compiled and
    // correct, re-entered per iteration like any other.
    let self_loop = callout_slots == 0
        && segment_writes == 0
        && matches!(
            slots.last().map(|slot| slot.kind),
            Some(DirectKind::Jcc { taken_delta: 0, .. })
        );
    if self_loop && x87_slots != 0 && x87_entry_top != x87_exit_top {
        return CompileOutcome::Retry;
    }
    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    let map_bases = if byte_reads == 0
        && word_reads == 0
        && dword_reads == 0
        && byte_stores == 0
        && word_stores == 0
        && dword_stores == 0
    {
        None
    } else {
        let Some(bases) = cpu.jit_fast_map.native_bases() else {
            return CompileOutcome::Retry;
        };
        Some(bases)
    };
    #[cfg(not(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    )))]
    let map_bases = if byte_reads == 0
        && word_reads == 0
        && dword_reads == 0
        && byte_stores == 0
        && word_stores == 0
        && dword_stores == 0
    {
        None
    } else {
        return CompileOutcome::Retry;
    };
    let memory_cpl3 = cpu.current_privilege_level() == 3;
    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    let code_watch_tables = if byte_stores == 0 && word_stores == 0 && dword_stores == 0 {
        None
    } else {
        Some([
            cpu.decode_cache.native_code_watch_table(),
            cpu.jit_direct.native_code_watch_table(),
        ])
    };
    #[cfg(not(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    )))]
    let code_watch_tables = if byte_stores == 0 && word_stores == 0 && dword_stores == 0 {
        None
    } else {
        return CompileOutcome::Retry;
    };
    // Republish the bases this block would bake, BEFORE it can be installed:
    // any block emitted on the R15 arm only ever runs after its own compile
    // reached here, so the slots are current whenever such a block is live.
    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    let one_lookup_store;
    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    {
        if let Some(bases) = map_bases {
            cpu.native_table_slots.publish_map(bases);
        }
        if let Some(tables) = code_watch_tables {
            cpu.native_table_slots.publish_code_watch(tables);
        }
        // The one-lookup store shape needs the stub pad; build it lazily at the first
        // store-bearing compile (both table sets exist here by construction) and publish its
        // entry addresses. A failed build (F5) leaves the flag off for THIS block only.
        one_lookup_store = cpu.jit_direct.one_lookup_store
            && cpu.jit_direct.r15_tables
            && match (map_bases, code_watch_tables) {
                (Some(bases), Some(tables)) => {
                    match cpu.jit_direct.direct.store_stub_addresses(bases, tables) {
                        Some(addresses) => {
                            cpu.native_table_slots.publish_store_stubs(addresses);
                            true
                        }
                        None => false,
                    }
                }
                // A storeless block never reaches a store emitter; keep the flag off so the
                // emission arms cannot depend on an unpublished pad even by accident.
                _ => false,
            };
    }
    // The read twin gates on `map_bases` ALONE (load design D2): a load-only block has no
    // code-watch tables, and the read stubs consult none. Same lazy build, same F5-style
    // per-block fallback through `None`. Deliberately looser than D2's "first load-bearing
    // compile": any block with map bases builds the pad, so a store-only workload pays one
    // small pad build it never calls into — cheaper than threading a has-reads bit through
    // here, and the publish is idempotent.
    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    let one_lookup_load;
    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    {
        one_lookup_load = cpu.jit_direct.one_lookup_load
            && cpu.jit_direct.r15_tables
            && match map_bases {
                Some(bases) => match cpu.jit_direct.direct.read_stub_addresses(bases) {
                    Some(addresses) => {
                        cpu.native_table_slots.publish_read_stubs(addresses);
                        true
                    }
                    None => false,
                },
                None => false,
            };
    }
    #[cfg(not(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    )))]
    let one_lookup_load = false;
    #[cfg(not(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    )))]
    let one_lookup_store = false;
    let fallthrough = LinkTarget {
        linear: entry_lin.wrapping_add(u32::from(span.guest_len)),
        mode_key: key.mode_key,
    };
    // A block that overwrites a segment register publishes NO successors, static or dynamic.
    //
    // The argument this replaces was that such a block could not link anyway, because it exits
    // with a different segment than it entered and `link_compatible` demands equal snapshots.
    // That compares the wrong two things: `link_compatible` compares the two blocks'
    // COMPILE-TIME ENTRY snapshots, and on the pass that compiles them the write is very often a
    // no-op -- `mov ds, ax` where AX already holds DS is the ordinary "reload DS with what it
    // has" case. The edge links, and then a LINKED successor runs no segment check at all: a
    // chained transfer jumps into the successor's body without returning to `run_direct_block`,
    // so its `data_matches` never executes. A later entry with a different AX writes DS and jumps
    // straight into a body baked against the old base.
    //
    // Barring both edges makes the property true by construction. Inbound links stay safe: the
    // source's snapshot equality plus the root's `all_data_matches` still pin the entry state.
    let segment_write_block = segment_writes != 0;
    let dynamic_successor = !segment_write_block
        && matches!(
            slots.last().map(|slot| slot.kind),
            Some(
                DirectKind::Ret { .. }
                    | DirectKind::Ret16 { .. }
                    | DirectKind::JmpMem { .. }
                    | DirectKind::JmpReg { .. }
                    | DirectKind::CallReg { .. }
                    | DirectKind::CallMem { .. }
            )
        );
    #[cfg(feature = "direct-link-refusal-census")]
    struct TerminalLinks {
        targets: [Option<LinkTarget>; 2],
        successor_mask: [bool; 2],
        emitted_mask: [bool; 2],
    }
    #[cfg(feature = "direct-link-refusal-census")]
    let terminal_links = match slots.last().map(|slot| slot.kind) {
        Some(DirectKind::Jcc { taken_delta, .. }) => TerminalLinks {
            targets: [
                Some(fallthrough),
                Some(LinkTarget {
                    linear: entry_lin.wrapping_add(taken_delta),
                    mode_key: key.mode_key,
                }),
            ],
            successor_mask: [true, !self_loop],
            emitted_mask: [!self_loop; 2],
        },
        Some(
            DirectKind::Call { target_delta, .. }
            | DirectKind::Call16 { target_delta, .. }
            | DirectKind::Jmp { target_delta },
        ) => TerminalLinks {
            targets: [
                Some(LinkTarget {
                    linear: entry_lin.wrapping_add(target_delta),
                    mode_key: key.mode_key,
                }),
                None,
            ],
            successor_mask: [true, false],
            emitted_mask: [true, false],
        },
        Some(
            DirectKind::Ret { .. }
            | DirectKind::Ret16 { .. }
            | DirectKind::JmpMem { .. }
            | DirectKind::JmpReg { .. }
            | DirectKind::CallReg { .. }
            | DirectKind::CallMem { .. },
        ) => TerminalLinks {
            targets: [None, None],
            successor_mask: [false, false],
            emitted_mask: [false, false],
        },
        _ => TerminalLinks {
            targets: [Some(fallthrough), None],
            successor_mask: [true, false],
            emitted_mask: [true, false],
        },
    };
    #[cfg(feature = "direct-link-refusal-census")]
    let successors = if segment_write_block {
        [None, None]
    } else {
        [
            terminal_links.successor_mask[0]
                .then_some(terminal_links.targets[0])
                .flatten(),
            terminal_links.successor_mask[1]
                .then_some(terminal_links.targets[1])
                .flatten(),
        ]
    };
    #[cfg(not(feature = "direct-link-refusal-census"))]
    let successors = match slots.last().map(|slot| slot.kind) {
        _ if segment_write_block => [None, None],
        Some(DirectKind::Jcc { taken_delta, .. }) => [
            Some(fallthrough),
            (!self_loop).then_some(LinkTarget {
                linear: entry_lin.wrapping_add(taken_delta),
                mode_key: key.mode_key,
            }),
        ],
        Some(
            DirectKind::Call { target_delta, .. }
            | DirectKind::Call16 { target_delta, .. }
            | DirectKind::Jmp { target_delta },
        ) => [
            Some(LinkTarget {
                linear: entry_lin.wrapping_add(target_delta),
                mode_key: key.mode_key,
            }),
            None,
        ],
        Some(
            DirectKind::Ret { .. }
            | DirectKind::Ret16 { .. }
            | DirectKind::JmpMem { .. }
            | DirectKind::JmpReg { .. }
            | DirectKind::CallReg { .. }
            | DirectKind::CallMem { .. },
        ) => [None, None],
        _ => [Some(fallthrough), None],
    };
    #[cfg(feature = "direct-link-refusal-census")]
    let emitted_static_targets = [
        terminal_links.emitted_mask[0]
            .then_some(terminal_links.targets[0])
            .flatten(),
        terminal_links.emitted_mask[1]
            .then_some(terminal_links.targets[1])
            .flatten(),
    ];
    let link_cells = [Arc::new(LinkCell::new()), Arc::new(LinkCell::new())];
    let emitted = emit::emit(EmitInput {
        slots: &slots,
        span,
        raw_clocks,
        weighted_fp_clocks,
        byte_reads,
        word_reads,
        dword_reads,
        byte_stores,
        word_stores,
        dword_stores,
        self_loop,
        x87_entry_top: (x87_slots != 0).then_some(x87_entry_top),
        memory: MemoryEmitContext {
            map: map_bases,
            code_watch_tables,
            cpl3: memory_cpl3,
            r15_tables: cpu.jit_direct.r15_tables,
            watch_page_bit: cpu.jit_direct.watch_page_bit,
            one_lookup_store,
            one_lookup_load,
            segments: segment_layout,
            address_wrap: if d {
                emit::AddressWrap::None
            } else {
                emit::AddressWrap::Word
            },
        },
        link_cell_ptrs: link_cells.each_ref().map(|cell| cell.address()),
        fetch_trace: cpu.jit_direct.native_fetch_trace,
    });
    CompileOutcome::Compiled(Compilation {
        span,
        fetch_lens,
        raw_clocks,
        weighted_fp_clocks,
        byte_reads,
        word_reads,
        dword_reads,
        byte_stores,
        word_stores,
        dword_stores,
        segment_layout,
        memory_cpl3,
        has_wide_accesses,
        self_loop,
        has_x87: x87_slots != 0,
        callout_slots,
        callout_port_slots,
        callout_memory_slots,
        x87_entry_top,
        x87_exit_top,
        dynamic_successor,
        successors,
        #[cfg(feature = "direct-link-refusal-census")]
        emitted_static_targets,
        link_cells,
        body_offset: emitted.body_offset,
        imm_lanes,
        disp_lanes: disp_lane_count,
        code: emitted.code,
    })
}

#[cfg(test)]
pub(crate) fn compile_with_instruction_limit_for_test(
    cpu: &mut CpuGsw,
    entry_lin: u32,
    d: bool,
    instruction_limit: usize,
) -> Option<Compilation> {
    match compile_with_instruction_limit(cpu, entry_lin, d, instruction_limit) {
        CompileOutcome::Compiled(compilation) => Some(compilation),
        CompileOutcome::StructuralReject(_) | CompileOutcome::Retry => None,
    }
}

/// The limit a static control target must satisfy, given the branch's operand size.
///
/// A Word-size relative branch masks its target to 16 bits: `relative_jump` computes
/// `(eip + rel) & operand_size.mask()`. The emitted form bakes an UNMASKED delta, so it is
/// correct only where that mask is a no-op. Clamping the limit to 0xFFFF makes the existing
/// `<=` check express exactly that condition, because `x <= a && x <= 0xFFFF` is
/// `x <= min(a, 0xFFFF)`.
///
/// In real mode `cs.limit` is already 0xFFFF, so the clamp is a no-op and the mask was never
/// observable, which is why this went unnoticed. The case it exists for is a 66-prefixed branch
/// in 32-bit code, where the limit is typically `u32::MAX` and the interpreter would wrap while
/// the emitted form would not.
///
/// Nothing reaches this at Word size today: `classify`'s Word allowlist admits no control
/// transfer. It is a precondition for that allowlist opening, split out so the two are
/// separately attributable, and it is a free function so it can be tested at all.
fn control_target_limit(operand_size: OperandSize, cs_limit: u32) -> u32 {
    match operand_size {
        OperandSize::Word => cs_limit.min(0xFFFF),
        OperandSize::Dword => cs_limit,
    }
}

/// The block-entry-relative delta of `kind`'s static control target, or `None` for a kind that
/// has no static target.
///
/// Extracted so the guard below and the Word mechanism counters read the SAME notion of "this
/// slot is a control transfer". A second `matches!` at the counter's call site would be a
/// second source of truth, and the two could drift as kinds are added.
fn static_control_target(kind: DirectKind) -> Option<u32> {
    match kind {
        DirectKind::Call { target_delta, .. }
        | DirectKind::Call16 { target_delta, .. }
        | DirectKind::Jmp { target_delta } => Some(target_delta),
        DirectKind::Jcc { taken_delta, .. } => Some(taken_delta),
        _ => None,
    }
}

fn static_control_target_within_limit(kind: DirectKind, entry_eip: u32, limit: u32) -> bool {
    static_control_target(kind).is_none_or(|delta| entry_eip.wrapping_add(delta) <= limit)
}

fn kind_segment_access_supported(cpu: &CpuGsw, kind: DirectKind) -> bool {
    SEGMENT_ORDER.into_iter().all(|segment| {
        let read = kind.read_segment() == Some(segment);
        let write = kind.write_segment() == Some(segment);
        (!read && !write)
            || segment_access_supported(cpu, cpu.registers.segment(segment), read, write)
    })
}

#[cfg(test)]
#[path = "direct_test.rs"]
mod tests;
