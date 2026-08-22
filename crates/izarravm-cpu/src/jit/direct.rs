// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Admission and storage for page-local direct-code blocks.

mod callout;
#[cfg(feature = "direct-callout-attribution")]
mod callout_attribution;
pub(crate) mod census;
mod classify;
mod emit;
mod env_gates;
mod native_exit;
mod segment_layout;
#[cfg(feature = "smc-census")]
mod smc_census;

/// Re-exported rather than moved-and-repathed: every one of these names is referenced as
/// `jit::direct::X` from `run.rs`, `lib.rs` and the emitter, and the extraction is pure motion,
/// so the paths must not move with the text.
pub(crate) use native_exit::{
    DirectEntryFn, NativeBlockTrace, NativeExit, SideExitReason, UnresolvedReason,
};

/// Same reason as the `native_exit` re-export above: the extraction is pure motion, so every
/// `jit::direct::X` path that named a segment-layout item keeps naming it here.
pub(crate) use segment_layout::*;

/// And the same again for the environment gates, which moved out to keep this file under the
/// layout limit. Every knob is still `jit::direct::X` to its callers.
pub(crate) use env_gates::*;

#[cfg(test)]
pub(crate) use callout::ResumeSnapshot;
pub(crate) use callout::{
    CALL_OUT_STACK_FRAME_DWORDS, CallOutHelper, CallOutSlotCounts, CallOutTable, InterpretOneCell,
    InterpretOneRow,
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
#[cfg(feature = "smc-census")]
pub(crate) use smc_census::{SmcCensus, smc_census_default};

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use izarravm_core::CpuPersona;

use census::{BarrierStop, SuffixSeed, record_structural_barrier};
// The stall/census TAXONOMY lives in `census.rs` beside the builder that already consumed it
// (`stall_snapshot`, `snapshot`), moved verbatim to keep this file under the source-line ceiling.
// Re-exported from here because every out-of-module path names them through `jit::direct`.
#[cfg(feature = "direct-admission-census")]
pub(crate) use census::AdmissionDecline;
pub(crate) use census::{
    BlockCacheStats, DirectBarrierCensus, DirectStallTally, DormantReason, LinkClearCause,
    LinkRefusal, RetryCause, UnboundTarget, barrier_census_default,
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
/// Emitted-size model for the compile walk, in host bytes. It exists for ONE reason: an installed
/// block owns exactly one host page, and when the walk runs past that page `compile_with_page_len`
/// recovers by binary-searching shorter prefixes, which re-walks AND RE-EMITS the block four more
/// times. The model lets the walk stop at the page instead of discovering it afterwards.
///
/// It is a COST estimate, never a correctness one. Under-predicting costs nothing but the search
/// that was going to run anyway; over-predicting costs a slot of block length. The search stays as
/// the net, so no arm of this table can make a block that does not fit install.
///
/// MEASURED 2026-08-22 on the tombraid loader phase (`.bench/tombraid_loader_c`, 19,489 sampled
/// emissions, least squares against the real `code.len()`): intercept 489.5, register-only slot
/// 33.4, memory slot 341.3, call-out slot 572.7, terminal slot +195.3; RMS residual 27.5 bytes and
/// 95th-percentile absolute residual 15 bytes against a 3,789-byte mean, agreeing with the
/// one-page boundary on every sample. The constants below round those fits UP, because the two
/// errors are not symmetric: an under-prediction pays four extra compiles and an over-prediction
/// pays a fraction of one slot.
///
/// x87 and memory-ALU slots are deliberately priced at the memory-slot rate rather than measured:
/// the loader carries none of either, and both classes already have their own instruction caps
/// (`MAX_X87_BLOCK_INSTRUCTIONS`, `MAX_MEMORY_ALU_BLOCK_INSTRUCTIONS`) sized to keep such a block
/// inside its page. Pricing them low leaves those blocks forming exactly as they do today; pricing
/// them high would shorten blocks whose length is already governed elsewhere. A memory-ALU slot
/// reaches that rate through its own accesses; an x87 slot declares none, so
/// `emitted_bytes_estimate` names it, and the four register-operand kinds that emit a side exit
/// are named beside it for the same reason.
pub(super) const EMITTED_BLOCK_FIXED_BYTES: u32 = 576;
pub(super) const EMITTED_REGISTER_SLOT_BYTES: u32 = 40;
pub(super) const EMITTED_MEMORY_SLOT_BYTES: u32 = 352;
pub(super) const EMITTED_CALL_OUT_SLOT_BYTES: u32 = 592;
pub(super) const EMITTED_TERMINAL_SLOT_EXTRA_BYTES: u32 = 208;
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

/// Code sites the demoted-call-out set will hold. A memory bound on a map that deliberately
/// outlives the blocks it was learned from, so containment in `entries` cannot bound it.
///
/// 4,096 sites is 32 KiB of set at two `u32`s a site, and two orders of magnitude above what a
/// whole loader phase produces: the tombraid loader learns 64 sites in 500 M cycles, all of them
/// one row (`0x8e_mov_sreg`). At the cap a further site is REFUSED and its demotion does not
/// retire anything -- the block keeps its slot and pays the emitted prologue test on every
/// execution, which is the pre-mechanism cost and is bounded. `note_demoted_callout_site` is where
/// that is decided, and `jit_direct_demoted_callout_sites_refused` counts it, because a cap that
/// binds silently is a cap nobody finds.
const DEMOTED_CALLOUT_SITE_CAP: usize = 4_096;

/// Probes a `Dormant` key parked for a CLEARABLE compile-walk cause must absorb before the retry
/// lift re-admits it. See `BlockCache::lift_clearable_retry_dormant` for what one visit is: the
/// sticky-decline memo throttles this site to about one probe per memo era, so the constant buys a
/// long-run gate rather than a 64-exit one.
///
/// Sixty-four rather than a smaller number because the failure mode of lifting too eagerly is a
/// compile attempt per key per window with a park behind it, on the exact population that is
/// already the largest unattributed exit class. A key whose decode line really has been refilled
/// stays liftable for as long as the run goes on, so lateness costs latency and earliness costs
/// compile time.
pub(crate) const RETRY_LIFT_VISITS: u8 = 64;

/// How many spent-lift records the set may hold. It grows by at most one entry per lift, a lift
/// needs `RETRY_LIFT_VISITS` probes, and a key can contribute at most one entry per clearable
/// cause (two), so this is far above what any measured workload reaches -- the loader parks 466
/// keys in total. Full means no further key is lifted, which fails closed to the pre-slice
/// behaviour rather than to an eviction policy that could hand one key an endless supply of
/// lifts.
const RETRY_LIFT_SPENT_CAP: usize = 4_096;

/// Untried entries a call-out block may spend at trial quota before the governor gives up on
/// classifying it. A call-out behind a rarely-taken branch would otherwise sit at quota 1
/// forever, which is a real regression for the block's OTHER instructions.
pub(crate) const MAX_UNTRIED_TRIALS: u8 = 8;

/// What the governor has learned about one block's port call-outs. See `run_direct_block`'s G2
/// for the transitions and why classification is trial-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallOutAdmission {
    /// Nothing learned yet, and `u8` entries already spent trying. Admitted at quota 1.
    Untried(u8),
    /// Every serve in the trial left the bus untouched, so the block completes and takes its
    /// static link. Admitted at the full chain quota.
    Lazy,
    /// Some serve in the trial touched device state and step-broke. Refused at head, exactly as
    /// before the governor existed.
    IoTouching,
    /// Some serve in the trial returned abnormal -- a denied or undecoded port. Refused at head.
    Denied,
    /// `MAX_UNTRIED_TRIALS` entries produced no serve at all. Normal quota, call-out still
    /// refused at head: today's behaviour, which is the safe resting state.
    Unclassified,
}

impl Default for CallOutAdmission {
    fn default() -> Self {
        Self::Untried(0)
    }
}
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
/// The store width a DWORD lane accepts. The dword field is patched whole or not at all; a byte
/// or word patch of it takes the normal invalidation path.
///
/// Since the 2026-08-19 L2 arm-1 slice this is one of TWO widths a lane can carry, not the only
/// one — see `IMM8_LANE_WIDTH`. Each lane records its own width and the write choke accepts a
/// patch only at that lane's exact width, so the two classes cannot absorb each other's writes.
pub(crate) const IMM_LANE_WIDTH: u32 = 4;
/// The store width a ONE-BYTE lane accepts (`imm8_lane_for`, the `0x80 /r` ALU r/m8 imm8 family).
///
/// Its own width class rather than a widened match on `IMM_LANE_WIDTH`, and the distinction is
/// load-bearing in both directions: a one-byte patch landing on a DWORD lane must still reject on
/// width (it changes one byte of a field the emitted code reads whole, so the block's baked shape
/// is wrong), and a four-byte patch landing on a one-byte lane must still reject on width (it
/// rewrites the three instruction bytes after the immediate as well). `smc_lane_reject_width`
/// counts both, and its meaning — "a write started exactly at a lane and was refused for its
/// size" — is unchanged by the second class existing.
pub(crate) const IMM8_LANE_WIDTH: u32 = 1;
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

mod block_key;
pub(crate) use block_key::{BlockKey, key_for, key_for_phys, native_keys_admitted};

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
    /// `segment_write_block` itself has TWO PRODUCERS since S4f and the proof above is unchanged
    /// by that: it is a proof about the two `successors` arms, not about what sets the flag. The
    /// producers are the `LoadSegReal`/`PopSegReal` lowerings and the `InterpretOne` rows whose
    /// `may_write_segment` says yes, and they reach the flag through separate accumulators so that
    /// only the first also marks `dirty_segments`.
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

    /// Call-out slots that run one interpreter instruction. Priced at
    /// `INTERPRET_ONE_MAX_CORE_CLOCKS` plus `INTERPRET_ONE_MAX_DATA_ACCESSES` worst-width data
    /// accesses each in `compute_iteration_upper`, because unlike the other two classes this one
    /// is a maximum over an allowlist rather than one opcode's constant. Both constants are named
    /// rather than restated: the access count was two while every row was a one-operand memory
    /// form and is four since `0x8E` joined, and a literal here would have gone stale.
    pub(crate) fn callout_interpret_one_slots(&self) -> u32 {
        self.callout_slots.interpret_one()
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

    /// The slot this id occupies, for the one test that has to prove a slot was RECYCLED rather
    /// than appended. Nothing outside the module may key on it: the generational identity is the
    /// whole point of `BlockId`.
    #[cfg(test)]
    pub(crate) fn index_for_test(self) -> usize {
        self.index()
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
    Dormant(DormantEntry),
    Rejected(RejectedSpan),
    Compiled(BlockId),
}

#[derive(Clone, Copy)]
struct HotEntry {
    key: BlockKey,
    id: BlockId,
    generation: u32,
}

/// A parked key's Dormant record: why it parked, and everything the retry lift needs to decide
/// whether to re-probe it.
///
/// Four bytes in a variant that had eight to spare (`Compiled` carries a `BlockId`), so the whole
/// record is free in `entries`, which is the map every probe and every invalidation walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DormantEntry {
    /// Which gate parked it. `SpanHot` is the heat lane and carries no cause.
    reason: DormantReason,
    /// The compile walk's own answer, `Some` exactly for `DormantReason::CompileRetry`.
    cause: Option<RetryCause>,
    /// Probes that reached this key while it was parked, saturating at `RETRY_LIFT_VISITS`.
    /// See `lift_clearable_retry_dormant` for what a visit actually is.
    visits: u8,
    /// Set when the key already spent a lift for THIS cause and came straight back. The evidence
    /// is in: re-walking reaches the same answer, so no further lift is offered.
    permanent: bool,
}

/// What `lift_cold_smc_dormant` found. `StillDormant` is the sticky-decline memo's exact
/// predicate: the entry was Dormant and the recovery lift did NOT fire, so the census class was
/// `DormantProbe` and nothing about the run changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DormantLift {
    /// The key was not Dormant at all (a plain `Rejected` span, or the cache is disabled and
    /// synthesised the probe result).
    NotDormant,
    /// The entry chunk's heat stamp had aged out; the key is back to `Seen`.
    Lifted,
    /// Dormant, stamp still current: parked, and provably parked for the rest of this epoch.
    StillDormant,
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
    /// Parallel to `blocks`, same `BlockId::index()`: this block's CHAIN segment requirement --
    /// its own `SegmentLayout` merged with the requirement of every block reachable from it
    /// through currently live links. Only `used` ever differs from `segment_layouts[i]`, because
    /// the merge is non-adopting (`SegmentLayout::merge_chain`).
    ///
    /// MONOTONE for the whole life of the block in slot `i`: written by `install` (reset to the
    /// block's own layout, in the same statement that writes `segment_layouts[i]`, so the two can
    /// never come apart) and by `widen_chain_requirement`, which only ever adds bits. Nothing
    /// else -- not unlink, not retire, not the link-epoch bump -- touches it. A stale-too-WIDE
    /// requirement costs an over-strict edge refusal; a stale-too-NARROW one is a wrong-base
    /// miscompile, so narrowing is made unrepresentable rather than merely unlikely.
    chain_layouts: Vec<SegmentLayout>,
    /// Parallel to `blocks`, same `BlockId::index()`: the physical start of each mutable imm32
    /// lane the block's emitted code reads through, `NO_IMM_LANE` for an unused slot. Out of
    /// `CompiledBlock` for the reason its size pin states — nothing here is read on a block entry,
    /// only at the SMC write choke. A recycled slot is refilled by `install`, so a retired
    /// occupant's lanes can never answer for its successor.
    block_imm_lanes: Vec<[u32; MAX_BLOCK_IMM_LANES]>,
    /// The WIDTH CLASS of each entry in `block_imm_lanes`, same index, `0` for an unused slot.
    /// Either `IMM_LANE_WIDTH` (4) or `IMM8_LANE_WIDTH` (1).
    ///
    /// A parallel array rather than a field packed into the lane address, because the address array
    /// is what the write choke scans and it is scanned on every SMC write to a compiled span: the
    /// common case is "no lane matches", and that comparison stays a bare `u32` equality against a
    /// value that needs no unmasking. The width is read only on the slots that already matched or
    /// already overlap.
    block_imm_lane_widths: Vec<[u8; MAX_BLOCK_IMM_LANES]>,
    /// G1 lane trial spend marks: the heat epoch in which `lane_trial_spend` last granted this
    /// key its one compile-through-heat attempt (see `lane_trial_enabled` for the mechanism).
    /// Stale epochs are simply overwritten on the next grant, so the map only ever holds one
    /// entry per key that has EVER been hot — thousands on a Build-engine fixture, cleared with
    /// the rest of the cache storage.
    lane_trial_epochs: HashMap<BlockKey, u32>,
    /// Per-key x87 TOP-mismatch retire count, capped at `X87_TOP_RETIRE_CAP`. A key that has
    /// spent its budget is TOP-STICKY: the entry refusal in `run_direct_block` still fires and the
    /// instruction still interprets, but the block is no longer demoted for another
    /// re-specialization. See `retire_key_for_top_mismatch`.
    ///
    /// The key set is a SUBSET of `entries`': the count is only written on the `Compiled` path,
    /// and both sites that drop a key from `entries` (`invalidate_physical_range`,
    /// `reset_storage`) drop it here too. That containment is what bounds the map at
    /// `DEFAULT_ENTRY_CAP` without an eviction policy, and it is why a rewritten page hands the
    /// new code at that address a fresh budget instead of its predecessor's stickiness.
    top_mismatch_retires: HashMap<BlockKey, u8, PodKeyBuildHasher>,
    /// Code sites, by `(physical, mode_key)`, whose `InterpretOne` call-out the governor demoted.
    /// The compile walk reads it and ends the block BEFORE such a slot, so the recompile produces
    /// the hard boundary the row had before it was admitted.
    ///
    /// A SITE and not a block key, because the demotion is a property of the instruction: the same
    /// `MOV DS,[bx]` reached from a second entry is the same losing bet, and keying on the block
    /// entry would let every other entry re-learn it over eight more executions. Physical rather
    /// than linear for the reason invalidation is physical: the code byte is what was judged.
    ///
    /// Bounded by `DEMOTED_CALLOUT_SITE_CAP` rather than by containment in `entries`, because it
    /// deliberately OUTLIVES the block it was learned from -- that is the whole mechanism. It does
    /// NOT outlive a whole-cache wipe: `reset_storage` clears it with every other map, on the
    /// invariant that a cache that has thrown away its rejected spans, its dormant heat and its
    /// admission classes has learned nothing. Re-learning costs three resyncs, a retire and a
    /// recompile per site, which at the 64 sites the tombraid loader finds is not worth a special
    /// case; and a reset can follow an overlay load, where a physical address holding different
    /// code would otherwise inherit the old code's judgement. MEASURED both ways on the loader and
    /// byte-identical, because that fixture takes no cache reset at all (`jit_direct_cache_resets`
    /// is 0), so this is an argument and not a measurement -- said plainly rather than dressed up
    /// as one.
    demoted_callout_sites: HashSet<(u32, u32), PodKeyBuildHasher>,
    /// The KEY of a block whose `InterpretOne` slot the governor just demoted, parked for
    /// `run_direct_block` to retire once the native run returns.
    ///
    /// A latch rather than a direct retire, because the demotion is observed from INSIDE the
    /// running block: `retire_key_for_recompile` frees the block's metadata slot and unlinks it,
    /// and doing that while its code is on the host stack would pull the ground out from under the
    /// return.
    ///
    /// The KEY and not a bool. A chained entry runs successor blocks, and `run_direct_block`'s own
    /// `span.key` is the ROOT's, so a bool would retire the root and leave the demoted slot where
    /// it is -- permanently, because `note_execution` latches once per cell and never asks again.
    /// The key comes off the cell, which was compiled into exactly one block.
    ///
    /// HERE and not beside `callout_error` on `DirectRuntimeState`, which is where the two other
    /// call-out latches live and where this one was written first: `DirectRuntimeState` is a
    /// by-value field of `CpuGsw`, and sixteen bytes of `Option<BlockKey>` there moved
    /// `CpuGsw.registers` and `CpuGsw.pending_flags`, both of which are pinned by tests precisely
    /// so that the hot interpreter region is not reshuffled by accident. `BlockCache` is behind a
    /// `Box`, so a field here costs the CPU struct nothing -- the same reason
    /// `last_side_exit_reason` lives on `JitState`.
    ///
    /// At most one is ever pending: a cell latches its demotion once, and the entry that observed
    /// it returns before another can start.
    callout_retire_pending: Option<BlockKey>,
    /// The guest EIP an `InterpretOne` STI slot left behind, latched when that slot RESUMED with
    /// `interrupt_shadow` still armed and read once by `run_direct_block` after the native return.
    ///
    /// The question it answers is "did any instruction retire after the arming slot". The helper
    /// cannot: when it returns, the rest of the block has not run yet. The boundary can, by
    /// comparing the block's final EIP against this one, and that comparison decides whether the
    /// shadow was consumed inside the block (clear it) or the block ended at the STI (leave it
    /// armed for the interpreter, which is what hardware does).
    ///
    /// HERE and not on `DirectRuntimeState` beside the other two run-scoped latches, for the
    /// reason `callout_retire_pending` moved here in the S3 review round: that struct is inline in
    /// `CpuGsw` ahead of `registers` and `pending_flags`, whose offsets emitted code BAKES. Adding
    /// a field there moves them and breaks the layout pins. The block cache is behind a `Box`, so
    /// nothing it holds can move a baked offset.
    ///
    /// At most one is ever live, for `callout_retire_pending`'s reason: the entry that set it
    /// returns before another can start.
    interrupt_shadow_armed_at: Option<(u32, u32)>,
    /// How many arming call-out steps in this run left the shadow set. The latch above carries the
    /// value this counter had when it was written, and the boundary asserts the two agree, which
    /// is the invariant review finding F1 broke: a latch from an EARLIER arming slot than the last
    /// one is exactly the state in which the boundary clears a shadow it should have left alone.
    ///
    /// The stamp and the latch take the SAME predicate at the one site that writes them
    /// (`interpret_one_step`), and that is load-bearing rather than tidy. Stamped on "this step
    /// armed it" instead, a second arming slot rewrote the latch without moving the count and the
    /// assertion held on exactly the shape it exists for.
    interrupt_shadow_arms: u32,
    /// `interrupt_shadow` as it stood when the running block was ENTERED, published here for
    /// `ResumeSnapshot::capture` and cleared after the native return.
    ///
    /// On `BlockCache` for the reason the latch above is: `DirectRuntimeState` is inline in
    /// `CpuGsw` ahead of `registers` and `pending_flags`, whose offsets emitted code bakes.
    block_entry_interrupt_shadow: bool,
    /// Set when the boundary above CLEARED the shadow, i.e. the block consumed an STI's
    /// one-instruction reprieve inside itself. `run_budgeted_inner` reads it to run the
    /// interrupt-transition test the interpreter runs after a shadowed instruction; see the fold
    /// beside `can_take_before`.
    interrupt_shadow_consumed: bool,
    /// Every (key, cause) pair that has already spent a retry lift.
    ///
    /// Read by `dormant` to mark a same-cause re-park PERMANENT, which is what stops a key that
    /// the walk refuses deterministically-but-for-a-clearable-looking-reason from buying a lift
    /// every `RETRY_LIFT_VISITS` probes for ever. Capped at `RETRY_LIFT_SPENT_CAP`.
    ///
    /// A SET OF PAIRS and not a map from key to cause. The map remembered only the LAST cause, so
    /// a key alternating `DecodeMiss` and `TranslationMismatch` found its record overwritten every
    /// time and lifted for ever -- the exact treadmill the rule exists to stop, reached by a key
    /// whose two causes are both clearable. As a set the bound is what it says: at most one lift
    /// per key per cause, and `clearable_by_retry` names two causes, so at most two lifts per key.
    retry_lift_spent: HashSet<(BlockKey, RetryCause)>,
    /// Test-only override of `lane_trial_enabled` — the env gate is a process-global `OnceLock`,
    /// so in-process tests of the trial path set this instead of the environment.
    lane_trial_override: Option<bool>,
    block_portals: Vec<Arc<BlockPortal>>,
    link_cells: Vec<[Arc<LinkCell>; 2]>,
    /// One `InterpretOneCell` per `InterpretOne` slot, per block, in slot order.
    ///
    /// Beside `link_cells` and kept alive the same way, which is the whole of design review M11:
    /// the governor's state has to survive as long as the emitted code that bakes its address, and
    /// it must NOT be keyed by anything `retire_block` can recycle. A block's own allocation is
    /// the only place with exactly that lifetime, and it is why the demotion needs no recompile.
    ///
    /// A `Vec` per block rather than a fixed array: `MAX_BLOCK_CALLOUT_SLOTS` blocks carry none at
    /// all, and this vector is never read on an entry path -- only at install and at retire.
    interpret_one_cells: Vec<Vec<Arc<InterpretOneCell>>>,
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
    /// The call-out admission governor's learned class per block, parallel to `blocks` and keyed
    /// exactly like `iteration_upper_cache` above -- same `BlockId::index()`, same epoch, same
    /// `install`-zeroes-a-recycled-slot discipline, and deliberately no clear in `retire_block`:
    /// one invariant, one mechanism.
    ///
    /// Why it is learned and not compiled in: `IN AL,DX` takes its port from live DX, and the
    /// `MOV DX,imm` that sets it is usually in another block, so nothing at compile time knows
    /// which port a call-out slot will read. See `run_direct_block`'s G2 for the whole rule.
    callout_admission: Vec<CallOutAdmission>,
    /// The `jit_cost_dial_epoch()` `callout_admission` was learned under.
    ///
    /// A SAFETY KEY against a persona change, not a refresh mechanism, and the difference is
    /// worth stating because the first draft of this comment got it wrong. The epoch is
    /// `active_mode + 1`, its only writer is `Machine::set_mode`, and that calls `CpuGsw::set_mode`
    /// first, which clears every compiled block along with this array. So the epoch cannot roll
    /// inside a run and it bounds nothing: a classification is TERMINAL for the block's lifetime,
    /// and the only reclassification that exists is a fresh compile into a recycled slot. The
    /// demotion arm in `run_direct_block` is the one exception, and it is deliberately one-way.
    callout_admission_epoch: u64,
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
    /// SMC census stage A. It lives on the CACHE rather than on `JitState` (which is where the
    /// design put it) for one reason: every stage-A increment site is a `BlockCache` method
    /// reached through `&mut self`, and `invalidate_physical_range` cannot see a `JitState` field
    /// without a signature change the design explicitly forbids. The layout argument is unchanged
    /// — `CpuGsw` owns `Box<JitState>`, which owns this cache, so the field costs zero bytes on
    /// `CpuGsw` and the pinned `pending_flags` offset does not move. A clone drops it, exactly as
    /// the link-refusal census above does: a lockstep clone must not double-count its parent.
    #[cfg(feature = "smc-census")]
    smc_census: Option<Box<SmcCensus>>,
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
            chain_layouts: Vec::new(),
            block_imm_lanes: Vec::new(),
            block_imm_lane_widths: Vec::new(),
            lane_trial_epochs: HashMap::default(),
            top_mismatch_retires: HashMap::default(),
            demoted_callout_sites: HashSet::default(),
            callout_retire_pending: None,
            retry_lift_spent: HashSet::default(),
            interrupt_shadow_armed_at: None,
            interrupt_shadow_arms: 0,
            block_entry_interrupt_shadow: false,
            interrupt_shadow_consumed: false,
            lane_trial_override: None,
            block_portals: Vec::new(),
            link_cells: Vec::new(),
            interpret_one_cells: Vec::new(),
            link_sources: HashMap::new(),
            outbound: Vec::new(),
            global_block_upper_cache: [0; 2],
            iteration_upper_cache: Vec::new(),
            callout_admission: Vec::new(),
            callout_admission_epoch: 0,
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
            #[cfg(feature = "smc-census")]
            smc_census: smc_census_default(),
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
            Some(BlockState::Dormant(..) | BlockState::Rejected(_)) => BlockProbe::Rejected,
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
            compilation.callout_port_slots
                + compilation.callout_memory_slots
                + compilation.callout_interpret_one_slots,
            compilation.callout_slots,
            "a call-out slot belongs to none of the three helper classes"
        );
        debug_assert_eq!(
            usize::from(compilation.callout_interpret_one_slots),
            compilation.interpret_one_cells.len(),
            "every InterpretOne slot owns exactly one governor cell"
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
                compilation.callout_interpret_one_slots,
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
            self.chain_layouts.push(compilation.segment_layout);
            self.block_imm_lanes.push(compilation.imm_lanes);
            self.block_imm_lane_widths.push(compilation.imm_lane_widths);
            if index == self.block_portals.len() {
                self.block_portals.push(Arc::new(BlockPortal::new()));
            } else {
                debug_assert!(index < self.block_portals.len());
                self.block_portals[index].clear();
            }
            self.link_cells.push(compilation.link_cells.clone());
            self.interpret_one_cells
                .push(compilation.interpret_one_cells.clone());
            self.outbound.push([None, None]);
            self.dynamic_next_slots.push(0);
            self.block_link_epochs.push(0);
            self.block_active.push(true);
            self.iteration_upper_cache.push(0);
            self.callout_admission.push(CallOutAdmission::default());
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
            // A recycled slot must never inherit the retired occupant's WIDENED chain
            // requirement: the new block reaches a different successor set entirely.
            self.chain_layouts[index] = compilation.segment_layout;
            self.block_imm_lanes[index] = compilation.imm_lanes;
            self.block_imm_lane_widths[index] = compilation.imm_lane_widths;
            self.link_cells[index] = compilation.link_cells.clone();
            self.interpret_one_cells[index] = compilation.interpret_one_cells.clone();
            self.outbound[index] = [None, None];
            self.dynamic_next_slots[index] = 0;
            self.block_link_epochs[index] = 0;
            self.block_active[index] = true;
            // A recycled slot must not serve the retired occupant's cost bound to its successor.
            self.iteration_upper_cache[index] = 0;
            // Nor its learned call-out class, for the same reason.
            self.callout_admission[index] = CallOutAdmission::default();
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
    ///
    /// `retry_cause` is `Some` exactly when `reason` is `CompileRetry`: it is the compile walk's
    /// own answer, threaded here through `CompileOutcome::Retry` because this is the one site
    /// that runs once per compile attempt. The `keys` column is incremented only on the branch
    /// that actually parks the entry, which makes it a count of DISTINCT keys parked for that
    /// cause rather than of attempts.
    pub(crate) fn dormant(
        &mut self,
        key: BlockKey,
        reason: DormantReason,
        retry_cause: Option<RetryCause>,
    ) {
        debug_assert_eq!(
            retry_cause.is_some(),
            reason == DormantReason::CompileRetry,
            "a compile Retry park carries its cause and no other park has one"
        );
        self.stalls.dormant[reason as usize] += 1;
        if let Some(cause) = retry_cause {
            self.stalls.retry_causes[cause as usize] += 1;
        }
        if self.entries.get(&key) == Some(&BlockState::Seen) {
            if let Some(cause) = retry_cause {
                self.stalls.retry_cause_keys[cause as usize] += 1;
            }
            // A key that already spent its lift for THIS cause and came straight back parks
            // PERMANENTLY. The lift is an offer to re-walk once, and a re-park with the same
            // answer is the evidence that re-walking does not help; without this the key would
            // buy another 64 visits and another compile for ever.
            //
            // `retry_lift_reparks` counts PARK EVENTS, not distinct keys, and can therefore exceed
            // `retry_lifts` -- 10,265 against 1,961 on duke. That is not a defect and not a
            // treadmill: the permanence is enforced by `retry_lift_spent`, which this reads, so a
            // key past its lift never lifts again however often it re-parks. What produces the
            // extra events is a key returning to `Seen` by some OTHER path -- the SMC heat lift,
            // or `retire_key_for_recompile` after an invalidation -- and then re-parking on the
            // same gate. Read the pair as "the arm fired N times and its keys came back at least
            // N' times", not as a per-key rate.
            let permanent =
                retry_cause.is_some_and(|cause| self.retry_lift_spent.contains(&(key, cause)));
            if permanent {
                self.stalls.retry_lift_reparks += 1;
            }
            self.entries.insert(
                key,
                BlockState::Dormant(DormantEntry {
                    reason,
                    cause: retry_cause,
                    visits: 0,
                    permanent,
                }),
            );
        }
    }

    /// The RETRY lift: re-admit a key whose compile-walk failure is one that outside state can
    /// clear, after it has absorbed `RETRY_LIFT_VISITS` probes without being re-tried.
    ///
    /// `lift_cold_smc_dormant`'s sibling and deliberately not an extension of it. That one lifts
    /// keys the SMC heat gate parked and is driven by a stamp aging out; this one lifts keys the
    /// COMPILE WALK parked and is driven by a visit count, because there is no stamp to age: the
    /// walk failed on state (a decode line that was not resident, a translation that moved) which
    /// nothing about the key records. On the tombraid loader phase 194 of 466 dormant keys are
    /// `DecodeMiss` and they are never re-probed at all.
    ///
    /// WHAT A VISIT IS, stated because the number is meaningless without it. This is called from
    /// the `BlockProbe::Rejected` arm of `try_direct_continuation`, which the sticky-decline memo
    /// already throttles to roughly ONE per memo era per decode slot. So 64 visits is 64 memo
    /// eras, not 64 static-unbound exits, and the gate is a long-run one by construction rather
    /// than by the size of the constant.
    ///
    /// ONLY CLEARABLE CAUSES, and the discrimination is `RetryCause::clearable_by_retry`, an
    /// exhaustive match with no catch-all. A cap, the admission matrix or the min-length rule
    /// reaches the same answer on every walk for ever, and lifting one would be a compile per 64
    /// visits with a guaranteed park behind it.
    ///
    /// BOUNDED THREE WAYS: the visit gate, the one-lift-per-cause rule enforced by
    /// `retry_lift_spent` and read back in `dormant`, and the cap on that map. Past the cap no
    /// further key is lifted, which fails closed to today's behaviour.
    ///
    /// Answers whether the key is now `Seen`, because the caller has to know not to write a
    /// sticky-decline memo over a key it just re-admitted.
    pub(crate) fn lift_clearable_retry_dormant(&mut self, key: BlockKey) -> bool {
        // The whole mechanism, visit counting included, behind `IZARRAVM_RETRY_LIFT`. Default OFF
        // since 2026-08-22: see the gate for what duke measured and why the arm has to be
        // removable before anything else about that regression can be read.
        if !retry_lift_enabled() {
            return false;
        }
        let Some(BlockState::Dormant(entry)) = self.entries.get_mut(&key) else {
            return false;
        };
        if entry.permanent {
            return false;
        }
        let Some(cause) = entry.cause.filter(|cause| cause.clearable_by_retry()) else {
            return false;
        };
        entry.visits = entry.visits.saturating_add(1);
        if entry.visits < RETRY_LIFT_VISITS {
            return false;
        }
        if self.retry_lift_spent.len() >= RETRY_LIFT_SPENT_CAP {
            return false;
        }
        self.retry_lift_spent.insert((key, cause));
        self.entries.insert(key, BlockState::Seen);
        self.stalls.retry_lifts += 1;
        true
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

    /// The governor's class for one block, valid only under `epoch`. A stale epoch, a block that
    /// was never installed and a recycled slot all read `Untried(0)` -- the same miss-means-retry
    /// default `iteration_upper_cached` uses, and the reason no clear belongs in `retire_block`.
    pub(crate) fn callout_admission(&self, id: BlockId, epoch: u64) -> CallOutAdmission {
        if self.callout_admission_epoch != epoch {
            return CallOutAdmission::default();
        }
        self.active_index(id)
            .and_then(|index| self.callout_admission.get(index).copied())
            .unwrap_or_default()
    }

    pub(crate) fn set_callout_admission(
        &mut self,
        id: BlockId,
        epoch: u64,
        state: CallOutAdmission,
    ) {
        if self.callout_admission_epoch != epoch {
            self.callout_admission.fill(CallOutAdmission::default());
            self.callout_admission_epoch = epoch;
        }
        let Some(index) = self.active_index(id) else {
            return;
        };
        if let Some(slot) = self.callout_admission.get_mut(index) {
            *slot = state;
        }
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

    /// One-byte immediate lanes registered by an install — the imm8 share of the aggregate
    /// `smc_lane_registrations` the same install site feeds.
    pub(crate) fn note_imm8_lane_registrations(&mut self, lanes: u64) {
        self.stalls.imm8_lane_registrations += lanes;
    }

    /// Group-2 count lanes registered by an install — the L2 arm-2 share of the aggregate
    /// `smc_lane_registrations` the same install site feeds. Separate from
    /// `imm8_lane_registrations` even though both classes register at `IMM8_LANE_WIDTH`, because
    /// the two arms are independent knobs and a combined ladder leg has to be able to say which
    /// class moved.
    pub(crate) fn note_count_lane_registrations(&mut self, lanes: u64) {
        self.stalls.count_lane_registrations += lanes;
    }

    /// The packed first touch screened a slot that no longer had a line by the time the
    /// interpreted arm asked for it. See the field.
    pub(crate) fn note_decode_pack_late_view_miss(&mut self) {
        self.stalls.decode_pack_late_view_miss += 1;
    }

    /// Whether the cache is in its self-disabled state, in which `probe` synthesises `Rejected`
    /// for every key and `classify_rejected_probe` deliberately reports NO census class. A memo
    /// must not be written there or it would replay `DormantProbe` and break the census closure
    /// (design §1.4). The review's B1 remedy prefers reading this bool at the write site to
    /// advancing the era stamp on the `disabled` transitions, which have no site a `JitState`
    /// field could be advanced at.
    pub(crate) fn cache_disabled(&self) -> bool {
        self.disabled
    }

    /// One decline the sticky-decline memo answered without running the admission chain.
    pub(crate) fn note_decline_memo_hit(&mut self) {
        self.stalls.decline_memo_hits += 1;
    }

    /// One era-stamp advance, and whether it wrapped 63 -> 1 and therefore swept the pack array.
    pub(crate) fn note_decline_memo_advance(&mut self, swept: bool) {
        self.stalls.decline_memo_advances += 1;
        self.stalls.decline_memo_sweeps += u64::from(swept);
    }

    /// How many link targets are waiting for a block to appear. A block that publishes no
    /// successors must add none, which is what the segment-write bar means at this level.
    #[cfg(test)]
    pub(crate) fn waiting_len_for_test(&self) -> usize {
        self.waiting.len()
    }

    #[cfg(test)]
    pub(crate) fn set_lane_trial_for_test(&mut self, on: bool) {
        self.lane_trial_override = Some(on);
    }

    /// Park a key Dormant from whatever state it is in, so a fixture can reach a park shape the
    /// production path only produces after a specific compile failure.
    ///
    /// `dormant` insists the entry is exactly `Seen`, which is right for production (it is what
    /// keeps a park from clobbering a Compiled entry) and is the one thing a fixture cannot
    /// arrange for a key that is already parked for another reason.
    #[cfg(test)]
    pub(crate) fn park_dormant_for_test(
        &mut self,
        key: BlockKey,
        reason: DormantReason,
        cause: Option<RetryCause>,
    ) {
        self.entries.insert(key, BlockState::Seen);
        self.dormant(key, reason, cause);
    }

    /// Wind a parked key's visit counter forward, so a fixture that needs the lift to fire on its
    /// NEXT probe does not have to drive `RETRY_LIFT_VISITS` memo eras to get there.
    #[cfg(test)]
    pub(crate) fn set_dormant_visits_for_test(&mut self, key: BlockKey, visits: u8) {
        if let Some(BlockState::Dormant(entry)) = self.entries.get_mut(&key) {
            entry.visits = visits;
        }
    }

    /// Whether a key is parked at all, for fixtures that assert the lift did or did not move it.
    #[cfg(test)]
    pub(crate) fn is_dormant_for_test(&self, key: BlockKey) -> bool {
        matches!(self.entries.get(&key), Some(BlockState::Dormant(..)))
    }

    pub(crate) fn demote_smc_hot(&mut self, heat: &mut SmcHeatMap, key: BlockKey, epoch: u32) {
        self.dormant(key, DormantReason::SpanHot, None);
        let _ = heat.bump(key.physical, 1, epoch);
    }

    /// G1 recovery: a heat-demoted Dormant whose entry-chunk stamp has aged out (older epoch)
    /// returns to Seen, so the next probe walks the normal admission path (both heat gates
    /// re-check). Seen rather than a remove keeps the key tracked exactly once in `physical_keys`
    /// (the `retire_key_for_recompile` transition); the stamp is consumed, one recovery per demotion.
    ///
    /// Returns which of the three shapes this call took. `StillDormant` is EXACTLY the predicate
    /// the sticky-decline memo encodes — "was Dormant, did not lift"
    /// (`dev_docs/sticky-decline-memo-design.md` §1.4) — and the entry-state test it needs is one
    /// this function already performs, so reporting it costs nothing.
    pub(crate) fn lift_cold_smc_dormant(
        &mut self,
        heat: &mut SmcHeatMap,
        key: BlockKey,
        epoch: u32,
    ) -> DormantLift {
        if !matches!(self.entries.get(&key), Some(BlockState::Dormant(..))) {
            return DormantLift::NotDormant;
        }
        if heat.take_stale_stamp(key.physical, epoch) {
            self.entries.insert(key, BlockState::Seen);
            DormantLift::Lifted
        } else {
            DormantLift::StillDormant
        }
    }

    /// Record that the governor demoted the `InterpretOne` slot at this code site, so every later
    /// compile walk ends its block before the instruction instead of emitting a slot whose only
    /// remaining behaviour is the abnormal exit.
    ///
    /// Answers whether the site IS RECORDED afterwards, which is what the caller's retire has to
    /// be gated on. See `note_demotion` in `jit/direct/callout.rs` for why: a retire whose
    /// recompile will put the slot back is a demote/retire/recompile treadmill, and the one thing
    /// that stops the recompile re-admitting is this map holding the site.
    ///
    /// Full at `DEMOTED_CALLOUT_SITE_CAP` answers FALSE, and that is the whole reason this returns
    /// anything. The set is a COST policy, and a guest that somehow reached the cap is better
    /// served by paying the prologue test on the overflow sites than by an eviction policy that
    /// could thrash a site in and out of the allowlist -- but it must not also be paying a
    /// recompile per execution to learn the same refusal again. The cap is far above what a whole
    /// loader phase produces (64 sites on tombraid, 2026-08-22).
    ///
    /// A site that was ALREADY there answers TRUE, not false. It is recorded, so the recompile
    /// ends before the slot and the retire the caller then asks for terminates. That case is a
    /// block compiled before the site was learned, demoting afterwards, and it is exactly the
    /// block whose slot needs removing. Same precedent as `X87_TOP_RETIRE_CAP`: the CAP is what
    /// suppresses the retire, never the repeat.
    pub(crate) fn note_demoted_callout_site(&mut self, physical: u32, mode_key: u32) -> bool {
        if self.demoted_callout_sites.contains(&(physical, mode_key)) {
            return true;
        }
        if self.demoted_callout_sites.len() >= DEMOTED_CALLOUT_SITE_CAP {
            self.stalls.demoted_callout_sites_refused += 1;
            return false;
        }
        self.demoted_callout_sites.insert((physical, mode_key));
        true
    }

    /// Park a retire for `run_direct_block` to take after the native return. See the field.
    pub(crate) fn request_callout_block_retire(&mut self, key: BlockKey) {
        self.callout_retire_pending = Some(key);
    }

    /// Take whatever retire is parked, clearing it. Every exit from `run_direct_block` calls this,
    /// including the machine-stopping one: a latch left behind is read by the NEXT entry, which
    /// would then change the block cache for a demotion that happened inside a different run.
    pub(crate) fn take_callout_retire_pending(&mut self) -> Option<BlockKey> {
        self.callout_retire_pending.take()
    }

    /// Count one call-out step that ARMED the shadow, whatever row it was and whichever status it
    /// returned. Deliberately separate from the latch below: the two are written from the same
    /// place today, and a future edit that guards the latch on one status while leaving this
    /// count on every step makes the boundary's assertion fire instead of silently comparing
    /// against a stale address. That is the shape review finding F1 found.
    pub(crate) fn note_interrupt_shadow_arm(&mut self) {
        self.interrupt_shadow_arms = self.interrupt_shadow_arms.wrapping_add(1);
    }

    /// Latch the EIP an arming call-out slot left behind, stamped with the arm count as it stands
    /// now. See the fields.
    pub(crate) fn note_interrupt_shadow_armed(&mut self, end_eip: u32) {
        self.interrupt_shadow_armed_at = Some((end_eip, self.interrupt_shadow_arms));
    }

    /// Take the arming slot's EIP and its arm count, clearing the latch. Both the native return
    /// and the dispatcher entry call this, for the reason `take_callout_retire_pending` gives: a
    /// latch left behind would be read by an entry that did not set it.
    pub(crate) fn take_interrupt_shadow_armed(&mut self) -> Option<(u32, u32)> {
        self.interrupt_shadow_armed_at.take()
    }

    /// How many arming steps have left the shadow set. Read by the boundary's debug assertion.
    pub(crate) fn interrupt_shadow_arms(&self) -> u32 {
        self.interrupt_shadow_arms
    }

    /// Publish the shadow the block is being entered with, for `ResumeSnapshot::capture`.
    pub(crate) fn set_block_entry_interrupt_shadow(&mut self, armed: bool) {
        self.block_entry_interrupt_shadow = armed;
    }

    pub(crate) fn block_entry_interrupt_shadow(&self) -> bool {
        self.block_entry_interrupt_shadow
    }

    /// The M5 measurement: one interpreted SS load, classified by whether it moved the record.
    pub(crate) fn note_ss_load(&mut self, same_record: bool) {
        if same_record {
            self.stalls.ss_load_same_record += 1;
        } else {
            self.stalls.ss_load_changed_record += 1;
        }
    }

    /// Record that a block consumed an STI's shadow inside itself. See the field.
    pub(crate) fn note_interrupt_shadow_consumed(&mut self) {
        self.interrupt_shadow_consumed = true;
    }

    /// Take that record, clearing it, so the interrupt-transition test runs once per block.
    pub(crate) fn take_interrupt_shadow_consumed(&mut self) -> bool {
        std::mem::take(&mut self.interrupt_shadow_consumed)
    }

    /// Drop every demoted site the write at `physical..physical + width` lands on, because the
    /// code the judgement was about is being replaced.
    ///
    /// Called from BOTH of `note_code_write_inner`'s invalidation doors -- the compiled-block one
    /// and the decode-line one -- and from neither `invalidate_physical_range` nor the top of that
    /// function. Each of those three placements is wrong in its own way:
    ///
    /// * `invalidate_physical_range` alone misses most of it. A demotion RETIRES its block, so by
    ///   the time an overlay lands there is usually no compiled block left to make
    ///   `range_hits_compiled_code` true. The decode line is still live, because the interpreter
    ///   is now running that instruction.
    /// * the top of `note_code_write_inner` catches everything and costs too much: that is the
    ///   door every CHANGED byte store takes, watched or not, and a `retain` over sixty entries on
    ///   each of several million stores measured 1.15x min wall against this placement on the
    ///   tombraid loader (four interleaved pairs). The `is_empty` shortcut only helps a run that
    ///   has demoted nothing, which is every run until one does.
    /// * the two doors together cost nothing measurable: both have already established that the
    ///   write touches code, which is a tiny fraction of stores.
    ///
    /// Left stale, the ban follows the address into whatever code is written there next and only a
    /// whole-cache wipe lifts it: a permanent, silent coverage loss on an overlay-loading guest.
    ///
    /// The low end is widened by the longest x86 instruction. A site names the instruction's FIRST
    /// byte and the instruction runs to fifteen more, so a write landing inside it changes that
    /// instruction without touching the address the map is keyed on. Over-retaining costs one
    /// re-learn -- three resyncs and a recompile -- and under-retaining is the stale ban.
    ///
    /// The `is_empty` shortcut is what keeps this off the shipped store path in every run that
    /// never demotes a slot: one length load and a not-taken branch.
    pub(crate) fn forget_demoted_sites_in(&mut self, physical: u32, width: u32) {
        if self.demoted_callout_sites.is_empty() || width == 0 {
            return;
        }
        const MAX_INSTRUCTION_LEN: u32 = 15;
        let low = physical.saturating_sub(MAX_INSTRUCTION_LEN);
        let high = physical.saturating_add(width);
        self.demoted_callout_sites
            .retain(|&(site, _)| site < low || site >= high);
    }

    /// Whether the compile walk must treat this code site as a hard boundary.
    ///
    /// The emptiness test is first and is the shape that matters: on a guest that never demotes a
    /// slot -- which is every guest until one does -- this costs a length load per call-out slot
    /// and no hash at all.
    pub(crate) fn callout_site_demoted(&self, physical: u32, mode_key: u32) -> bool {
        !self.demoted_callout_sites.is_empty()
            && self.demoted_callout_sites.contains(&(physical, mode_key))
    }

    /// Sites currently in the demoted-call-out map. Read by the census gauge and, under `cfg(test)`
    /// as `demoted_callout_site_count_for_test`, by the fixtures -- one accessor, two names, so a
    /// test cannot drift from what the gauge reports.
    pub(crate) fn demoted_callout_sites_len(&self) -> usize {
        self.demoted_callout_sites.len()
    }

    /// Fill the demoted-site map to `DEMOTED_CALLOUT_SITE_CAP` with entries no fixture can
    /// collide with, so a test can reach the cap without four thousand real demotions.
    ///
    /// `u32::MAX` as the mode key is what makes them inert: `jit_mode_key` packs five bits and a
    /// persona rank, so no real key can equal it and no walk can match one of these.
    #[cfg(test)]
    pub(crate) fn fill_demoted_callout_sites_for_test(&mut self) {
        for physical in 0..DEMOTED_CALLOUT_SITE_CAP as u32 {
            self.demoted_callout_sites.insert((physical, u32::MAX));
        }
        assert_eq!(self.demoted_callout_sites.len(), DEMOTED_CALLOUT_SITE_CAP);
    }

    /// Whether `key` currently names an installed block. A read where
    /// `retire_key_for_recompile`'s bool is the same fact with a side effect, which a test that
    /// then asserts about a SECOND key cannot use.
    #[cfg(test)]
    pub(crate) fn key_is_compiled_for_test(&self, key: BlockKey) -> bool {
        matches!(self.entries.get(&key), Some(BlockState::Compiled(_)))
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

    /// How many times one key may be retired for an x87 TOP mismatch before it goes TOP-STICKY.
    ///
    /// The two bracketing measurements: 0 is the falsification arm
    /// (`.bench/results/tomb-turnover-falsify-20260815/`, +8.9% wall on `tombraid-586` with every
    /// guest counter and the frame hash identical), and `u8::MAX` is pre-slice `main`, whose
    /// 4.77M TOP-mismatch recompiles on that row bought zero native execution. 2 gives every key
    /// one re-specialization plus one spare, which is what a one-time TOP shift -- the only shape
    /// where the retire pays -- actually needs, and it is bounded per key by construction for
    /// every entry sequence. Not an env knob.
    const X87_TOP_RETIRE_CAP: u8 = 2;

    /// The x87 TOP-mismatch half of `retire_key_for_recompile`, capped per key.
    ///
    /// The entry REFUSAL is the caller's and is unconditional -- this decides only whether the
    /// block is also demoted so the next encounter re-specializes it at the then-live TOP. A key
    /// that has already spent its budget keeps its block and keeps refusing, so the worst case is
    /// permanent interpretation of that block: slow, never wrong. On a guest that cycles TOPs
    /// through one key the uncapped form recompiles forever and enters natively never.
    pub(crate) fn retire_key_for_top_mismatch(
        &mut self,
        watch: &mut NativeCodeWatch,
        key: BlockKey,
    ) -> bool {
        // Mirror `retire_key_for_recompile`'s own state check BEFORE touching the map: a key that
        // is not `Compiled` cannot be retired, and writing a count for it would leave the map
        // holding a key `entries` does not, breaking the containment the memory bound rests on.
        if !matches!(self.entries.get(&key), Some(BlockState::Compiled(_))) {
            return false;
        }
        // One lookup on the sticky path -- the one a churning guest takes millions of times. The
        // second lookup below is paid only by a retire that is actually about to happen, beside a
        // hot-table clear and a `retire_block`.
        let spent = *self.top_mismatch_retires.entry(key).or_insert(0);
        if spent >= Self::X87_TOP_RETIRE_CAP {
            self.stalls.x87_top_retires_suppressed += 1;
            return false;
        }
        self.top_mismatch_retires.insert(key, spent + 1);
        if spent + 1 == Self::X87_TOP_RETIRE_CAP {
            self.stalls.x87_top_sticky_crossings += 1;
        }
        self.retire_key_for_recompile(watch, key)
    }

    pub(crate) fn clear(&mut self, watch: &mut NativeCodeWatch) {
        // Unconditionally, and above the early return below. The one event that invalidates this
        // cache is a persona change, which arrives here through `CpuGsw::set_mode`, and an empty
        // block cache does not imply an unchanged persona. Clearing it here rather than inside
        // `reset_storage` removes a reachability argument standing between a mode switch and a
        // miscompiled quota.
        self.global_block_upper_cache = [0; 2];
        self.iteration_upper_cache.fill(0);
        self.callout_admission.fill(CallOutAdmission::default());
        // CS reloads and monitor transitions can invalidate code millions of times while the
        // direct cache is unused. Avoid clearing the 65,536-entry hot table when it is already
        // empty.
        if self.entries.is_empty() && self.blocks.is_empty() && self.arena.is_none() {
            // No second clear for `top_mismatch_retires`: its key set is a subset of `entries`'
            // (see the field), so an empty `entries` implies an empty map. One invariant, one
            // mechanism -- assert it here rather than papering over a violation.
            debug_assert!(
                self.top_mismatch_retires.is_empty(),
                "TOP-mismatch budgets outlived their entries"
            );
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
        // Every edge is gone, so every CHAIN requirement those edges justified is gone with it.
        // Monotonicity is a rule about a LIVE link graph; here there is none left, and the state
        // written back is exactly what `install` would write, which is the array's only other
        // writer. Without this a flushed block keeps demanding a segment nothing live reaches --
        // safe, but permanently over-strict, and it would refuse precisely the class-B edges this
        // mask exists to admit. Wholesale SMC and paging flushes make that the common case, not a
        // corner. See dev_docs/plans/2026-08-18-chain-used-link-mask.md.
        self.chain_layouts.copy_from_slice(&self.segment_layouts);
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
        #[cfg(feature = "smc-census")]
        let mut census_call = smc_census::CallAccum::default();
        if width == 0 || self.entries.is_empty() {
            // Classified too: the choke bumps `perf.smc_scan_calls` for this call, so leaving it
            // out would break the `scan_calls == perf.smc_scan_calls` closure.
            #[cfg(feature = "smc-census")]
            self.note_smc_census_call(0, 0, 0, &census_call);
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
            #[cfg(feature = "smc-census")]
            let mut census_page = smc_census::PageAccum::default();
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
                // Captured BEFORE the drain: this is the page occupancy design §12.6 says nothing
                // records today, and "24.2 keys/call" is the window length, not this.
                #[cfg(feature = "smc-census")]
                {
                    census_page.counts.page_visits = 1;
                    census_page.counts.page_keys_len_sum = keys.len() as u64;
                }
                let window_start = keys.partition_point(|tracked| tracked.physical < window_low);
                let window_end = window_start
                    + keys[window_start..]
                        .partition_point(|tracked| tracked.physical < window_high);
                let mut survivor_count = window_start;
                result.keys_scanned = result
                    .keys_scanned
                    .saturating_add(u32::try_from(window_end - window_start).unwrap_or(u32::MAX));
                #[cfg(feature = "smc-census")]
                {
                    census_page.counts.keys_scanned = (window_end - window_start) as u64;
                }
                for index in window_start..window_end {
                    let key = keys[index];
                    let Some(state) = self.entries.get(&key).copied() else {
                        #[cfg(feature = "smc-census")]
                        {
                            census_page.entries_get_misses += 1;
                        }
                        continue;
                    };
                    let overlaps = match state {
                        BlockState::Seen | BlockState::Dormant(..) => {
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
                        // Review finding M7: THIS is the quantity a per-page presence filter would
                        // elide, and it is the denominator R2's W is defined against.
                        #[cfg(feature = "smc-census")]
                        {
                            census_page.counts.keys_surviving += 1;
                            census_page.survivors_moved += 1;
                        }
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
                    //
                    // The width test is PER LANE since the L2 arm-1 slice, not against the one
                    // global `IMM_LANE_WIDTH`. It is still an exact-width test at an exact lane
                    // start, so nothing about the fail-closed argument changes: a lane accepts the
                    // store that rewrites its field WHOLE and no other. What the second class buys
                    // is that a one-byte `0x80` immediate patch is now such a store, where before
                    // no store of width 1 could ever be one.
                    if let BlockState::Compiled(id) = state
                        && lanes
                        && let Some(index) = self.active_index(id)
                    {
                        let block_lanes = self.block_imm_lanes[index];
                        let block_lane_widths = self.block_imm_lane_widths[index];
                        if physical != NO_IMM_LANE
                            && block_lanes.iter().zip(block_lane_widths.iter()).any(
                                |(lane, lane_width)| {
                                    *lane == physical && width == u32::from(*lane_width)
                                },
                            )
                        {
                            result.lane_accepts += 1;
                            #[cfg(feature = "smc-census")]
                            {
                                census_page.counts.lane_accepts += 1;
                                census_page.survivors_moved += 1;
                            }
                            keys[survivor_count] = key;
                            survivor_count += 1;
                            continue;
                        }
                        for (lane, _) in block_lanes
                            .iter()
                            .copied()
                            .zip(block_lane_widths.iter().copied())
                            .filter(|(lane, lane_width)| {
                                *lane != NO_IMM_LANE
                                    && physical_ranges_overlap(
                                        physical,
                                        width,
                                        *lane,
                                        u32::from(*lane_width),
                                    )
                            })
                        {
                            if physical == lane {
                                result.lane_reject_width += 1;
                            } else {
                                result.lane_reject_address += 1;
                            }
                        }
                    }

                    self.entries.remove(&key);
                    // Below the lane `continue` above deliberately: a patched imm32 lane keeps
                    // its block and its `entries` row, so it must keep its budget too. A genuine
                    // kill drops all three -- the code at this address is NEW and inheriting the
                    // previous occupant's stickiness would pin it out of specialization silently.
                    //
                    // The demoted-site map is the fourth thing that has to go and is NOT dropped
                    // here: it is keyed on the INSTRUCTION rather than on the block entry, and it
                    // outlives the block. By the time an overwrite arrives the demoted block has
                    // usually been retired already, so this arm is not even reached --
                    // `range_hits_compiled_code` is what gates the call. `forget_demoted_sites_in`
                    // is called from the code-write door instead.
                    self.top_mismatch_retires.remove(&key);
                    let hot_index = key.hot_index();
                    if self.hot[hot_index].is_some_and(|hot| hot.key == key) {
                        self.hot[hot_index] = None;
                    }
                    match state {
                        BlockState::Rejected(span) => {
                            watch.release_range(span.key.physical, u32::from(span.guest_len));
                        }
                        BlockState::Compiled(id) => self.retire_block(watch, id),
                        BlockState::Seen | BlockState::Dormant(..) => {}
                    }
                    invalidated += 1;
                    #[cfg(feature = "smc-census")]
                    {
                        census_page.counts.keys_killed += 1;
                    }
                }
                // Survivors compacted into [window_start, survivor_count);
                // close the kill hole so the untouched tail keeps the sorted
                // order the window search depends on.
                if survivor_count != window_end {
                    #[cfg(feature = "smc-census")]
                    {
                        census_page.drain_calls += 1;
                        census_page.drain_elements += (window_end - survivor_count) as u64;
                    }
                    keys.drain(survivor_count..window_end);
                }
                if !keys.is_empty() {
                    #[cfg(feature = "smc-census")]
                    {
                        census_page.reinserted = true;
                    }
                    self.physical_keys.insert(page, page_keys);
                }
                #[cfg(feature = "smc-census")]
                {
                    census_call.pages_present += 1;
                    census_call.keys_surviving += census_page.counts.keys_surviving;
                    // Q2's PER-PAGE question, which the per-call split cannot answer: a call
                    // spanning two pages can kill on one and scan the other for nothing.
                    census_page.counts.no_kill_visits =
                        u64::from(census_page.counts.keys_killed == 0);
                    self.note_smc_census_page(page, &census_page);
                }
            } else {
                #[cfg(feature = "smc-census")]
                self.note_smc_census_absent_page();
            }
            cursor = cursor.wrapping_add(step);
            remaining -= step;
        }
        result.blocks = invalidated;
        #[cfg(feature = "smc-census")]
        self.note_smc_census_call(
            invalidated as u64,
            u64::from(result.lane_accepts),
            u64::from(result.keys_scanned),
            &census_call,
        );
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
    /// The `RetryCause` a dormant key was parked with, or `None` when it is not a compile-Retry
    /// park at all (the heat gate, the page-cover failure and the install failure carry no cause).
    ///
    /// A LOOKUP rather than a second return value from `classify_unbound_target`, because only the
    /// census lane wants it: the class table is read on every static-unbound exit in every build,
    /// and this is read only when the census is armed and the class came back `DormantOther`.
    pub(crate) fn dormant_retry_cause(&self, key: BlockKey) -> Option<RetryCause> {
        match self.entries.get(&key) {
            Some(BlockState::Dormant(entry)) => entry.cause,
            _ => None,
        }
    }

    pub(crate) fn classify_unbound_target(&self, key: BlockKey) -> UnboundTarget {
        match self.entries.get(&key) {
            None => UnboundTarget::Absent,
            Some(BlockState::Seen) => UnboundTarget::Seen,
            Some(BlockState::Dormant(entry)) if entry.reason == DormantReason::SpanHot => {
                UnboundTarget::DormantHeat
            }
            Some(BlockState::Dormant(..)) => UnboundTarget::DormantOther,
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
            Some(BlockState::Dormant(..)) => Some(AdmissionDecline::DormantProbe),
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
        self.chain_layouts.clear();
        self.block_imm_lanes.clear();
        self.block_imm_lane_widths.clear();
        self.lane_trial_epochs.clear();
        self.top_mismatch_retires.clear();
        self.demoted_callout_sites.clear();
        self.callout_retire_pending = None;
        self.retry_lift_spent.clear();
        // The three interrupt-shadow latches. A reset drops every block, so a latch surviving it
        // would be read at the next boundary by an entry that did not set it -- the reason
        // `callout_retire_pending` is cleared on the line above (review finding F5).
        self.interrupt_shadow_armed_at = None;
        self.interrupt_shadow_arms = 0;
        self.interrupt_shadow_consumed = false;
        self.block_entry_interrupt_shadow = false;
        self.link_cells.clear();
        self.interpret_one_cells.clear();
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
        self.callout_admission.clear();
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
        //
        // The segment arm compares the two ends' CHAIN requirements, not their own snapshots, and
        // the merge it computes here IS the decision: on conflict this refuses with nothing
        // written, and on success the merged requirement is handed to `widen_chain_requirement`
        // AFTER the link is published. The propagation never re-decides this edge.
        //
        // Computed BEHIND the epoch test, not beside it. That is the same short-circuit the split
        // if-chain preserves, and it is load-bearing twice over: a stale index's layout entry is
        // not meaningful, and the epoch arm is a high-frequency refusal that must not start paying
        // for a six-segment merge it never consults.
        let stale_epoch = self.block_link_epochs.get(source_index).copied()
            != Some(self.link_epoch)
            || self.block_link_epochs.get(target_index).copied() != Some(self.link_epoch);
        let chain_merge = (!stale_epoch)
            .then(|| self.chain_layouts[source_index].link_merge(self.chain_layouts[target_index]))
            .flatten();
        let refusal = if stale_epoch {
            Some(LinkRefusal::StaleEpoch)
        } else if chain_merge.is_none() {
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
        // AFTER the edge is visible: the propagation walks `inbound`, and this edge's own source
        // may itself be someone's target. `chain_merge` was proved `Some` by the refusal chain
        // above.
        if let Some(merged) = chain_merge {
            self.widen_chain_requirement(source, merged);
        }
        true
    }

    /// Absorb `merged` as `source`'s chain requirement and push the widening backwards until it
    /// settles. Called only from `try_link_inner`, only after the edge is published.
    ///
    /// The obligation being restored is: for every live edge `P -> Q`, `chain(Q).used` is a
    /// subset of `chain(P).used` and the two agree on every descriptor in `chain(Q).used`. A
    /// widen at `Q` can break that for `Q`'s PREDECESSORS, so the walk is inbound-only; it cannot
    /// break it for `Q`'s successors, because their obligation ranges over their own (unchanged)
    /// masks, and a bit new to `Q` cannot already be in a successor's mask -- if it were, `Q`
    /// would have had it before this widen.
    ///
    /// A predecessor that cannot absorb the widen -- its own frozen descriptor for one of the new
    /// segments disagrees -- has its edge CUT. That arm is reachable and load-bearing even under
    /// the non-adopting merge: equality is demanded at link time over a mask that later GROWS,
    /// so an edge admitted because nobody pinned ES becomes unsound the moment a block downstream
    /// of the target pins ES with a different descriptor.
    ///
    /// Termination: a requirement bit, once set, is never cleared while the block lives, and
    /// there are six of them, so each block is pushed at most six times per generation.
    fn widen_chain_requirement(&mut self, source: BlockId, merged: SegmentLayout) {
        let Some(source_index) = self.active_index(source) else {
            return;
        };
        // ABOVE the no-change return, deliberately. Non-adoption is a property of every merge this
        // function is ever handed, not only of the ones that widen something -- a merge that
        // rewrote the block's descriptors while leaving the mask alone is exactly the silent
        // failure this asserts against, and behind the return it would never be checked.
        debug_assert_eq!(
            self.chain_layouts[source_index].data, merged.data,
            "the non-adopting merge never rewrites a block's own descriptors",
        );
        if self.chain_layouts[source_index].used == merged.used {
            return;
        }
        debug_assert_eq!(
            self.chain_layouts[source_index].used & merged.used,
            self.chain_layouts[source_index].used,
            "a chain requirement may only ever widen",
        );
        self.chain_layouts[source_index] = merged;
        let mut worklist = vec![source];
        while let Some(widened) = worklist.pop() {
            let Some(widened_index) = self.active_index(widened) else {
                continue;
            };
            let requirement = self.chain_layouts[widened_index];
            // Snapshot: `unlink_outbound` below edits this very vector.
            let Some(inbound) = self.inbound.get(&widened).cloned() else {
                continue;
            };
            for link in inbound {
                // An `inbound` entry can name a block whose slot has since been recycled. Widening
                // or cutting on that index would touch a DIFFERENT block's edges; the retirement
                // walk in `unlink_block` guards the same way.
                let Some(predecessor_index) = self.active_index(link.block) else {
                    continue;
                };
                match self.chain_layouts[predecessor_index].merge_chain(requirement) {
                    Some(predecessor_merged) => {
                        if predecessor_merged.used != self.chain_layouts[predecessor_index].used {
                            self.chain_layouts[predecessor_index] = predecessor_merged;
                            worklist.push(link.block);
                        }
                    }
                    None => {
                        self.unlink_outbound(link.block, link.slot, LinkClearCause::ChainWiden);
                    }
                }
            }
        }
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
            #[cfg(feature = "smc-census")]
            self.note_smc_census_unlink(false, 0, 0);
            return;
        };
        #[cfg(feature = "smc-census")]
        let mut census_walked = 0u64;
        #[cfg(feature = "smc-census")]
        let mut census_reparked = 0u64;
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
                #[cfg(feature = "smc-census")]
                {
                    census_walked += 1;
                }
                let source_index = link.block.index();
                if self.active_index(link.block) == Some(source_index) {
                    let slot = usize::from(link.slot);
                    self.link_cells[source_index][slot].clear();
                    self.outbound[source_index][slot] = None;
                    if let Some(successor) = self.blocks[source_index].successors[slot] {
                        // Review finding M9: `link.block` is the SOURCE, so a self-linking block
                        // re-parks a waiting entry naming `id` right here. That is why the second
                        // `remove_waiting_sources` below is load-bearing and R6's "drop the second
                        // pass" arm cannot be proved. This counter measures how often it happens.
                        #[cfg(feature = "smc-census")]
                        if link.block == id {
                            census_reparked += 1;
                        }
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
        #[cfg(feature = "smc-census")]
        self.note_smc_census_unlink(true, census_walked, census_reparked);
    }

    fn remove_waiting_sources(&mut self, id: BlockId) {
        #[cfg(feature = "smc-census")]
        let census_map_len = self.waiting.len() as u64;
        #[cfg(feature = "smc-census")]
        let mut census_visited = 0u64;
        self.waiting.retain(|_, sources| {
            #[cfg(feature = "smc-census")]
            {
                census_visited += sources.len() as u64;
            }
            sources.retain(|source| source.block != id);
            !sources.is_empty()
        });
        // Phase (e). `retain` walks the WHOLE waiting map, not this block's sources, and
        // `unlink_block` calls it twice — so `waiting_map_len_sum` is what prices the pass, not
        // the call count.
        #[cfg(feature = "smc-census")]
        self.note_smc_census_waiting_retain(
            census_map_len,
            census_visited,
            census_map_len - self.waiting.len() as u64,
        );
    }

    fn retire_block(&mut self, watch: &mut NativeCodeWatch, id: BlockId) {
        let Some(index) = self.active_index(id) else {
            // §12.9: a second key naming the same block finds it already retired. Counted, so the
            // per-key kill count and the per-block death count stay separately visible.
            #[cfg(feature = "smc-census")]
            self.note_smc_census_retire(false, 0, 0);
            return;
        };
        let span = self.blocks[index].span;
        #[cfg(feature = "smc-census")]
        let census_decode_slots = self.block_decode_slots[index].len() as u64;
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
        self.block_imm_lane_widths[index] = [0; MAX_BLOCK_IMM_LANES];
        self.free_block_slots
            .push(u16::try_from(index).expect("block slot index must fit its ID"));
        self.live_blocks -= 1;
        watch.release_range(span.key.physical, u32::from(span.guest_len));
        #[cfg(feature = "smc-census")]
        self.note_smc_census_retire(true, u64::from(span.guest_len), census_decode_slots);
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
        #[cfg(feature = "smc-census")]
        {
            cache.smc_census = None;
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
    /// The walk gave up without naming a rejected span. The payload is WHICH gate gave up; it
    /// reaches `BlockCache::dormant` unchanged and is the whole of `DirectStallTally`'s
    /// `retry_causes` column.
    Retry(RetryCause),
}

#[cfg(test)]
impl CompileOutcome {
    pub(crate) fn expect(self, message: &str) -> Compilation {
        match self {
            Self::Compiled(compilation) => compilation,
            Self::StructuralReject(_) | Self::Retry(_) => panic!("{message}"),
        }
    }

    pub(crate) fn unwrap(self) -> Compilation {
        self.expect("called CompileOutcome::unwrap() on a non-compiled outcome")
    }

    pub(crate) fn unwrap_or_else(self, fallback: impl FnOnce() -> Compilation) -> Compilation {
        match self {
            Self::Compiled(compilation) => compilation,
            Self::StructuralReject(_) | Self::Retry(_) => fallback(),
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
    /// Slots whose helper runs one interpreter instruction; see `CallOutHelper::interprets_one`.
    /// The length of `interpret_one_cells`, asserted at install.
    pub callout_interpret_one_slots: u8,
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
    /// One governor cell per `InterpretOne` slot, in slot order; see `BlockCache`'s field.
    pub interpret_one_cells: Vec<Arc<InterpretOneCell>>,
    body_offset: usize,
    /// Physical start of each mutable immediate this block's emitted code reads from guest RAM,
    /// `NO_IMM_LANE` for an unused slot. `install` copies these into the cache's per-block lane
    /// array, which is what the SMC write choke matches a patch against.
    imm_lanes: [u32; MAX_BLOCK_IMM_LANES],
    /// The width class of each `imm_lanes` entry, `0` for an unused slot. Copied into the cache's
    /// `block_imm_lane_widths` beside the addresses; see that field for why the two are parallel
    /// arrays rather than one array of pairs.
    imm_lane_widths: [u8; MAX_BLOCK_IMM_LANES],
    /// How many of `imm_lanes` are DISPLACEMENT lanes (`disp_lane_for`). The write choke never
    /// needs the distinction — a lane is a lane there — but the install site does: the split
    /// between `smc_lane_registrations` and `disp_lane_registrations` is what says which lane
    /// kind an A/B's `smc_lane_accepts` movement belongs to.
    disp_lanes: u8,
    /// How many of `imm_lanes` are ONE-BYTE lanes (`imm8_lane_for`), for the same reason
    /// `disp_lanes` exists: the L2 arm-1 A/B has to be able to say that a `smc_lane_accepts`
    /// movement came from the new class rather than from the `0x81` family shifting under it.
    imm8_lanes: u8,
    /// How many of `imm_lanes` are GROUP-2 COUNT lanes (`count_lane_for`), for the same reason
    /// `imm8_lanes` exists and NOT folded into it even though both are `IMM8_LANE_WIDTH`: the two
    /// classes are selected by independent knobs, so a `smc_lane_accepts` movement on a combined
    /// ladder leg has to be attributable to one of them.
    count_lanes: u8,
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

    pub(crate) fn imm8_lane_count(&self) -> usize {
        usize::from(self.imm8_lanes)
    }

    pub(crate) fn count_lane_count(&self) -> usize {
        usize::from(self.count_lanes)
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
///
/// `width` is the lane's WIDTH CLASS in bytes — `IMM_LANE_WIDTH` for the `0x81` dword family and
/// `disp_lane_for`'s disp32 field, `IMM8_LANE_WIDTH` for the `0x80` imm8 family. It travels with
/// the lane all the way to the cache's `block_imm_lane_widths` so the write choke can test a patch
/// against the width of the lane it landed on rather than against a single global width.
#[derive(Clone, Copy)]
pub(crate) struct ImmLane {
    pub(crate) physical: u32,
    pub(crate) host: usize,
    pub(crate) width: u8,
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
    /// `data_matches`/`all_data_matches` for the other five).
    ///
    /// A CHAINED path reaches this slot without an entry check of its own, and what covers that is
    /// the `used` mask below, not any rule about equal snapshots -- links stopped requiring those
    /// in the 2026-08-18 chain-used mask slice. Because `selector_segment` puts the segment in
    /// `used`, it is in this block's chain requirement, and every edge on the path here had to
    /// agree with that requirement (`SegmentLayout::link_merge`) or be refused. So no chained path
    /// reaches this slot under a different selector, and a link made downstream that would break
    /// the agreement cuts the inbound edge instead.
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
    /// `POP Sreg` (0x07 ES, 0x1F DS) on a SIXTEEN-BIT stack at Word operand size, in REAL MODE or
    /// V86 -- the tombraid loop-A census's largest row, behind `IZARRAVM_V86_LOOP_ROWS`.
    ///
    /// One kind, not two, because only one width exists. `classify` refuses the Dword form in its
    /// own arm (there the interpreter pops FOUR bytes and loads the low 16), so `stack_width_kind`
    /// only ever sees this at Word and maps it through unchanged on a 16-bit stack; a 32-bit stack
    /// falls to that matrix's `_ => None` and the row stays a barrier there. That is deliberately
    /// the opposite shape from `Pop`/`Pop16`, which have both widths because both are measured.
    ///
    /// It is `Pop16`'s read followed by `LoadSegReal`'s write, and both halves are unchanged from
    /// the arms that already ship them, including the two things about each that are easy to get
    /// wrong: the pointer advance is a 16-bit register op (`alu_r16_imm16` on the SP home), so
    /// ESP's high half survives; and there is deliberately NO limit store on the segment write,
    /// because a real-mode segment load leaves the cached limit alone -- see `LoadSegReal`'s emit
    /// arm for the full argument, which holds here for the same reason. The one thing it does NOT
    /// inherit is `Pop16`'s POP SP ordering note: the destination is a segment register, so it can
    /// never alias the stack pointer this instruction is advancing.
    ///
    /// Every guest-visible write sits after every side-exit guard, so a memory exit leaves the
    /// instruction un-started exactly as `Pop16`'s does.
    ///
    /// `written_segment` reports the segment, which makes any block holding this slot a
    /// SEGMENT-WRITE block: the compile walk's dirty-segment rule ends the block at the first
    /// later slot that bakes anything from that segment, the self-loop shape is barred, and no
    /// static link is attempted. All three are inherited from `LoadSegReal` rather than added
    /// here.
    PopSegReal {
        segment: SegmentIndex,
    },
    /// `CLC` (0xF8) and `STC` (0xF9) -- the tombraid loop-A census's `0xf8` row at 95,090,745
    /// interpreted hits, behind `IZARRAVM_V86_LOOP_ROWS`.
    ///
    /// The interpreter is one line, `set_flag(FLAG_CF, set)`, and `emit_set_cf_only` is already a
    /// transcription of exactly that function's CF path -- both of its branches, the live-descriptor
    /// one that reproduces `PendingFlags::with_cf_override` in place and the bare one that writes
    /// EFLAGS directly, plus the trailing `eflags |= 0x2` that `set_flag` does on both. The rotate
    /// rows have been driving it from a CAPTURED carry since 2026-08-09; the only difference here
    /// is that the bit put into the flag shadow is a compile-time constant.
    ///
    /// No width field and no width bar of its own: neither instruction consults `operand_size`,
    /// so the Word and Dword forms are the same operation. `0xf8`/`0xf9` reach the Word allowlist
    /// only under the gate, which is a MEASUREMENT boundary rather than a correctness one.
    ///
    /// Raw clocks ride the `_ => 2` default, which is what the interpreter charges.
    CarryFlag {
        set: bool,
    },
    /// `SETcc r8` (0F 90..9F, register destination). The guest condition encoding is x86's own,
    /// so the emitted `setcc` takes it unchanged; `condition()` in the interpreter is the same
    /// truth table the host flags implement.
    SetCc {
        condition: u8,
        dst: u8,
    },
    /// `SETcc m8` (0F 90..9F, MEMORY destination) -- the tombraid FMV census's `0x0F94 /0` row at
    /// 27,602,402 interpreted hits, behind `IZARRAVM_FPU_LOOP_ROWS`.
    ///
    /// A separate kind from `SetCc` rather than a `StoreSource` on `Store`, because of WHERE the
    /// value has to be computed. `emit_store` materialises its source LAST, inside the page-kind
    /// arm, after the address, the permission check and the write-pointer resolve have all
    /// clobbered RAX/RCX/RDX/RDI -- and the value here is `popfq`-derived from RBP, which cannot
    /// run there without disturbing the store's own live scratch. So the byte is computed FIRST,
    /// parked in the frame, and the store reads it back: see `StoreSource::ParkedByte` and
    /// `emit_set_cc_mem`.
    ///
    /// ALL SIXTEEN conditions, not just `/4` SETE. The condition is a raw four-bit code handed
    /// straight to `Encoder::setcc`, identical to what the register form above already does with
    /// it, so there is no per-condition correctness question to measure separately -- and the
    /// closure rule stated at the top of `classify` ("admitting one member of a shared arm while
    /// refusing its sibling would be arbitrary") applies to this arm exactly as it does there.
    ///
    /// Byte-wide by FORM, whatever the operand-size prefix says: the interpreter's arm calls
    /// `write_operand_u8` without consulting `operand_size`. `0x0f9x` is absent from classify's
    /// Word-size allowlist, so a 66-prefixed encoding never reaches the arm and an unprefixed one
    /// in a CS.D = 0 segment does not either -- which is the width bar for this row, and it is the
    /// SAME bar the register form has always had.
    SetCcMem {
        condition: u8,
        addr: DirectAddr,
    },
    /// `SAHF` (0x9E) -- the tombraid FMV census's second-largest loop-B row at 55,203,044
    /// interpreted hits, behind `IZARRAVM_FPU_LOOP_ROWS`.
    ///
    /// The interpreter is three lines: `materialize_flags()`, then
    /// `eflags = (eflags & !0xd5) | (ah & 0xd5) | 0x02`. Every bit in `0xd5` (CF, PF, AF, ZF, SF)
    /// is inside `ARITH_FLAGS`, so this touches nothing the lazy descriptor does not already own,
    /// and OF -- the sixth `ARITH_FLAGS` member -- is deliberately PRESERVED.
    ///
    /// No width field and no width bar of its own: SAHF has identical semantics at either operand
    /// size (it reads AH and writes five EFLAGS bits, neither of which is width-dependent), and
    /// `0x9e` is absent from classify's Word-size allowlist so the Word form stays a barrier. That
    /// absence is a MEASUREMENT boundary, not a correctness one -- the census row is dword and no
    /// fixture measures a 16-bit SAHF -- and it is stated here because the kind carries no width to
    /// bar on.
    Sahf,
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
    /// LEA r16/r32, m. `width` is the OPERAND size and decides how much of the effective address
    /// reaches the destination: Dword replaces the register, Word merges sixteen bits and leaves
    /// the high half alone, exactly as `write_gpr_sized(reg, Word, offset)` does.
    ///
    /// The field is what the Word admission needed. The arm previously ended in `mov_r32_r32`,
    /// which defines all 32 bits, so a 66-prefixed LEA would have clobbered the destination's
    /// high half. This is the same fix MOVZX/MOVSX got with `dst_width` and `Shift` with `width`.
    ///
    /// ADDRESS size is a separate question and does not live here: it rides the block-level
    /// `address_wrap`, which `emit_effective_address` consumes. The two are independent, and the
    /// `lea16_at_a_dword_address_size_keeps_the_high_half` fixture is the cell that separates
    /// them.
    Lea {
        dst: u8,
        addr: DirectAddr,
        width: MemoryWidth,
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
        /// Present only for the shapes `imm8_lane_for` admits (the reg-destination `0x80 /r`
        /// byte family, no prefixes). When present the emitted form IGNORES `imm` and loads the
        /// one immediate byte out of guest RAM on every execution, so a guest patch of that byte
        /// needs no recompile. `imm` still carries the value decoded at compile time and is what
        /// the non-lane form bakes.
        ///
        /// A runtime immediate is FLAG-NEUTRAL here, and that is why this family shipped as L2's
        /// first arm while the group-2 count byte did not: `emit_alu_byte_preloaded` computes the
        /// flags from the host operation's own result at run time whatever the source operand is
        /// (`emit_alu_reg_byte` already feeds it a register), so there is no compile-time split on
        /// the immediate's VALUE to preserve. `emit_rotate_reg`, by contrast, picks its capture
        /// mask at emission from the count — see `rotate_rows_enabled`'s "THE DESIGN COST".
        lane: Option<ImmLane>,
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
    /// `TEST r/m16|32, r16|32` (0x85), REGISTER form -- AND for flags with no write-back.
    ///
    /// `width` arrived with the 2026-08-21 duke slice and is the whole of that slice on this kind.
    /// It is the same move `MovImm`, `AluImm`, `Load` and `Store` each made in their own slice:
    /// the kind used to hard-code Dword, which is why `0x85` is named in the classify header's
    /// "deliberately NOT here" list as a Dword sibling with no width field. Now that it carries
    /// one, the entry on that list is discharged rather than excepted, and the Word admission is a
    /// GATED allowlist term (`test_word_rows_enabled`) rather than an unconditional one.
    ///
    /// **The Dword emission is untouched, deliberately.** `emit` keeps the original `emit_test`
    /// for `MemoryWidth::Dword` and routes only Word to `emit_test_preloaded`, so the gate-OFF arm
    /// is byte-identical to the pre-slice tree by inspection: with the gate off no Word `Test` is
    /// ever constructed, and every Dword one reaches the emitter it always did.
    ///
    /// The MEMORY form is still refused in the arm, at both widths, and the census says nothing
    /// asks for it: duke3d-586's whole `0x85` population is register-form (twelve rows,
    /// 53,583,389 runtime hits) with **zero** memory rows.
    Test {
        a: u8,
        b: u8,
        width: MemoryWidth,
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
        /// Present only for the shapes `count_lane_for` admits (the register-destination `0xC1`
        /// form, no prefixes, `imm_len == 1`, `len == 3`). When present the emitted form IGNORES
        /// `count` and loads the count byte out of guest RAM on every execution, so a guest patch
        /// of that byte needs no recompile. `count` still carries the value decoded at compile time
        /// and is what the non-lane form bakes.
        ///
        /// **A runtime count is NOT flag-neutral here, and that is this lane's whole cost.**
        /// `emit_rotate_reg` picks its capture mask at EMISSION from the masked count -- 0 emits
        /// nothing at all, 1 captures `CF|OF` and publishes the shadow, 2..31 captures CF and goes
        /// through `emit_set_cf_only`. The lane form has to reproduce that split as a RUNTIME
        /// three-way branch (`emit_rotate_reg_lane`), which is what `rotate_rows_enabled`'s "THE
        /// DESIGN COST" paragraph priced and what kept this family out of L2 arm 1.
        lane: Option<ImmLane>,
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
    /// DIV (0xF7 /6) and IDIV (0xF7 /7) r/m32, MEMORY form; `signed` selects IDIV. Behind
    /// `IZARRAVM_FPU_LOOP_ROWS`. The tombraid FMV census's `0xF7 /7 memory dword` row is
    /// 27,602,949 interpreted hits; `/6` rides the same shared classifier arm by the closure rule
    /// (`DivReg`'s arm is `matches!(m.reg, 6 | 7)`, and splitting the pair at the memory form
    /// would be exactly the arbitrary split the top of `classify` warns against).
    ///
    /// `DivReg`'s comment used to say the memory form was "deliberately absent, and the reason is
    /// the fault rather than the address: a memory DIV can side-exit for two independent reasons --
    /// the read's own guards and the divide guard -- at the same slot, and the second must not be
    /// reachable before the first has been proved not to fire." **That ordering requirement is
    /// what this kind's emitter is built around** and it is stricter than it first sounds, because
    /// the hazard is not the ORDER of the exits, it is the mode-13 read COUNTER that sits between
    /// them. `emit_ram_read_pointer` deposits `mode13_*_reads` into the frame before it returns,
    /// and those lanes are copied out on EVERY exit -- so a divide-guard exit taken after the
    /// deposit would charge the read once natively and once more when the interpreter re-executes
    /// the instruction whole.
    ///
    /// `emit_div_mem` therefore uses the DEFERRED-completion shape that `Ret` and `JmpMem` already
    /// use for their CS-limit exit: `emit_ram_read_pointer_inner` (which moves no counter), then
    /// every divide guard including the post-divide quotient-range one, then the home write-back,
    /// and only then `emit_mode13_read_completion`. Every exit out of this slot is therefore
    /// pre-deposit and pre-effect.
    ///
    /// No `width` field, for `DivReg`'s reason exactly: classify's `OperandSize::Word` gate
    /// excludes `0xf7` (bar `/0` TEST), so the arm is unreachable at Word, and a `width` field
    /// would invite a future edit to pass `operand_width` and lower a 16-bit divide as a 32-bit
    /// one. The bar is on the KIND -- this kind is dword-only by construction -- rather than on
    /// the absence of a 0x66 prefix, which is why an UNPREFIXED `F7 /7 mem` in a CS.D = 0 segment
    /// (`OperandSize::Word`, prefix mask 0) is refused too.
    DivMem {
        addr: DirectAddr,
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
        /// Present only for the shapes `count_lane_for` admits: the register-destination `0xC1
        /// /4..=7` (Dword) and `0xC0 /4` (Byte) forms, no prefixes, `imm_len == 1`, `len == 3`.
        /// See `RotateReg::lane` for why a runtime count costs a three-way branch, and
        /// `emit_shift_lane` for the branch itself.
        ///
        /// **Word can never carry one, and `count_lane_for` bars it EXPLICITLY on this field.**
        /// The first version of this slice barred it by inference instead — "a Word `0xC1` needs a
        /// `0x66`, which fails both the prefix bar and `len == 3`" — and that inference is FALSE in
        /// a 16-bit code segment, where the operand size follows CS.D and not a prefix. An
        /// unprefixed `c1 e0 03` in a CS.D=0 segment is `shl ax, 3`: `Prefixes::default()`,
        /// `disp_len 0`, `imm_len 1`, `len 3`, `width: Word`. `0xC1` is on classify's Word
        /// allowlist, so it reached the emitter, which has no CL-form Word lane and panicked the
        /// compiler. The width test is the bar and
        /// `a_word_group_two_shift_in_a_sixteen_bit_segment_takes_no_count_lane` is the regression
        /// fixture. (`RotateReg` needs no such field: classify refuses both rotates at Word
        /// outright.)
        lane: Option<ImmLane>,
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
    /// LEAVE (0xC9) at DWORD operand size on a 32-bit stack: `ESP <- EBP` then `POP EBP`.
    /// Fieldless because the instruction has no operands and this variant stands for one cell of
    /// the (operand size x SS.B) matrix. It cannot be spelled as `Pop { dst: 5 }` plus a register
    /// move, because `raw_clocks`, `read_segment` and the dword-read membership all key on the
    /// variant.
    ///
    /// The Word cells are `Leave16` below. The fourth cell, Dword operand on a 16-bit stack, is
    /// still refused: it would move four bytes with a 16-bit pointer, which is a miscompile
    /// rather than a missed lowering. `stack_width_kind` is what says so.
    Leave,
    /// LEAVE (0xC9) at WORD operand size, on either stack width. `stack32` is SS.B, resolved in
    /// `stack_width_kind` because `classify` has no CPU; it is safe to bake because SS.B is bit 3
    /// of `jit_mode_key`, so a block compiled for one stack width can never be entered with the
    /// other.
    ///
    /// The two halves of the instruction follow DIFFERENT widths and that is the whole content of
    /// this variant (386 PRM 17-96). The pointer move follows SS.B: a 32-bit stack moves the FULL
    /// EBP into ESP even here, and a 16-bit stack moves only BP into SP and leaves ESP[31:16]
    /// alone. The popped frame pointer follows the OPERAND size and is two bytes merged into BP.
    ///
    /// A separate variant rather than a field on `Leave` for the reason `Pop16` is separate from
    /// `Pop`: `Leave`'s emitter hard-codes the Dword read, the +4 advance and a full 32-bit
    /// destination write, and `raw_clocks`, `read_segment` and the access-lane membership all key
    /// on the variant.
    Leave16 {
        stack32: bool,
    },
    /// ENTER imm16, 0 (0xC8) at WORD operand size, on either stack width. `alloc` is the frame
    /// size and `stack32` is SS.B, resolved the same way `Leave16`'s is.
    ///
    /// Three effects in order: push the old BP as two bytes, set BP to the stack pointer AFTER
    /// that push, then subtract `alloc` from the pointer. The middle step reads the pointer at
    /// StackAddrSize and writes it at the operand size (386 PRM 17-62), so on a 32-bit stack BP
    /// takes the low half of the full ESP. The allocation is an implicit stack reference with no
    /// memory access, so it follows SS.B rather than the operand size.
    ///
    /// NESTING LEVEL 0 ONLY. A level above zero copies the enclosing display with a loop of reads
    /// and pushes, each with its own fault point and its own partial-commit rewind; `classify`
    /// refuses it and the block stops there. The Dword operand form is refused for the same
    /// reason it has no emitter: no census row asks for it.
    Enter16 {
        alloc: u16,
        stack32: bool,
    },
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
    /// A byte the SLOT computed before the store's address work started, parked in the native
    /// frame at `STACK_PUSH_MEM_VALUE` and read back here. Today's only producer is `SetCcMem`.
    ///
    /// It exists because `emit_store` materialises its source LAST -- inside the page-kind arm,
    /// after the address, the permission check and the write-pointer resolve have consumed every
    /// one of RAX/RCX/RDX/RDI -- and a SETcc byte cannot be produced there: it needs
    /// `emit_load_host_flags`, which works in RAX and moves RSP with a `push`/`popfq` pair.
    ///
    /// The frame slot is the one `emit_push_mem` already uses for the same shape (a value read
    /// before a store whose own path clobbers all four scratch registers), and the aliasing
    /// argument recorded on `STACK_PUSH_MEM_VALUE` carries over unchanged: the write and the read
    /// happen inside a single slot's emission, and `SetCcMem` is not an ALU kind, so it can never
    /// be live at the same time as `STACK_ALU_OLD_RESULT`'s own user.
    ParkedByte,
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
            // `Leave16` joins them: the interpreter's 0xc9 arm returns one `clocks(4)` for both
            // operand sizes and both stack widths.
            Self::Pop { .. } | Self::Pop16 { .. } | Self::Leave | Self::Leave16 { .. } => 4,
            // ENTER's interpreter arm returns `clocks(10)` (execute.rs, 0xc8) against a default
            // of 2, so an omitted arm here under-charges every lowered ENTER by eight raw clocks.
            // Invisible from inside the emitter for the reason the `CallOut` note below gives:
            // `completed_raw` sums this same accessor, so the end-of-emit assertion agrees with
            // itself whatever this returns.
            Self::Enter16 { .. } => 10,
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
            // ... and the SAME arm covers the memory form: `execute_condmove_decoded` returns one
            // `clocks(4)` for the whole `0x0f90..=0x0f9f` range without looking at the operand
            // shape, so the two kinds share this line rather than each guessing.
            Self::SetCc { .. } | Self::SetCcMem { .. } => 4,
            // SAHF's interpreter arm (execute.rs, 0x9e) returns clocks(3) -- one more than the
            // `_ => 2` default, which is exactly the size of gap the campaign has shipped twice
            // and that no emitter assertion can see (`completed_raw` sums this same accessor).
            // LAHF's arm returns clocks(2) and is not lowered at all.
            Self::Sahf => 3,
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
            // `PopSegReal` joins it at the same 7: the interpreter's 0x07 / 0x1f arms return
            // clocks(7) too, and the stack read is charged separately through `word_reads` the
            // way `Pop16`'s is.
            Self::LoadSegReal { .. } | Self::PopSegReal { .. } => 7,
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
                    | Self::PopSegReal { .. }
                    // ONE word read at either stack width: the popped frame pointer follows the
                    // OPERAND size, which is Word for both `Leave16` cells.
                    | Self::Leave16 { .. }
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
                    // The divisor. ONE dword read, the same one `ImulMemAcc` registers: the
                    // dividend is EDX:EAX and never touches memory.
                    | Self::DivMem { .. }
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
                } | Self::SetCcMem { .. }
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
                    // ONE word store at either stack width: the saved frame pointer is pushed at
                    // the OPERAND size. The frame allocation that follows is an implicit stack
                    // reference with no memory access at all.
                    | Self::Enter16 { .. }
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

    /// The operand address and access width of a slot whose emitter reaches the UNRELAXED wide
    /// guard -- `emit_wide_page_guard`, whose alignment half SIDE-EXITS rather than falling into a
    /// split-charge slow path. `None` for every other kind.
    ///
    /// THIS IS AN ENUMERATION OF EMITTER SITES, not of opcodes, and it is written against them one
    /// for one. An earlier version of the certain-exit rule listed `RmwIncDec` alone and justified
    /// the narrowness with "it is the only unrelaxed site this slice can reach"; that was FALSE,
    /// and the review that caught it named the counterexample the tree already asserts 30 lines
    /// from the claim (`the_non_relaxed_sites_still_refuse_a_misaligned_access` uses
    /// `add dword [odd], imm8`, an `AluMemDest`). The list below is the whole of it:
    ///
    /// | kind | site |
    /// |---|---|
    /// | `RmwIncDec` | `emit_rmw_inc_dec` / `_dword` (`emit/mem.rs`) |
    /// | `AluMemDest` with `op != 7` | `emit_alu_mem_dest`'s writing branch |
    /// | `DoubleShiftMem` | `emit_double_shift_mem` |
    /// | `PushMem` | `emit_push_mem`, the SOURCE read |
    /// | `CallMem` | `emit_call_mem`, the SOURCE read |
    /// | `JmpMem` | its emit arm, which calls `emit_ram_read_pointer_inner` directly |
    /// | `DivMem` | `emit_div_mem`, likewise |
    /// | `X87 { addr: Some(..) }` | `emit_x87_memory_pointer`, guard before either fast arm |
    ///
    /// `AluMemDest`'s CMP (`op == 7`) is excluded because that branch alone reads through
    /// `emit_ram_read_pointer`, which dispatches to the relaxed lean site. `PushMem` and `CallMem`
    /// guard their STACK access too, but that address is ESP-relative and so is never decidable by
    /// the caller; only the source address is reported here.
    ///
    /// **Stated at the shipped one-lookup defaults.** `Load`, `LoadExtend`, `AluMemSource`,
    /// `ImulMem`, `ImulMemAcc`, `TestImmMem`, `Store`, `SetCcMem` and CMP's read all dispatch to a
    /// relaxed site through `emit_ram_read_pointer` / `emit_store`, and they do so only while
    /// `one_lookup_load` and `one_lookup_store` are on. With either turned off they become
    /// unrelaxed too and this list under-covers -- which costs a missed refusal, never a
    /// miscompile, on an arm no fixture benches.
    fn unrelaxed_wide_guard_access(self) -> Option<(DirectAddr, MemoryWidth)> {
        match self {
            Self::RmwIncDec { width, addr, .. } => Some((addr, width)),
            Self::AluMemDest {
                op: 0..=6,
                width,
                addr,
                ..
            } => Some((addr, width)),
            Self::DoubleShiftMem { addr, .. }
            | Self::PushMem { addr }
            | Self::CallMem { addr, .. }
            | Self::JmpMem { addr }
            | Self::DivMem { addr, .. } => Some((addr, MemoryWidth::Dword)),
            Self::X87 {
                insn,
                addr: Some(addr),
            } => insn
                .metadata()
                .memory
                .map(|access| (addr, x87_memory_width(access))),
            _ => None,
        }
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
            | Self::DivMem { addr, .. }
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
            // The STACK segment, not the one being written. `PopSegReal`'s destination is reported
            // through `written_segment`, which is a different question and must not be folded in
            // here: doing so would pin the destination in `used` and retire the block every time
            // an unrelated ES reload moved a value the slot never reads.
            | Self::PopSegReal { .. }
            | Self::Leave
            | Self::Leave16 { .. }
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
            Self::LoadSegReal { segment, .. } | Self::PopSegReal { segment } => Some(segment),
            _ => None,
        }
    }

    fn write_segment(self) -> Option<SegmentIndex> {
        match self {
            Self::Store { addr, .. }
            | Self::RmwIncDec { addr, .. }
            | Self::SetCcMem { addr, .. }
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
            | Self::Enter16 { .. }
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
                // `PopSegReal` is here for the same load-bearing reason `PushMem` is, one width
                // over: it exists only in the 16-bit-stack shape, and the stack-width matrix is
                // what refuses it on a 32-bit stack. Leaving it out of this predicate would skip
                // the matrix entirely and emit a 16-bit stack read against a 32-bit ESP.
                | Self::PopSegReal { .. }
                | Self::Leave
                // Both new kinds are here for the load-bearing reason `PushMem` and `PopSegReal`
                // are: the stack-width matrix is only consulted for kinds this predicate accepts,
                // and it is what pairs the emitted pointer arithmetic with SS.B.
                | Self::Leave16 { .. }
                | Self::Enter16 { .. }
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

    /// This slot's contribution to the block's emitted host bytes, for the walk's page budget.
    /// Derivation and the measurement behind the constants: `EMITTED_BLOCK_FIXED_BYTES`.
    ///
    /// Three classes and one adder, mostly read off methods this walk already calls, so a new
    /// `DirectKind` variant is priced the moment it declares its accesses rather than needing a
    /// new arm here. A slot that names no access is register-only; every other non-call-out slot
    /// carries an address computation, a fast-map probe and a watched/device fallback, which is
    /// what the memory rate pays for.
    ///
    /// Two groups need naming because their size is not in their accesses:
    ///
    /// * x87 slots. They declare no GPR access, so the accessor test would price them at the
    ///   register rate, and the constants' doc says they are priced at the MEMORY rate. That is
    ///   deliberately low rather than measured -- `MAX_X87_BLOCK_INSTRUCTIONS` already keeps such
    ///   a block inside its page -- but low is not the same as forty bytes, and the doc has to be
    ///   true of the code.
    /// * the four register-operand kinds that still emit a SIDE EXIT: the two divide guards and
    ///   the two dynamic transfers. A stub plus its shared accounting block is worth far more than
    ///   a register slot, so pricing them as one under-predicts a block full of them.
    ///
    /// Both are COST corrections, not correctness ones: the recovery search is the net, and the
    /// worst an over-price does is end a block one slot early.
    pub(super) fn emitted_bytes_estimate(self) -> u32 {
        let body = if self.is_call_out() {
            EMITTED_CALL_OUT_SLOT_BYTES
        } else if self.is_x87()
            || matches!(
                self,
                Self::DivReg { .. }
                    | Self::MulReg { .. }
                    | Self::JmpReg { .. }
                    | Self::CallReg { .. }
            )
            || self.byte_reads()
                | self.word_reads()
                | self.dword_reads()
                | self.byte_stores()
                | self.word_stores()
                | self.dword_stores()
                != 0
        {
            EMITTED_MEMORY_SLOT_BYTES
        } else {
            EMITTED_REGISTER_SLOT_BYTES
        };
        body + if self.is_terminal() {
            EMITTED_TERMINAL_SLOT_EXTRA_BYTES
        } else {
            0
        }
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
/// ## The CS override (V86 loop-A slice, `IZARRAVM_V86_LOOP_ROWS`, default ON since 2026-08-20)
///
/// CS was refused here explicitly rather than by omission, on two stated grounds, and the tombraid
/// loop-A census answers both. It is admitted only behind the gate, so the off arm is the refusal
/// byte for byte.
///
/// The refusal cost 12,674 doom exits when it was written (on `0xFF /4` `jmp dword [cs:m]`; zero on
/// quake). On tombraid-586 it costs the whole of loop A: the driver keeps its counters in its own
/// code segment, so `0xff /1` `dec word cs:[m]` (95,055,642 interpreted hits), `0x2b /0`
/// `sub ax, cs:[m]` (95,055,326), `0xc7 /0` `mov word cs:[m], imm16` and `0xa3`
/// `mov cs:[m], ax` all stop their walks HERE while `classify` would lower every one of them
/// unchanged. See `v86_loop_rows_enabled` for the disassembly and for why the re-profile's
/// "prefix mask 64" reads as a data segment and is not one.
///
/// The first stated reason was that **a CS-override WRITE is already unreachable**, because
/// `segment_access_supported` refuses `write` to a code segment, so admitting CS would admit reads
/// only. That is true in PROTECTED mode and only there: `segment_access_supported`'s first line
/// returns `true` unconditionally in real mode and V86, where a segment load has no descriptor to
/// have a type. Loop A is V86. So the mechanism this gate admits is exactly "reads wherever CS is
/// readable, writes only where a segment load has no descriptor" -- a split the existing function
/// already expresses, rather than a new gate this one has to add. A block admitted under one of
/// those modes can never later be entered under the other: CR0.PE and EFLAGS.VM are bits 1 and 2 of
/// `jit_mode_key`, which `BlockKey` carries and the entry check compares, the same argument
/// `stack_width_kind` makes for `LoadSegReal`.
///
/// One thing the reversal DOES change for the refused half, worth stating because it moves a census
/// row rather than a behaviour: a protected-mode CS-override WRITE used to stop at this function,
/// i.e. as a `CompileStop::Structural` with an attributed rejected span. With the gate on it gets
/// past here and is refused one step later by `kind_segment_access_supported`, which is a
/// `CompileStop::Retry` and lands in the census as a DORMANT key rather than a rejected span. Same
/// outcome for the guest, different class in the tables, and a retry rather than a memoized
/// decline.
///
/// The second was that **CS is the one segment this backend homes TWICE**, in `SegmentLayout.cs`
/// and at index 1 of `data`, so a CS-override memory kind would be the first thing to depend on
/// the two homes agreeing. They cannot disagree. `Registers::cs()` is literally
/// `self.segment(SegmentIndex::Cs)` (`lib.rs`), and `SegmentLayout::capture` fills `cs` and
/// `data[1]` from that one array element in the same call, so `cs_matches` and the `data[1]` half
/// of `data_matches` are the same predicate -- and `all_data_matches`, which the dispatcher entry
/// check runs, already compares `data[1]` on every block today. What the admission newly does is
/// put the CS bit in `used`, which makes `SegmentLayout::descriptor(Cs)` legal (its `debug_assert`
/// is on `used`) and adds CS to `merge_chain`'s comparison; the latter can refuse no edge that
/// `link_merge`'s own `self.cs != target.cs` test does not refuse first.
/// `SegmentLayout::selector` keeps its CS special case and is still reached only from
/// `MovSegToReg` and `Push { Selector }`, both of which exclude CS by their own guards.
fn prefixes_supported_for(prefixes: Prefixes, operand_size: OperandSize, d: bool) -> bool {
    if prefixes.segment_override == Some(SegmentIndex::Cs) && !v86_loop_rows_enabled() {
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

pub(crate) fn compile(cpu: &mut CpuGsw, entry_lin: u32, d: bool) -> CompileOutcome {
    compile_with_page_len(cpu, entry_lin, d, super::exec_mem::host_page_len())
}

fn compile_with_page_len(
    cpu: &mut CpuGsw,
    entry_lin: u32,
    d: bool,
    page_len: usize,
) -> CompileOutcome {
    let full = match compile_with_budget(cpu, entry_lin, d, MAX_BLOCK_INSTRUCTIONS, page_len) {
        CompileOutcome::Compiled(compilation) => compilation,
        other => return other,
    };
    if full.code.len() <= page_len {
        return CompileOutcome::Compiled(full);
    }
    cpu.jit_direct.note_compile_page_overflow();

    // The size model under-predicted this block. Fall back to the search WITHOUT the budget, so
    // the recovery path keeps exactly the semantics it had before the model existed: shorter
    // candidates use the same fallthrough exit, so emitted size increases with the instruction
    // count, and the longest prefix that fits one arena page is what installs. Two-instruction
    // nonterminal prefixes remain interpreter-only.
    let mut lower = 3usize;
    let mut upper = usize::from(full.span.instructions).saturating_sub(1);
    let mut best = None;
    while lower <= upper {
        let midpoint = lower + (upper - lower) / 2;
        cpu.jit_direct.note_compile_page_search_step();
        let candidate = match compile_with_instruction_limit(cpu, entry_lin, d, midpoint) {
            CompileOutcome::Compiled(compilation) => compilation,
            // The INNER cause propagates. This arm is defence in depth rather than a live
            // path: a search step walks a PREFIX of a block the full walk already compiled, so
            // every per-slot gate it meets has already answered yes, its `stop` can only be the
            // loop-limit `Boundary`, and `midpoint` is never below three. Nothing here can
            // Retry today. It is written to carry the cause anyway so that a future gate which
            // does depend on the block's length reports itself instead of the host page.
            CompileOutcome::Retry(cause) => return CompileOutcome::Retry(cause),
            // A structural barrier the SEARCH found and the full walk did not, which is not a
            // host-page failure however it arrives here. It was reported as `HostPageLen` until
            // review finding F7, which mislabelled the one arm on this path that has nothing to
            // do with the arena page. It is its own cause; whether it is reachable at all is a
            // question the counter now answers instead of the label pre-judging it.
            CompileOutcome::StructuralReject(_) => {
                return CompileOutcome::Retry(RetryCause::SearchStructural);
            }
        };
        if candidate.code.len() <= page_len {
            best = Some(candidate);
            lower = midpoint + 1;
        } else {
            upper = midpoint - 1;
        }
    }
    best.map_or(
        CompileOutcome::Retry(RetryCause::HostPageLen),
        CompileOutcome::Compiled,
    )
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
    Retry(RetryCause),
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
        //
        // `PopSegReal` rides the same refusal, and it must: its emit arm is `LoadSegReal`'s three
        // stores with the selector coming off the stack instead of out of a register, so the
        // descriptor question is identical and the source of the selector changes nothing about
        // it. The mode-key argument above carries across unchanged -- V86 is bit 2 of
        // `jit_mode_key`, so a block admitted here under real mode or V86 can never later be
        // entered under protected mode.
        //
        // The two part company on the ANSWER since the S3 policy widening. `LoadSegReal` is
        // `0x8E`, which is on the `InterpretOne` allowlist, so its protected-mode form takes the
        // call-out instead of the boundary: the helper runs `load_protected_segment` with every
        // check, every fault vector and the accessed-bit write-back, and R2 decides afterwards
        // whether the block may carry on. `PopSegReal` is `0x07`/`0x1F` POP ES/POP DS, which is
        // NOT on that allowlist, so it keeps the refusal. Admitting it would be an unmeasured
        // row, and this file's rule is that an admission needs a census behind it.
        //
        // Deciding it HERE and not in `classify` is the same reason the refusal was here: the
        // mode is CPU state and `classify` has no CPU.
        //
        // The kind that comes out reports NO segment write, where `LoadSegReal` reported one, so
        // this block's `dirty_segments` mask no longer learns that the instruction can move a
        // record. Safe for one reason, stated because it is not local: R2 compares every record
        // the LATER SLOTS bake, so a slot that moved one of those ends the run before any of them
        // addresses through it -- and the block publishes no successors, so there is no chained
        // body to enter against a record it moved either.
        DirectKind::LoadSegReal { .. } if cpu.is_protected_mode() && !cpu.is_v86_mode() => {
            DirectKind::CallOut {
                helper: CallOutHelper::InterpretOne {
                    row: InterpretOneRow::MovSreg,
                },
            }
        }
        DirectKind::PopSegReal { .. } if cpu.is_protected_mode() && !cpu.is_v86_mode() => {
            return None;
        }
        other => other,
    };
    match (kind, cpu.stack_is_32bit(), operand_size) {
        (kind, _, _) if !kind.uses_stack() => Some(kind),
        // `PopSegReal` is matched BEFORE the blanket 32-bit-stack arm below, because it is the one
        // stack kind with no 32-bit shape at all: `classify` refuses it at Dword, so the arm below
        // could never wave it through, but a future edit that admitted the Dword form would
        // otherwise reach an emitter that only knows the 16-bit one. Refusing here says so.
        (DirectKind::PopSegReal { segment }, false, OperandSize::Word) => {
            Some(DirectKind::PopSegReal { segment })
        }
        (DirectKind::PopSegReal { .. }, _, _) => None,
        // ENTER, matched BEFORE the blanket 32-bit-stack arm below for the reason `PopSegReal` is:
        // `classify` refuses the Dword operand form, so nothing could reach that arm today, but a
        // future admission of the Dword form would otherwise be waved through to an emitter that
        // only knows the Word one. Both stack widths are built, and each carries its own pointer
        // arithmetic in the emitter.
        (DirectKind::Enter16 { alloc, .. }, stack32, OperandSize::Word) => {
            Some(DirectKind::Enter16 { alloc, stack32 })
        }
        (DirectKind::Enter16 { .. }, _, _) => None,
        // LEAVE's Word cells, both stack widths. The Dword cell on a 32-bit stack is the blanket
        // arm below.
        (DirectKind::Leave, stack32, OperandSize::Word) => Some(DirectKind::Leave16 { stack32 }),
        // The fourth cell, spelled out rather than left to `_ => None`: a Dword LEAVE on a 16-bit
        // stack would read four bytes and advance four with a 16-bit pointer, and no emitter
        // builds that. A silent fallthrough is the same answer for as long as no arm below
        // happens to match `Leave`, which is a property of the arm order rather than a decision.
        (DirectKind::Leave, false, OperandSize::Dword) => None,
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
        width: IMM_LANE_WIDTH as u8,
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

/// The ONE-BYTE twin of `imm_lane_for`: `0x80 /r`, ALU r/m8 with an imm8, REGISTER destination,
/// no prefixes — the first user of the `IMM8_LANE_WIDTH` class, behind `IZARRAVM_IMM8_LANES`.
///
/// # Why this family and not the other two
///
/// The duke3d-586 SMC shape trace (`.bench/results/duke-smc-trace-20260819/README.md`) measures
/// three `imm_len == 1` patch shapes carrying 79% of all block kills and 62% of newly-hot chunk
/// events: `0x80` at 4.73 M events, `0xC1` at 1.97 M and `0x0FA4` (SHLD) at 0.21 M. Only `0x80`
/// ships here, and the cut is a CORRECTNESS one rather than a ranking one. `emit_rotate_reg` and
/// the SHLD emitter both pick their flag-capture behaviour at EMISSION from the immediate's value
/// (`rotate_rows_enabled`'s "THE DESIGN COST" paragraph states the three-way split: count 0 moves
/// no flag at all, count 1 captures `CF|OF`, 2..31 captures CF alone), so a runtime immediate would
/// have to be paired with a runtime-conditional flag path that does not exist. The byte ALU has no
/// such split: `emit_alu_byte_preloaded` derives every flag from the host operation it just ran,
/// which is why `emit_alu_reg_byte` can already feed it a register operand. Swapping a baked
/// `mov ecx, imm32` for a `movzx ecx, byte [lane]` therefore changes the SOURCE of the operand and
/// nothing else — same operation, same lazy-flag descriptor, same truncation, same write-back
/// suppression on CMP.
///
/// # The admission bars, and each one's job
///
/// - `imm8_lanes_enabled()`: the A/B arm. Default OFF, so the shipped binary is the pre-slice
///   world and the ladder can measure both arms out of one executable.
/// - `DirectKind::AluByteImm { lane: None, .. }`: the only kind whose emitter has a lane arm.
/// - `insn.opcode == 0x80`: `classify` also produces `AluByteImm` for the AL-accumulator short
///   forms (`0x04`/`0x0C`/…/`0x3C`), whose immediate sits at offset ONE, not two. Testing the
///   opcode is what makes `physical + 2` a fact about the encoding rather than an assumption.
///   A `0x82` alias, if the decoder ever produced one, is likewise out.
/// - `insn.prefixes == Prefixes::default()`: no segment override, no address-size override, no
///   operand-size override, no REP and no LOCK. Any prefix byte moves the immediate off offset 2,
///   and a LOCK'd patch is refused rather than argued impossible — the same bar `imm_lane_for`
///   sets.
/// - `insn.disp_len == 0`, `insn.imm_len == 1`, `insn.len == 3`: the decoder's own record of what
///   it consumed. Together they pin the immediate at instruction offset 2 and nowhere else. The
///   register destination is implied by the kind (`classify` produces `AluByteImm` only from
///   `DecodedOperand::Reg`) and re-implied by `len == 3`, which no memory form can reach.
/// - `direct_host_bytes(lane, IMM8_LANE_WIDTH)`: the page-kind guard, verbatim from
///   `imm_lane_for` — only a page the fetch cache hands out may back a lane, so device apertures
///   and unmapped pages never produce one. The instruction is already page-local in physical
///   (`physical_page_local` in the compile loop), so the single byte cannot straddle.
/// - `lanes_used >= MAX_BLOCK_IMM_LANES`: the shared per-block budget. Slots past it keep their
///   baked immediate, which is a missed optimisation and never a correctness question.
///
/// NO HEAT GATE, unlike `disp_lane_for`. The two are not the same trade: a disp lane costs two
/// host instructions on every EXECUTION of a load that may never be patched, which is what made
/// doom's unpatched texture loads regress; this lane replaces a `mov r32, imm32` with a
/// `mov r64, imm64` + `movzx r32, byte [r64]`, on a row duke measures at 4.73 M actual patch
/// events. If the ladder shows the untaxed sites dominating, the disp gate
/// (`has_record_range(lane, IMM8_LANE_WIDTH)`) is the ready-made second arm — but gating on
/// unmeasured suspicion would ship two mechanisms as one A/B.
fn imm8_lane_for(
    cpu: &CpuGsw,
    insn: &DecodedInsn,
    kind: DirectKind,
    physical: u32,
    lanes_used: usize,
) -> Option<(DirectKind, ImmLane)> {
    if lanes_used >= MAX_BLOCK_IMM_LANES || !imm8_lanes_enabled() {
        return None;
    }
    let DirectKind::AluByteImm {
        op,
        dst,
        imm,
        lane: None,
    } = kind
    else {
        return None;
    };
    if insn.opcode != 0x80
        || insn.prefixes != Prefixes::default()
        || insn.disp_len != 0
        || insn.imm_len != 1
        || insn.len != 3
    {
        return None;
    }
    let lane = physical.checked_add(2)?;
    let host = cpu.direct_host_bytes(lane, IMM8_LANE_WIDTH)?;
    let lane = ImmLane {
        physical: lane,
        host,
        width: IMM8_LANE_WIDTH as u8,
    };
    Some((
        DirectKind::AluByteImm {
            op,
            dst,
            imm,
            lane: Some(lane),
        },
        lane,
    ))
}

/// The GROUP-2 COUNT twin of `imm8_lane_for`: the count byte of `0xC1 /0` ROL, `0xC1 /1` ROR,
/// `0xC1 /4..=7` (SHL/SHR/SAL/SAR at Dword) and `0xC0 /4` SHL r8 — register destinations, no
/// prefixes. The second user of the `IMM8_LANE_WIDTH` class, behind `IZARRAVM_COUNT_LANES`.
///
/// # Why this family, and why it comes second
///
/// This is the RE-TEST TRIGGER `rotate_rows_enabled` names. duke3d patches the COUNT BYTE of its
/// group-2 shifts and rotates (the SMC shape table's `0xC1 /0,/4,/5` `imm_len=1` rows, ~1.97 M
/// events on the duke3d-586 long row), and since the 2026-08-19/20 `IZARRAVM_ROTATE_ROWS` default
/// flip those sites are ADMITTED — so each patch now kills a compiled block that may also carry
/// live `0x81` imm lanes and `0x8A` displacement lanes, taking their accepts down with it.
/// `smc_lane_accepts` fell 109.0 M -> 91.8 M (-16%) on that row when the rows were admitted, and
/// this class is what turns those kills back into accepts.
///
/// It comes second because a laned count is not flag-neutral the way a laned `0x80` immediate is:
/// the emitters pick their whole flag shape from the count's VALUE at emission. That is the design
/// cost, it is paid in `emit_rotate_reg_lane` and `emit_shift_lane` as a runtime three-way branch,
/// and it is the reason this is a separate slice with a separate knob rather than a widening of
/// `imm8_lane_for`'s opcode test.
///
/// # The admission bars, and each one's job
///
/// - `count_lanes_enabled()`: the A/B arm. **Default ON since the 2026-08-20 ladder** (-5.73%
///   short, -4.94% long), with `0` / `off` the escape to the pre-slice world; both arms ship in one
///   executable so a later ladder can still measure them. Independent of `IZARRAVM_IMM8_LANES` on
///   purpose — see `count_lanes_enabled`.
/// - `DirectKind::RotateReg { lane: None, .. }` / `DirectKind::Shift { lane: None, width: Byte |
///   Dword, .. }`: the only two kinds whose emitters have a lane arm, at the only two widths whose
///   emitters have one. `ShiftCl` (`0xD3`) is excluded by kind: its count is already runtime data
///   out of guest CL and it has no immediate byte to lane.
///
///   **THE WIDTH HALF OF THAT TEST IS LOAD-BEARING AND WAS ONCE ABSENT.** The first version of
///   this function argued Word away instead of testing it: "a Word `0xC1` needs a `0x66` prefix,
///   so the prefix bar and `len == 3` already refuse it". That is true in a 32-bit code segment
///   and FALSE in a 16-bit one, where the operand size follows CS.D. An unprefixed `c1 e0 03` in a
///   CS.D=0 segment decodes as `shl ax, 3` at `OperandSize::Word` and satisfies every other bar
///   here; `0xC1` is on classify's Word allowlist, so the lane attached and `emit_shift_lane`
///   reached its `unreachable!` and PANICKED THE COMPILER on ordinary DOS code. Barring on the
///   kind's own width is what makes the refusal a fact rather than an inference about encodings.
///   `a_word_group_two_shift_in_a_sixteen_bit_segment_takes_no_count_lane` is the regression
///   fixture; the Word form keeps compiling with a baked count, exactly as before this slice.
///   `RotateReg` needs no width test: classify refuses both rotates at Word outright.
/// - `insn.opcode == 0xC1 || insn.opcode == 0xC0`: `0xD1` produces the SAME two kinds but carries
///   no immediate at all — its count is the literal 1 baked into the opcode — so `physical + 2`
///   would name the next instruction's first byte. **This bar is REDUNDANT today and kept anyway,
///   which is worth stating plainly rather than leaving a reader to discover by mutation:** every
///   opcode that produces `RotateReg` or `Shift` other than these two (`0xD0`..`0xD3`) has
///   `imm_len == 0`, so the length bars below already refuse them, and no fixture in
///   `cpu_jit_count_lane_test` can isolate this test's removal. It stays because it is the bar that
///   makes `physical + 2` a fact about the ENCODING rather than an inference from three other
///   fields, on `imm_lane_for`'s over-determined-checks principle.
/// - `insn.prefixes == Prefixes::default()`: no segment override, no address-size override, no
///   operand-size override, no REP and no LOCK. Any prefix byte moves the immediate off offset 2,
///   and a LOCK'd patch is refused rather than argued impossible — `imm_lane_for`'s bar exactly.
///   This bar does NOT keep Word out — the width test above is what does that, and reading this
///   bar as if it did is the exact mistake that shipped a compiler panic.
/// - `insn.disp_len == 0`, `insn.imm_len == 1`, `insn.len == 3`: the decoder's own record of what
///   it consumed. Together they pin the count byte at instruction offset 2 and nowhere else. The
///   register destination is implied by the kinds (`classify` produces both only from
///   `DecodedOperand::Reg`) and re-implied by `len == 3`, which no memory form can reach.
/// - `direct_host_bytes(lane, IMM8_LANE_WIDTH)`: the page-kind guard, verbatim from
///   `imm_lane_for` — only a page the fetch cache hands out may back a lane, so device apertures
///   and unmapped pages never produce one. The instruction is already page-local in physical
///   (`physical_page_local` in the compile loop), so the single byte cannot straddle.
/// - `lanes_used >= MAX_BLOCK_IMM_LANES`: the shared per-block budget. Slots past it keep their
///   baked count, which is a missed optimisation and never a correctness question.
///
/// NO HEAT GATE, for `imm8_lane_for`'s reason and with one measurement on top of it: the L1
/// `heat_gated` arm gated this very row on heat and LOST (+1.6% wall), because only 11.4% of the
/// ROL row's runtime hits sit on unheated chunks. Gating the LANE by heat would be the same bet
/// from the other side. If the ladder shows the untaxed sites dominating, `has_record_range(lane,
/// IMM8_LANE_WIDTH)` is the ready-made second arm.
///
/// # What does NOT change, stated where a reviewer will look for it
///
/// **Admission is untouched, so `census_native_suffix` owes no new mirror.** This lane attaches to
/// a kind `classify` has already admitted — every bar above narrows which admitted slots take a
/// lane, and none of them can turn a Native classification into a boundary. The L1 heat gate needed
/// a census mirror precisely because it was the one admission rule that was not a `classify`
/// answer; this is not an admission rule at all. The suffix scan therefore stops exactly where the
/// compile walk stops on both arms of this knob.
fn count_lane_for(
    cpu: &CpuGsw,
    insn: &DecodedInsn,
    kind: DirectKind,
    physical: u32,
    lanes_used: usize,
) -> Option<(DirectKind, ImmLane)> {
    if lanes_used >= MAX_BLOCK_IMM_LANES || !count_lanes_enabled() {
        return None;
    }
    if !matches!(
        kind,
        DirectKind::RotateReg { lane: None, .. }
            | DirectKind::Shift {
                lane: None,
                width: MemoryWidth::Byte | MemoryWidth::Dword,
                ..
            }
    ) {
        return None;
    }
    if !matches!(insn.opcode, 0xc0 | 0xc1)
        || insn.prefixes != Prefixes::default()
        || insn.disp_len != 0
        || insn.imm_len != 1
        || insn.len != 3
    {
        return None;
    }
    let lane = physical.checked_add(2)?;
    let host = cpu.direct_host_bytes(lane, IMM8_LANE_WIDTH)?;
    let lane = ImmLane {
        physical: lane,
        host,
        width: IMM8_LANE_WIDTH as u8,
    };
    let kind = match kind {
        DirectKind::RotateReg { op, dst, count, .. } => DirectKind::RotateReg {
            op,
            dst,
            count,
            lane: Some(lane),
        },
        DirectKind::Shift {
            op,
            dst,
            count,
            width,
            ..
        } => DirectKind::Shift {
            op,
            dst,
            count,
            width,
            lane: Some(lane),
        },
        // Unreachable past the kind test above; spelled as a refusal rather than an
        // `unreachable!` so a future widening of that test degrades to "no lane" instead of
        // panicking a compile.
        _ => return None,
    };
    Some((kind, lane))
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
        width: IMM_LANE_WIDTH as u8,
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

/// The physical address of the byte that encodes the COUNT for the two rows `IZARRAVM_ROTATE_ROWS`
/// gates -- `0xC1`/`0xD1` `/0` ROL and `0xC0 /4` SHL r8 -- or `None` for anything else, including
/// the rows that were lowered before the 2026-08-09 slice (`/1` ROR, `/4..=7` at Dword) and which
/// the knob deliberately does not cover.
///
/// `physical` is the instruction's start and the instruction is already known page-local in
/// physical (`physical_page_local` in the compile loop), so start-relative arithmetic over its own
/// bytes is contiguous.
///
/// Two byte positions, because the count lives in two different places:
///
/// * `0xC0`/`0xC1` carry an `imm8` and it is the instruction's LAST byte (`physical + len - 1`).
///   That is the byte duke patches: the SMC shape table's `0xC1 /0,/4,/5` `imm_len=1` rows.
/// * `0xD1` has no immediate at all -- its count is the literal 1 baked into the OPCODE, so the
///   only byte that can change the count is the opcode byte itself. Register form only (classify
///   admits nothing else), so the encoding is prefixes + opcode + modrm and the opcode sits at
///   `physical + len - 2`. Gating a `0xD1` on its opcode byte is the faithful inversion rather
///   than a widening: a `D1 -> C1` patch is exactly the write that would kill a block spanning it.
fn rotate_row_count_byte(insn: &DecodedInsn, physical: u32) -> Option<u32> {
    let reg = insn.modrm?.reg;
    match insn.opcode {
        0xc0 if reg == 4 => {}
        0xc1 if reg == 0 => {}
        0xd1 if reg == 0 => {
            // No immediate and no displacement is what makes `len - 2` the opcode byte. Both hold
            // for every form classify admits; asserting them here rather than assuming them means
            // a future widening of the admitted shape degrades to "no gate site, refuse" instead
            // of probing an unrelated byte.
            if insn.imm_len != 0 || insn.disp_len != 0 {
                return None;
            }
            return physical.checked_add(u32::from(insn.len).checked_sub(2)?);
        }
        _ => return None,
    }
    if insn.imm_len != 1 {
        return None;
    }
    physical.checked_add(u32::from(insn.len).checked_sub(1)?)
}

/// The L1 heat gate: `true` when this instruction is one of the gated group-2 rows AND its count
/// byte carries an SMC heat record, i.e. the byte has measured patch history and a block spanning
/// it would be at risk of the 2026-08-09 kill amplification.
///
/// This is `disp_lane_for`'s `has_record_range` probe INVERTED. There a record ADMITS the lane
/// form (the field is known to be patched, so pay two host instructions per execution to survive
/// the patch); here a record REFUSES the whole row (the count byte is known to be patched, so
/// leave it the hard boundary it has always been and let the block end before it). Sites with no
/// record convert; sites with one keep the shipped refusal exactly.
///
/// **WHAT THAT BOUNDS, AND WHAT IT DOES NOT.** It would be wrong to call the 2026-08-09
/// amplification structurally impossible on this arm, because a heat record is ERASABLE and this
/// gate reads only the record that exists right now:
///
/// * `SmcHeatMap::sync_resets` clears the whole map on a `BlockCache` clear or `reset_storage`, so
///   after every cache reset the arm behaves like `on` at every site until the records are
///   re-earned;
/// * `take_stale_stamp` CONSUMES an aged-out record as part of `lift_cold_smc_dormant` recovery,
///   which is the same erasure at one site.
///
/// What the gate actually gives is a BOUND with self-correction. In a window where the record is
/// missing, a patched site admits and the next patch kills the block exactly as it did on
/// 2026-08-09 -- but that kill is precisely what writes the record back
/// (`note_code_write_inner`), so the site refuses from its next recompile onward and the cost is
/// one kill per site per erasure rather than the steady-state churn the unconditional arm paid.
/// That convergence is the same shape as `disp_lane_for`'s, one kill behind the truth in the other
/// direction, and it is why the ladder's K1 guard (`smc_lane_accepts` must not fall,
/// `smc_heat_demotions` must not rise) is a real kill criterion rather than a formality.
///
/// The probe reads the heat accelerator WITHOUT `sync_smc_heat`, exactly as `disp_lane_for` does
/// and for the same reason (this is a `&CpuGsw` path). A stale read can at worst admit one site
/// that a later kill will refuse on recompile, or refuse one that could have converted --
/// admission tuning, never correctness, because the lowering itself is correct at every count.
///
/// One byte, not `IMM_LANE_WIDTH`: the design phrase is `has_record_range(count_lane, 1)`. The
/// heat map's resolution is a 16-byte chunk, so this is not as narrow as it reads -- it asks "has
/// anything in this instruction's chunk ever taken a heat-charged kill", which is the conservative
/// direction for a refusal gate.
fn rotate_row_count_byte_is_patched(cpu: &CpuGsw, insn: &DecodedInsn, physical: u32) -> bool {
    let Some(count_byte) = rotate_row_count_byte(insn, physical) else {
        return false;
    };
    cpu.jit_direct.smc_heat.has_record_range(count_byte, 1)
}

/// The walk without a page budget: every prefix the instruction limit allows, whatever it emits.
/// `compile_with_page_len`'s recovery search takes this door so its behaviour does not depend on
/// the size model that sent it there.
fn compile_with_instruction_limit(
    cpu: &mut CpuGsw,
    entry_lin: u32,
    d: bool,
    instruction_limit: usize,
) -> CompileOutcome {
    compile_with_budget(cpu, entry_lin, d, instruction_limit, usize::MAX)
}

fn compile_with_budget(
    cpu: &mut CpuGsw,
    entry_lin: u32,
    d: bool,
    instruction_limit: usize,
    byte_budget: usize,
) -> CompileOutcome {
    let Some(key) = key_for(cpu, entry_lin, d) else {
        return CompileOutcome::Retry(RetryCause::NoKey);
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
    let mut callout_interpret_one_slots = 0u8;
    // Built during the walk rather than after it, because each cell carries its slot's GUEST
    // OFFSET and length, which are in hand here and would have to be recovered from `slots` and
    // `span.key.linear` afterwards.
    // Paired with the SLOT INDEX the cell belongs to, not left in walk order, and design review
    // 11.1 B2 is the reason: the suffix mask is filled by index after the walk, and a cell pushed
    // for an instruction that was then abandoned before `slots.push` would silently shift every
    // later cell onto the wrong slot's suffix. Under-pinning a mask is a miscompile that no test
    // asserting the count alone would see.
    let mut interpret_one_cells: Vec<(usize, Arc<InterpretOneCell>)> = Vec::new();
    // How many `InterpretOne` slots hold a row that can overwrite a segment register.
    //
    // Feeds `segment_write_block` and NOTHING ELSE -- in particular not `dirty_segments`, which
    // would end the block at the next slot that uses the segment and take the whole relaxation
    // with it. What makes that safe is R2: a resumed slot has already compared every record any
    // OTHER slot in the block depends on, so nothing in the block ever runs against a record the
    // step moved.
    //
    // Zero when the knob is off, which is the whole of the off arm's successor behaviour: the
    // block publishes its successors exactly as it did before S4f.
    let mut callout_segment_writes = 0usize;
    // Read ONCE per walk and baked into the cells below, never at run time. A block keeps the arm
    // it was compiled under for its whole life, which is what makes an interleaved A/B readable.
    let segment_resume = callout_segment_resume_enabled();
    let x87_entry_top = cpu.fpu.top();
    let mut x87_exit_top = x87_entry_top;
    let mut memory_alu_slots = 0u8;
    let mut imm_lanes = [NO_IMM_LANE; MAX_BLOCK_IMM_LANES];
    let mut imm_lane_widths = [0u8; MAX_BLOCK_IMM_LANES];
    let mut imm_lane_count = 0usize;
    let mut disp_lane_count = 0u8;
    let mut imm8_lane_count = 0u8;
    let mut count_lane_count = 0u8;
    let mut stop = CompileStop::Boundary;
    // Running estimate of what this block will emit, against `byte_budget` (the arena page).
    // See `EMITTED_BLOCK_FIXED_BYTES` for why the walk carries this at all.
    let mut estimated_bytes = EMITTED_BLOCK_FIXED_BYTES;

    while slots.len() < instruction_limit.min(MAX_BLOCK_INSTRUCTIONS) {
        if x87_slots != 0 && slots.len() == MAX_X87_BLOCK_INSTRUCTIONS {
            break;
        }
        if memory_alu_slots != 0 && slots.len() == MAX_MEMORY_ALU_BLOCK_INSTRUCTIONS {
            break;
        }
        let Some(insn) = cpu.decode_cache.get(lin, d) else {
            stop = CompileStop::Retry(RetryCause::DecodeMiss);
            break;
        };
        let insn_len = u32::from(insn.len);
        if insn_len == 0 {
            stop = CompileStop::Retry(RetryCause::DecodeMiss);
            break;
        }
        let Some(next) = lin.checked_add(insn_len) else {
            stop = CompileStop::Retry(RetryCause::PageCross);
            break;
        };
        let slot_eip = lin.wrapping_sub(cs.base);
        if slot_eip
            .checked_add(insn_len - 1)
            .is_none_or(|last| last > cs.limit)
        {
            stop = CompileStop::Retry(RetryCause::SegmentLimit);
            break;
        }
        if entry_lin >> BLOCK_PAGE_SHIFT != next.wrapping_sub(1) >> BLOCK_PAGE_SHIFT {
            stop = CompileStop::Retry(RetryCause::PageCross);
            break;
        }
        let Some(expected_phys) = key.physical.checked_add(lin.wrapping_sub(entry_lin)) else {
            stop = CompileStop::Retry(RetryCause::PageCross);
            break;
        };
        let physical_page_local = expected_phys
            .checked_add(insn_len - 1)
            .is_some_and(|last| key.physical >> BLOCK_PAGE_SHIFT == last >> BLOCK_PAGE_SHIFT);
        // Two different failures, and the retry instrument splits them because a retry policy
        // would treat them differently: leaving the physical page is decided by the key and the
        // instruction lengths and will read the same on every future walk, while a decode line
        // whose physical start disagrees with the key's projection is the TRANSLATION having
        // moved, which a later walk can find repaired.
        if !physical_page_local {
            stop = CompileStop::Retry(RetryCause::PageCross);
            break;
        }
        if cpu.decode_cache.line_phys_start(lin, d) != Some(expected_phys) {
            stop = CompileStop::Retry(RetryCause::TranslationMismatch);
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
                        estimated_bytes,
                    },
                );
            }
            stop = structural_span.map_or(
                CompileStop::Retry(RetryCause::SpanUnformable),
                CompileStop::Structural,
            );
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
                        estimated_bytes,
                    },
                );
            }
            stop = structural_span.map_or(
                CompileStop::Retry(RetryCause::SpanUnformable),
                CompileStop::Structural,
            );
            break;
        }
        let planned = DirectUnitPlanner::classify(&insn, lin, entry_lin);
        // L1, the third `IZARRAVM_ROTATE_ROWS` arm (dev_docs/duke-reprofile-2026-08-19.md §6.1).
        //
        // WHY HERE AND NOT IN `classify`. `classify` takes a `&DecodedInsn` and two LINEAR
        // addresses; the heat map is keyed by PHYSICAL and lives behind `cpu`, and neither is in
        // scope there. `disp_lane_for` -- the gate this one inverts -- is called from this same
        // loop for exactly that reason, and mirroring its access path is what keeps the two probes
        // reading the one structure through the one accessor.
        //
        // WHY IT DOWNGRADES TO `HardBoundary` RATHER THAN INVENTING A REFUSAL KIND. The off arm's
        // refusal is a `classify` None, which lands in the census as an ordinary `hard_boundary`
        // unbound exit. A heat-gated refusal has to be the SAME row or the arms are not comparable
        // against the census this slice was ranked on. Falling into the existing
        // `PlannedInsn::HardBoundary` arm below gets that for free, barrier-census record and all.
        //
        // COST ON THE OTHER TWO ARMS. The arm compare is a `OnceLock` read against a cached enum
        // and short-circuits before any instruction inspection, so `Off` and `On` pay one
        // perfectly-predicted branch per NATIVE slot on the COMPILE walk -- never on the execution
        // path, which this function does not sit on
        // ([[default-off-instruments-tax-hot-path]]).
        let planned = match planned {
            PlannedInsn::Native(_)
                if rotate_rows_arm() == RotateRowsArm::HeatGated
                    && rotate_row_count_byte_is_patched(cpu, &insn, expected_phys) =>
            {
                PlannedInsn::HardBoundary
            }
            // The CERTAIN-EXIT rule (V86 loop-A slice), the same downgrade for a different reason.
            //
            // `emit_rmw_inc_dec` is one of the memory sites the one-lookup relaxation never
            // reached: it guards with `emit_wide_page_guard`, which ends in an alignment test that
            // side-exits rather than falling into a split-charge slow path. `classify` has said so
            // in prose for a long time, on the form-1 memory shape it refuses -- "admitted today,
            // an odd operand would sit INSIDE the block and side-exit at that slot on every
            // execution, so nothing after it retires natively".
            //
            // That prose was a reason to refuse a whole FORM. It is the wrong currency for an
            // operand whose address is a compile-time CONSTANT, which is what a `disp`-only
            // addressing mode is: there the alignment is decidable at compile time, so the slot is
            // either always fine or always an exit, and admitting the second is strictly worse than
            // the barrier it replaces. A rejected span short-circuits to the interpreter through
            // the decline memo; a certain-exit slot pays a dispatcher lookup, a segment check, a
            // native entry, the address, the page-cross bound, the alignment test and an exit stub,
            // and THEN the interpreter runs the instruction anyway.
            //
            // The tombraid loop is where it bites: `dec word cs:[0xf3]` at `0xc901c` resolves to
            // linear `0xc8113`, and a real-mode segment base is a multiple of 16, so the operand's
            // parity is the displacement's. Its block is `RmwIncDec` plus a terminal `Jcc`, which
            // the two-slot rule admits, and it is the target of 95,020,029 unbound exits per census
            // prefix. Without this rule the slice would compile that block and exit from slot ZERO
            // on all of them.
            //
            // SCOPE, and the first version of this comment got it wrong in a way worth recording.
            // It said "only `RmwIncDec`, because it is the only unrelaxed site this slice can
            // reach", and that was false: `emit_alu_mem_dest` takes the same unrelaxed guard for
            // every op but CMP, `classify` lowers `0x81`/`0x83` memory forms at `operand_width`,
            // and the CS clause of `prefixes_supported_for` is a PREFIX gate -- it unlocks every
            // memory kind at once, not the two the tombraid loop happens to use. The census says
            // so on its face: `0x83 /5 cs:` leaves the rejected table on the ON arm carrying
            // 9,464,397 unbound exits. Tombraid's own instance is aligned, so nothing was measured
            // wrong, but the argument was not sound and the next fixture would have found the hole.
            //
            // `DirectKind::unrelaxed_wide_guard_access` is the enumeration now, written against the
            // emitter sites one for one.
            //
            // COMPLETENESS, argued rather than asserted this time. A certain-exit slot needs an
            // unrelaxed site AND a statically decidable misaligned address, so the question is
            // which of this slice's admissions can produce one. Taken one at a time:
            //
            //  * `PopSegReal` reads the stack, whose address is ESP-relative and therefore never
            //    decidable here;
            //  * `CarryFlag` and the ALU form-5 arm touch no memory at all;
            //  * `0xa1` / `0xa3` produce `Load` and `Store`, which dispatch to the RELAXED lean
            //    sites and are SERVED misaligned rather than exiting;
            //  * the CS clause of `prefixes_supported_for` unlocks every memory kind at once,
            //    including all eight unrelaxed ones.
            //
            // So the CS clause is the ONLY admission here that can create a certain-exit slot, and
            // the rule is scoped to it. That is why the predicate tests `addr.segment == Cs`.
            //
            // Not scoping it that way was tried and is worse. An unconditional version refuses
            // seven pre-existing fixtures that deliberately build a statically misaligned disp-only
            // operand to exercise the RUNTIME guard (`the_non_relaxed_sites_still_refuse_a_
            // misaligned_access`, `a_misaligned_x87_access_still_exits`, the double-shift and
            // memory-ALU transactional rows, and two x87 alignment rows) -- and it would be
            // changing a hazard that PRE-DATES this slice, on a knob whose A/B cannot price it.
            // A statically misaligned `add dword [odd], imm8` through DS has certain-exited since
            // long before this branch; turning that into a barrier is a real and separate slice,
            // with its own measurement, and it is NOT this one.
            //
            // Still narrow one more way: only a base-less, index-less, lane-less address, because
            // anything else is not decidable here. A register-relative CS operand keeps its runtime
            // guard and is served or refused at run time exactly as before.
            PlannedInsn::Native(kind)
                if v86_loop_rows_enabled() && certainly_exits_on_alignment(cpu, kind, d) =>
            {
                PlannedInsn::HardBoundary
            }
            planned => planned,
        };
        let kind = match planned {
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
                            estimated_bytes,
                        },
                    );
                }
                stop = structural_span.map_or(
                    CompileStop::Retry(RetryCause::SpanUnformable),
                    CompileStop::Structural,
                );
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
            stop = CompileStop::Retry(RetryCause::AdmissionMatrix);
            break;
        };
        // The governor's answer, applied at compile time instead of at every execution. A slot it
        // demoted has exactly one behaviour left -- test the byte, take the abnormal exit, hand
        // the instruction to the interpreter -- which is the boundary the row had before it was
        // admitted, reached through a prologue and a side exit that the boundary does not pay.
        // Ending the block here IS that boundary, and it also drops the slots after it, which a
        // demoted block carries and can never reach.
        //
        // BELOW `stack_width_kind` and not beside the `PlannedInsn::HardBoundary` arm above, which
        // is where it was written first and where it was worth exactly nothing. `classify` does
        // not produce every call-out: the protected-mode `MOV Sreg,r/m` arrives as `LoadSegReal`
        // and BECOMES a `CallOut` inside `stack_width_kind`, because the mode is CPU state.
        // `0x8e_mov_sreg` is also the only row the tombraid loader demotes, so a check above that
        // conversion saw none of them -- 402,264 demotions with the mechanism "on", identical to
        // the run without it, which is how the placement was caught.
        //
        // Read only for a slot that actually interprets one instruction: the port and stack-frame
        // helpers have no cell and no governor, so no site of theirs can be in the set.
        if kind
            .call_out_helper()
            .is_some_and(CallOutHelper::interprets_one)
            && cpu
                .jit_direct
                .callout_site_demoted(expected_phys, key.mode_key)
        {
            if instruction_limit >= MAX_BLOCK_INSTRUCTIONS
                && cpu.jit_direct.barrier_census_enabled()
            {
                record_structural_barrier(
                    cpu,
                    &insn,
                    BarrierStop::CallOutDemoted,
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
                        estimated_bytes,
                    },
                );
            }
            stop = structural_span.map_or(
                CompileStop::Retry(RetryCause::SpanUnformable),
                CompileStop::Structural,
            );
            break;
        }
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
            stop = CompileStop::Retry(RetryCause::AdmissionMatrix);
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
                        estimated_bytes,
                    },
                );
            }
            stop = CompileStop::Boundary;
            break;
        }
        if kind.is_x87()
            && (x87_slots == MAX_X87_SLOTS || slots.len() >= MAX_X87_BLOCK_INSTRUCTIONS)
        {
            stop = CompileStop::Retry(RetryCause::X87Cap);
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
            stop = CompileStop::Retry(RetryCause::X87Cap);
            break;
        }
        if kind.is_call_out() && callout_slots == MAX_BLOCK_CALLOUT_SLOTS {
            // The evidence for or against raising the cap, which S5 prices. Counted at the point
            // the cap actually stops a walk, so it is a count of BLOCKS SHORTENED and not of
            // blocks that happen to hold four slots.
            cpu.jit_direct.note_callout_slot_cap_hit();
            stop = CompileStop::Retry(RetryCause::CalloutCap);
            break;
        }
        if kind.is_memory_alu()
            && (memory_alu_slots == MAX_MEMORY_ALU_SLOTS
                || slots.len() >= MAX_MEMORY_ALU_BLOCK_INSTRUCTIONS)
        {
            stop = CompileStop::Retry(RetryCause::MemoryAluCap);
            break;
        }
        if kind.uses_stack() && stack_accesses == MAX_BLOCK_STACK_ACCESSES {
            stop = CompileStop::Retry(RetryCause::StackAccessCap);
            break;
        }
        // The page budget. A BOUNDARY, not a Retry: the block simply ends here and its successor
        // starts at this instruction, which is the same shape the walk takes at a terminal slot.
        //
        // Held above the three-instruction floor `compile_with_instruction_limit`'s tail applies,
        // so the budget can never be the reason a block is too short to install. The MOST
        // EXPENSIVE three slots the rates in `EMITTED_BLOCK_FIXED_BYTES` can price are three
        // call-outs, and 576 + 3 * 592 = 2,352 bytes against a 4,096-byte page: even that shape
        // cannot trip the budget before the floor is cleared.
        //
        // The instruction that ENDS the block here has already been counted by the mechanism
        // counters above (`jit_direct_word_address_slots` and the two Word-control ones). That is
        // deliberate and matches every other cap in this walk -- the stack-access, memory-ALU,
        // call-out-slot and x87 caps all sit below those counters too, and all of them refuse an
        // instruction the counters have already seen. Those columns count instructions the walk
        // CONSIDERED, which is what makes "zero on a corpus with no 16-bit code" the inertness
        // claim they exist for; moving one cap above them would make that population mean
        // something different for one cap than for the other four.
        let next_estimated_bytes = estimated_bytes.saturating_add(kind.emitted_bytes_estimate());
        if slots.len() >= 3
            && usize::try_from(next_estimated_bytes).unwrap_or(usize::MAX) > byte_budget
        {
            stop = CompileStop::Boundary;
            break;
        }
        let slot_weighted_fp_clocks = kind.weighted_fp_clocks(cpu.persona());
        let Some(next_raw_clocks) = raw_clocks.checked_add(kind.raw_clocks()) else {
            stop = CompileStop::Retry(RetryCause::AccumulatorOverflow);
            break;
        };
        let Some(next_weighted_fp_clocks) = weighted_fp_clocks.checked_add(slot_weighted_fp_clocks)
        else {
            stop = CompileStop::Retry(RetryCause::AccumulatorOverflow);
            break;
        };
        let Some(next_byte_reads) = byte_reads.checked_add(kind.byte_reads()) else {
            stop = CompileStop::Retry(RetryCause::AccumulatorOverflow);
            break;
        };
        let Some(next_word_reads) = word_reads.checked_add(kind.word_reads()) else {
            stop = CompileStop::Retry(RetryCause::AccumulatorOverflow);
            break;
        };
        let Some(next_dword_reads) = dword_reads.checked_add(kind.dword_reads()) else {
            stop = CompileStop::Retry(RetryCause::AccumulatorOverflow);
            break;
        };
        let Some(next_byte_stores) = byte_stores.checked_add(kind.byte_stores()) else {
            stop = CompileStop::Retry(RetryCause::AccumulatorOverflow);
            break;
        };
        let Some(next_word_stores) = word_stores.checked_add(kind.word_stores()) else {
            stop = CompileStop::Retry(RetryCause::AccumulatorOverflow);
            break;
        };
        let Some(next_dword_stores) = dword_stores.checked_add(kind.dword_stores()) else {
            stop = CompileStop::Retry(RetryCause::AccumulatorOverflow);
            break;
        };
        stack_accesses += u8::from(kind.uses_stack());
        x87_slots += u8::from(kind.is_x87());
        callout_slots += u8::from(kind.is_call_out());
        if let Some(helper) = kind.call_out_helper() {
            callout_port_slots += u8::from(helper.probes_io_permission());
            callout_memory_slots += u8::from(helper.moves_a_stack_frame());
            callout_interpret_one_slots += u8::from(helper.interprets_one());
            if helper.interprets_one() {
                // The slot's offset from the block's ENTRY linear, which is what the helper adds
                // to the live `cpu.eip` to reach the instruction. `lin` is this instruction's
                // linear address and `entry_lin` the block's; both are page-local by
                // `BlockSpan::new`, so the difference fits a `u16` with three bits to spare.
                let slot_delta = u16::try_from(lin.wrapping_sub(entry_lin))
                    .expect("a page-local block's slot offset fits a u16");
                let row = helper
                    .interpret_one_row()
                    .expect("an InterpretOne helper names its census row");
                callout_segment_writes += usize::from(segment_resume && row.may_write_segment());
                // `slots.len()` IS the index this instruction is about to take: the push happens
                // further down the same iteration and nothing between here and it can `break`.
                // The assertion after the walk is what keeps that true.
                interpret_one_cells.push((
                    slots.len(),
                    Arc::new(InterpretOneCell::new(key, slot_delta, insn.len, row)),
                ));
            }
        }
        if let DirectKind::X87 { insn, .. } = kind {
            x87_exit_top = insn.advance_top(x87_exit_top);
        }
        memory_alu_slots += u8::from(kind.is_memory_alu());
        estimated_bytes = next_estimated_bytes;
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
        // Design review 11.1 M4: the suffix mask is only sound if a slot that ADDRESSES memory
        // pins the segment it addresses through, because the mask is built out of exactly that
        // answer. A kind that touched guest memory while pinning nothing would let a resumed
        // segment write leave a stale base under it and R2 would compare nothing about it.
        debug_assert!(
            kind.pinned_segments() != 0
                || (kind.byte_reads() == 0
                    && kind.word_reads() == 0
                    && kind.dword_reads() == 0
                    && kind.byte_stores() == 0
                    && kind.word_stores() == 0
                    && kind.dword_stores() == 0),
            "a kind with a data access must pin the segment it reaches it through"
        );
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
        // The three lane matchers are mutually exclusive by kind (`AluImm` vs `AluByteImm` vs
        // `Load`), so at most one fires per slot and all three draw on the one
        // `MAX_BLOCK_IMM_LANES` budget. Each records its lane's WIDTH CLASS beside the address;
        // the write choke tests a patch against that per-lane width, so a one-byte lane and a
        // dword lane in the same block cannot absorb each other's stores.
        #[cfg(feature = "barrier-census-closure")]
        let mut lane_probe_bits = 0u8;
        let kind = match imm_lane_for(cpu, &insn, kind, expected_phys, imm_lane_count) {
            Some((kind, lane)) => {
                imm_lanes[imm_lane_count] = lane.physical;
                imm_lane_widths[imm_lane_count] = lane.width;
                imm_lane_count += 1;
                #[cfg(feature = "barrier-census-closure")]
                {
                    lane_probe_bits |= census::lane_probe::IMM;
                }
                kind
            }
            None => match imm8_lane_for(cpu, &insn, kind, expected_phys, imm_lane_count) {
                Some((kind, lane)) => {
                    imm_lanes[imm_lane_count] = lane.physical;
                    imm_lane_widths[imm_lane_count] = lane.width;
                    imm_lane_count += 1;
                    imm8_lane_count += 1;
                    // The IMM bit, deliberately shared with the `0x81` family rather than given a
                    // third: B.3's export answers "did a mutable-IMMEDIATE lane attach on a walk
                    // from this entry", and both classes are that. The census split that says WHICH
                    // class did the work is `imm8_lane_registrations`, which is a counter rather
                    // than a per-site bit.
                    #[cfg(feature = "barrier-census-closure")]
                    {
                        lane_probe_bits |= census::lane_probe::IMM;
                    }
                    kind
                }
                // The group-2 COUNT lane (L2 arm 2). Placed after the two immediate matchers and
                // before the displacement one purely to keep the reading order "widest immediate
                // first"; the four are mutually exclusive by KIND (`AluImm` vs `AluByteImm` vs
                // `RotateReg`/`Shift` vs `Load`), so at most one can fire per slot whatever the
                // order, and all four draw on the one `MAX_BLOCK_IMM_LANES` budget.
                None => match count_lane_for(cpu, &insn, kind, expected_phys, imm_lane_count) {
                    Some((kind, lane)) => {
                        imm_lanes[imm_lane_count] = lane.physical;
                        imm_lane_widths[imm_lane_count] = lane.width;
                        imm_lane_count += 1;
                        count_lane_count += 1;
                        // The IMM bit, shared with both immediate classes for `imm8_lane_for`'s
                        // reason: B.3's export answers "did a mutable-IMMEDIATE lane attach on a
                        // walk from this entry", and a count byte is an immediate. The census
                        // split that says WHICH class did the work is `count_lane_registrations`.
                        #[cfg(feature = "barrier-census-closure")]
                        {
                            lane_probe_bits |= census::lane_probe::IMM;
                        }
                        kind
                    }
                    None => match disp_lane_for(cpu, &insn, kind, expected_phys, imm_lane_count) {
                        Some((kind, lane)) => {
                            imm_lanes[imm_lane_count] = lane.physical;
                            imm_lane_widths[imm_lane_count] = lane.width;
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
                },
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
            // The inner cause wins whenever the walk had already given up: the min-length rule
            // did not decide anything there, it would only re-report the same failure.
            CompileStop::Retry(cause) => CompileOutcome::Retry(cause),
            // `Boundary` with NO slots at all is not the min-length rule. The walk ended cleanly
            // before it could form a single slot -- the block-page budget or the instruction
            // limit refusing the first instruction -- and calling that `TooShort` files a budget
            // refusal under a rule that never looked at it. Review finding F7; a cause of its own
            // so the two can be told apart in a census that is used to price a retry policy.
            CompileStop::Boundary if slots.is_empty() => {
                CompileOutcome::Retry(RetryCause::BudgetFirstSlot)
            }
            // The case the min-length rule OWNS: the walk ended cleanly on a terminal slot, the
            // page budget or the dirty-segment rule, and the block is short only because the rule
            // says three.
            CompileStop::Boundary => CompileOutcome::Retry(RetryCause::TooShort),
        };
    }
    // THE SEGMENT MASKS (design section 11). Two passes, after the walk, because the answer for a
    // slot is a fact about the rest of the block and the walk does not know that while it is still
    // growing.
    //
    // The union is `pinned_segments`, which is read union write union selector, and it is that
    // definition rather than "the segments another slot reads": `MovSegToReg` bakes a SELECTOR as
    // a compile-time constant and reports through neither read nor write, so a mask built from the
    // access accessors alone would let a resumed `mov ds, ax` leave a stale selector baked into a
    // `mov ax, ds` beside it.
    //
    // EVERY OTHER SLOT, before as well as after. The suffix half is the obvious one -- a later
    // slot would run against a record the step moved. The PREFIX half looks redundant, since those
    // slots have already run, and the first loader gate is what showed it is not: `used` is the
    // block-wide pinned set and `data_matches` compares all of it at every entry, so a block that
    // resumes having moved a record its own prefix bakes fails its own entry check next visit and
    // recompiles every time (`reject_data_segment` 307,714 -> 514,327).
    //
    // STRICTLY other: a slot's own pinned set is in neither of its masks. It has already run by
    // the time R2 is asked, and its own operand was resolved against the records as they stood
    // before the step. In practice the distinction is inert -- a `CallOut` kind pins nothing at
    // all -- and it is written this way because the rule is about the OTHER slots, not because the
    // arithmetic needs it.
    //
    // Skipped entirely when the knob is off: the cells keep their `u8::MAX` defaults, R2 compares
    // all six records, and the block published its successors above.
    if segment_resume {
        debug_assert_eq!(
            interpret_one_cells.len(),
            slots
                .iter()
                .filter(|slot| slot
                    .kind
                    .call_out_helper()
                    .is_some_and(|helper| helper.interprets_one()))
                .count(),
            "an InterpretOne cell was allocated for an instruction that did not become a slot"
        );
        // TWO passes and two masks. The forward one is the block's PREFIX at each slot, which is
        // what stops a resumed segment write from moving a record the block itself bakes and so
        // failing its own entry check on the next visit. The backward one is the SUFFIX, which is
        // kept on its own so the resync path can price what the prefix half costs.
        //
        // Index-driven rather than iterator-driven because the cells carry their slot index and
        // are pushed in ascending order: `next` and `back` walk that order once each, and the two
        // assertions afterwards are what say the cells really did line up with the slots.
        let mut prefix_masks = vec![0u8; interpret_one_cells.len()];
        let mut suffix_masks = vec![0u8; interpret_one_cells.len()];
        let mut prefix = 0u8;
        let mut next = 0usize;
        for (index, slot) in slots.iter().enumerate() {
            while interpret_one_cells
                .get(next)
                .is_some_and(|(at, _)| *at == index)
            {
                prefix_masks[next] = prefix;
                next += 1;
            }
            prefix |= slot.kind.pinned_segments();
        }
        debug_assert_eq!(
            next,
            interpret_one_cells.len(),
            "a cell named a slot index off the end"
        );
        let mut suffix = 0u8;
        let mut back = interpret_one_cells.len();
        for index in (0..slots.len()).rev() {
            while back > 0 && interpret_one_cells[back - 1].0 == index {
                back -= 1;
                suffix_masks[back] = suffix;
            }
            suffix |= slots[index].kind.pinned_segments();
        }
        debug_assert_eq!(back, 0, "a cell named a slot index below the block");
        for (position, (_, cell)) in interpret_one_cells.iter_mut().enumerate() {
            let cell = Arc::get_mut(cell)
                .expect("the compile walk owns its cells until install clones them");
            cell.set_segment_masks(
                prefix_masks[position] | suffix_masks[position],
                suffix_masks[position],
            );
        }
        // `used` IS `pinned_segments` (SegmentLayout::capture), and the mask is a union over a
        // SUBSET of the slots that produced it, so this is a subset by construction. Asserted
        // because the entry check leans on it: `data_matches` compares the block-wide `used`, and
        // a mask reaching outside it would be comparing a record the block never pinned.
        debug_assert_eq!(
            interpret_one_cells
                .iter()
                .fold(0u8, |mask, (_, cell)| mask | cell.used_by_others())
                & !pinned_segments,
            0,
            "a segment mask named a segment the block does not pin"
        );
    }
    let interpret_one_cells: Vec<Arc<InterpretOneCell>> = interpret_one_cells
        .into_iter()
        .map(|(_, cell)| cell)
        .collect();
    let Some(last) = slots.last() else {
        return CompileOutcome::Retry(RetryCause::PostWalk);
    };
    let guest_len = last
        .lin
        .wrapping_add(u32::from(last.len))
        .wrapping_sub(entry_lin) as usize;
    let Some(span) = BlockSpan::new(key, guest_len, slots.len()) else {
        return CompileOutcome::Retry(RetryCause::PostWalk);
    };
    let Some(segment_layout) =
        SegmentLayout::capture(cpu, read_segments, write_segments, pinned_segments)
    else {
        return CompileOutcome::Retry(RetryCause::PostWalk);
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
        return CompileOutcome::Retry(RetryCause::PostWalk);
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
            return CompileOutcome::Retry(RetryCause::PostWalk);
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
        return CompileOutcome::Retry(RetryCause::PostWalk);
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
        return CompileOutcome::Retry(RetryCause::PostWalk);
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
    // with a different segment than it entered and `SegmentLayout::link_compatible` demanded equal
    // snapshots. (That method no longer exists: the chain-used mask replaced it with `link_merge`
    // in 2026-08-18. The refutation below did not depend on it and stands unchanged.)
    // That compares the wrong two things: the predicate compares the two blocks'
    // COMPILE-TIME ENTRY snapshots, and on the pass that compiles them the write is very often a
    // no-op -- `mov ds, ax` where AX already holds DS is the ordinary "reload DS with what it
    // has" case. The edge links, and then a LINKED successor runs no segment check at all: a
    // chained transfer jumps into the successor's body without returning to `run_direct_block`,
    // so its `data_matches` never executes. A later entry with a different AX writes DS and jumps
    // straight into a body baked against the old base.
    //
    // Barring both edges makes the property true by construction, and it is what keeps INBOUND
    // links safe as well -- not any argument about snapshots. A block that publishes no
    // successors is where the chain ENDS: its segment write is the last thing that happens before
    // control returns to `run_direct_block`, so there is no downstream body to enter against the
    // base the write just invalidated. That argument survives the chain-used mask
    // (dev_docs/plans/2026-08-18-chain-used-link-mask.md), which is about frozen compile-time
    // capture and says nothing about a value the block is about to overwrite at run time.
    // TWO producers now, not one. `segment_writes` counts the `LoadSegReal`/`PopSegReal`
    // lowerings; `callout_segment_writes` counts the `InterpretOne` slots whose row can call
    // `load_segment_checked`. The bar is the same and so is the argument for it: a chained
    // transfer jumps into a successor's body without returning to `run_direct_block`, so its
    // `data_matches` never runs, and a block that can overwrite a segment register must therefore
    // be where the chain ENDS.
    //
    // The call-out producer is what pays for the suffix-used relaxation of R2. That relaxation
    // lets a slot resume having moved a record no LATER SLOT IN THIS BLOCK uses; a linked
    // successor is not a later slot in this block, and nothing in the mask says anything about
    // what it bakes.
    let segment_write_block = segment_writes != 0 || callout_segment_writes != 0;
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
    let interpret_one_cell_ptrs: Vec<usize> = interpret_one_cells
        .iter()
        .map(|cell| cell.address())
        .collect();
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
        interpret_one_cells: &interpret_one_cell_ptrs,
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
        callout_interpret_one_slots,
        x87_entry_top,
        x87_exit_top,
        dynamic_successor,
        successors,
        #[cfg(feature = "direct-link-refusal-census")]
        emitted_static_targets,
        link_cells,
        interpret_one_cells,
        body_offset: emitted.body_offset,
        imm_lanes,
        imm_lane_widths,
        disp_lanes: disp_lane_count,
        imm8_lanes: imm8_lane_count,
        count_lanes: count_lane_count,
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
        CompileOutcome::StructuralReject(_) | CompileOutcome::Retry(_) => None,
    }
}

/// The same seam, keeping the whole outcome. The one caller is the fixture for the walk ending
/// with NO slots, which is a `Retry` and so invisible through the `Option` above.
#[cfg(test)]
pub(crate) fn compile_outcome_with_instruction_limit_for_test(
    cpu: &mut CpuGsw,
    entry_lin: u32,
    d: bool,
    instruction_limit: usize,
) -> CompileOutcome {
    compile_with_instruction_limit(cpu, entry_lin, d, instruction_limit)
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

/// Whether this slot's operand address is decidable at compile time and FAILS the alignment guard
/// its emitter will produce, i.e. a slot that would side-exit on every single execution. See the
/// call site in the compile walk for why that is worse than the barrier it would replace.
///
/// `base`, `index` and `disp_lane` must all be absent: with a register in the address the operand
/// moves at run time, and with a lane the displacement itself is patchable, so neither is decidable
/// here. What is left is the `disp`-only mode (`mod = 00, rm = 110` at Word address size, `rm = 101`
/// at Dword), which is exactly the shape the tombraid driver uses.
///
/// The offset is masked the way `emit_effective_address` masks it -- to sixteen bits in a 16-bit
/// code segment -- BEFORE the segment base is added, because that is the order the emitted address
/// is computed in and a mask applied afterwards would name an address the guest never forms.
/// `emit_alignment_test` runs on RAX, i.e. on the LINEAR address after the base add, so the base
/// belongs in this arithmetic. It is inert in real mode and V86, where a base is a multiple of 16
/// and cannot move bit 0 or bit 1, and load-bearing in protected mode, where a data segment's base
/// is arbitrary; `a_protected_mode_segment_base_decides_the_alignment` is the row that makes it so.
fn certainly_exits_on_alignment(cpu: &CpuGsw, kind: DirectKind, d: bool) -> bool {
    let Some((addr, width)) = kind.unrelaxed_wide_guard_access() else {
        return false;
    };
    // A CS-OVERRIDE OPERAND, which is exactly and only what this gate newly admits. `decode` folds
    // an explicit override into `AddrMode.segment` and no addressing mode DEFAULTS to CS (the
    // defaults are DS, and SS for the BP forms), so `segment == Cs` is equivalent to "the
    // instruction carried a `2E` prefix" and needs no separate flag to test.
    //
    // See the call site for why the rule stops here rather than covering every statically
    // misaligned unrelaxed access: a non-CS one has certain-exited since long before this slice,
    // and fixing that is a separate change with its own A/B.
    if addr.segment != SegmentIndex::Cs {
        return false;
    }
    if !width.needs_alignment_guard() {
        return false;
    }
    if addr.base.is_some() || addr.index.is_some() || addr.disp_lane.is_some() {
        return false;
    }
    let offset = if d { addr.disp } else { addr.disp & 0xffff };
    let linear = cpu
        .registers
        .segment(addr.segment)
        .base
        .wrapping_add(offset);
    linear & width.alignment_mask() != 0
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
