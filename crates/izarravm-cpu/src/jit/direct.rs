// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Admission and storage for page-local direct-code blocks.

mod classify;
mod emit;

use std::{collections::HashMap, sync::Arc};

use izarravm_core::CpuPersona;

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
    AddressSize, CpuGsw, DecodeGroup, DecodedInsn, DecodedOperand, OperandSize, Prefixes,
    Registers, SegmentIndex, SegmentRegister, U32BuildHasher,
};

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
use super::fast_map::{
    NATIVE_KIND_MASK, NATIVE_MODE13_KIND, NATIVE_PAGE_SHIFT, NATIVE_PAGE_USER,
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
const MAX_MEMORY_ALU_BLOCK_INSTRUCTIONS: usize = 4;
const MAX_MEMORY_ALU_SLOTS: u8 = 3;
pub(crate) const MAX_X87_BLOCK_CORE_CLOCKS: u64 = 3_928;
const DEFAULT_ENTRY_CAP: usize = 131_072;
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

    fn hot_index(self) -> usize {
        self.linear as usize & (HOT_LOOKUP_LEN - 1)
    }
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
    pub(crate) fn capture(cpu: &CpuGsw, read_segments: u8, write_segments: u8) -> Option<Self> {
        let data = SEGMENT_ORDER.map(|segment| cpu.registers.segment(segment));
        let used = read_segments | write_segments;
        for segment in SEGMENT_ORDER {
            let bit = segment_bit(segment);
            if used & bit == 0 {
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

    /// An inert all-zeros layout for sentinel descriptors (Track C C1d, design section
    /// 3.3b): filler for a descriptor whose only live fields are `entry` and `operands`;
    /// nothing ever validates or reads it. Consumed by the clif backend only.
    #[cfg(all(
        feature = "clif-backend",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    pub(crate) fn inert() -> Self {
        let zero = SegmentRegister {
            selector: 0,
            base: 0,
            limit: 0,
            access: 0,
            default_size_32: false,
        };
        Self {
            cs: zero,
            data: [zero; 6],
            used: 0,
        }
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

    pub(crate) fn link_compatible(self, target: Self) -> bool {
        self.cs == target.cs && self.data == target.data
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
    segment_layout: SegmentLayout,
    memory_cpl3: bool,
    has_wide_accesses: bool,
    self_loop: bool,
    has_x87: bool,
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

    pub(crate) fn cs_descriptor_matches(&self, cpu: &CpuGsw) -> bool {
        self.segment_layout.cs_matches(cpu)
    }

    pub(crate) fn data_descriptors_match(&self, cpu: &CpuGsw) -> bool {
        self.segment_layout.data_matches(cpu)
    }

    pub(crate) fn chain_descriptors_match(&self, cpu: &CpuGsw) -> bool {
        self.segment_layout.all_data_matches(cpu)
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

    /// Static-successor compatibility (Jmp/Jcc/Call/fallthrough edges). The dynamic RET PIC path
    /// (`try_link_inner` with a `target_eip`) layers an extra `has_x87` equality on top of this,
    /// so it stays strict; see the comment there for why.
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
    fn link_compatible(self, target: Self) -> bool {
        if self.span.key.mode_key != target.span.key.mode_key
            || self.memory_cpl3 != target.memory_cpl3
            || !self.segment_layout.link_compatible(target.segment_layout)
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
            // Integer source, float target: refused. A chained entry publishes body_ptr =
            // entry + body_offset and jumps straight there, so the target's own prologue never
            // runs. `emit_x87_enter` sits ABOVE body_offset (see `emit()`: body_offset is
            // captured right after the `x87_entry_top.is_some()` enter block), so skipping the
            // prologue means the target's XMM4-11 physical cache is never loaded from
            // `CpuGsw.fpu` and its baked compile-time entry TOP is never pinned to the CPU's
            // real `top()`. There is no boundary fix-up that helps here, unlike the float-to-
            // integer case: the missing work happens on the target side, before the jump lands,
            // not at the jump site.
            //
            // This refusal also underwrites the float-to-integer crossing's frame read above.
            // That crossing reloads RSI from STACK_SAVED_RSI, a slot only an x87 prologue writes;
            // an integer entry never runs one, so if this arm allowed the edge, an integer-headed
            // chain could reach that reload with the slot (and the XMM6-11 save area) never
            // initialized. Uniform frame length alone does not make the crossing's frame read
            // safe; this refusal is what makes it safe, by induction over the chain: every block
            // that can reach a float-to-integer crossing was itself entered through an x87
            // prologue.
            // RELAXED. An integer source may now reach a float target, because it lands on the
            // shared x87 re-entry pad rather than on `body`: the pad does exactly the work the
            // target's prologue would have done, loading the register cache into XMM4-11 and
            // packing the status/tag word into RSI, after guarding the target's baked entry TOP
            // against the CPU's live TOP.
            //
            // The frame induction the old refusal provided is restored rather than abandoned. A
            // float-to-integer crossing reloads RSI and XMM6-11 from slots only an x87 prologue
            // writes; the pad writes the same slots, so every block that can reach such a
            // crossing was entered either through a prologue or through the pad. `try_link_inner`
            // refuses this shape when no pad could be built, which keeps that induction total.
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlockState {
    Seen,
    Dormant,
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

/// Bounded direct-block cache. Hash lookup is authoritative; the direct-mapped table is only a
/// collision-checked accelerator. Capacity pressure clears the entire cache.
pub(crate) struct BlockCache {
    entries: HashMap<BlockKey, BlockState>,
    physical_keys: HashMap<u32, Vec<BlockKey>, U32BuildHasher>,
    blocks: Vec<CompiledBlock>,
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
    /// The `jit_cost_dial_epoch()` the cache above was computed under. The CPU cannot see a bus
    /// dial move, so the memo is keyed on the bus's own epoch rather than on an argument about
    /// who writes the dials. Reading one accessor and comparing beats six accessor calls, five
    /// `max`, three multiplies and a division.
    global_block_upper_epoch: u64,
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
            entries: HashMap::new(),
            physical_keys: HashMap::default(),
            blocks: Vec::new(),
            block_portals: Vec::new(),
            link_cells: Vec::new(),
            link_sources: HashMap::new(),
            outbound: Vec::new(),
            global_block_upper_cache: [0; 2],
            x87_pad: None,
            global_block_upper_epoch: 0,
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
    pub(crate) fn set_fast_map_enabled_for_test(&mut self, enabled: bool) {
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
            Some(BlockState::Dormant | BlockState::Rejected(_)) => BlockProbe::Rejected,
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

    /// Install bytes produced after `probe` returned `Compile`.
    pub(crate) fn install(
        &mut self,
        watch: &mut NativeCodeWatch,
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
            segment_layout: compilation.segment_layout,
            memory_cpl3: compilation.memory_cpl3,
            has_wide_accesses: compilation.has_wide_accesses,
            self_loop: compilation.self_loop,
            has_x87: compilation.has_x87,
            x87_entry_top: compilation.x87_entry_top,
            x87_exit_top: compilation.x87_exit_top,
            dynamic_successor: compilation.dynamic_successor,
            successors: compilation.successors,
        };
        watch.acquire_range(span.key.physical, u32::from(span.guest_len));
        if index == self.blocks.len() {
            self.blocks.push(block);
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
            self.link_cells[index] = compilation.link_cells.clone();
            self.outbound[index] = [None, None];
            self.dynamic_next_slots[index] = 0;
            self.block_link_epochs[index] = 0;
            self.block_active[index] = true;
        }
        self.register_decode_dependencies(id, &decode_slots[..decode_slot_len]);
        if compilation.dynamic_successor {
            let cell = &compilation.link_cells[0];
            self.link_sources
                .insert(cell.address(), LinkSource { block: id, slot: 0 });
        }
        self.live_blocks += 1;
        self.entries.insert(span.key, BlockState::Compiled(id));
        self.hot[span.key.hot_index()] = Some(HotEntry {
            key: span.key,
            id,
            generation: self.hot_generation,
        });
        self.make_link_visible(id);
        Some(id)
    }

    /// Prevent repeated compilation attempts for a block the emitter cannot handle.
    pub(crate) fn reject(&mut self, watch: &mut NativeCodeWatch, span: RejectedSpan) {
        if self.entries.get(&span.key) == Some(&BlockState::Seen) {
            watch.acquire_range(span.key.physical, u32::from(span.guest_len));
            self.entries.insert(span.key, BlockState::Rejected(span));
        }
    }

    /// Keep a non-structural failure on the interpreter until an explicit cache reset or a new
    /// mode/translation key makes another admission attempt meaningful.
    pub(crate) fn dormant(&mut self, key: BlockKey) {
        if self.entries.get(&key) == Some(&BlockState::Seen) {
            self.entries.insert(key, BlockState::Dormant);
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

    pub(crate) fn demote_smc_hot(&mut self, heat: &mut SmcHeatMap, key: BlockKey, epoch: u32) {
        self.dormant(key);
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
        if self.entries.get(&key) == Some(&BlockState::Dormant)
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
        for sources in self.inbound.values() {
            for source in sources {
                let index = source.block.index();
                if self.active_index(source.block) == Some(index) {
                    self.link_cells[index][usize::from(source.slot)].clear();
                    self.outbound[index][usize::from(source.slot)] = None;
                    links += 1;
                }
            }
        }
        self.inbound.clear();
        self.waiting.clear();
        self.linear_blocks.clear();
        self.stats.unlinks += links;
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
    pub(crate) fn invalidate_physical_range(
        &mut self,
        watch: &mut NativeCodeWatch,
        physical: u32,
        width: u32,
    ) -> usize {
        if width == 0 || self.entries.is_empty() {
            return 0;
        }

        let mut invalidated = 0;
        let mut cursor = physical;
        let mut remaining = width;
        while remaining != 0 {
            let page = cursor >> BLOCK_PAGE_SHIFT;
            let page_remaining =
                (1u32 << BLOCK_PAGE_SHIFT) - (cursor & ((1u32 << BLOCK_PAGE_SHIFT) - 1));
            let step = remaining.min(page_remaining);
            if let Some(mut keys) = self.physical_keys.remove(&page) {
                let mut survivor_count = 0;
                for index in 0..keys.len() {
                    let key = keys[index];
                    let Some(state) = self.entries.get(&key).copied() else {
                        continue;
                    };
                    let overlaps = match state {
                        BlockState::Seen | BlockState::Dormant => {
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
                        BlockState::Seen | BlockState::Dormant => {}
                    }
                    invalidated += 1;
                }
                keys.truncate(survivor_count);
                if !keys.is_empty() {
                    self.physical_keys.insert(page, keys);
                }
            }
            cursor = cursor.wrapping_add(step);
            remaining -= step;
        }
        invalidated
    }

    pub(crate) fn len(&self) -> usize {
        self.live_blocks
    }

    pub(crate) fn block(&self, id: BlockId) -> Option<CompiledBlock> {
        self.active_index(id)
            .and_then(|index| self.blocks.get(index).copied())
    }

    pub(crate) fn take_stats(&mut self) -> BlockCacheStats {
        std::mem::take(&mut self.stats)
    }

    pub(crate) fn has_linked_successor(&self, block: CompiledBlock) -> bool {
        self.active_index(block.id)
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
        for cells in &self.link_cells {
            cells[0].clear();
            cells[1].clear();
        }
        self.stats.unlinks += links;
        self.stats.cache_resets += 1;
        self.entries.clear();
        self.physical_keys.clear();
        self.blocks.clear();
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
            return false;
        };
        let Some(target_index) = self.active_index(target) else {
            return false;
        };
        let source_block = self.blocks[source_index];
        let target_block = self.blocks[target_index];
        if self.block_link_epochs.get(source_index).copied() != Some(self.link_epoch)
            || self.block_link_epochs.get(target_index).copied() != Some(self.link_epoch)
            || !source_block.link_compatible(target_block)
            // The dynamic RET PIC path resolves a near-RET target at runtime from an arbitrary
            // return address, not from a compile-time successor shape, and
            // emit_completed_dynamic_path never emits the boundary spill link_compatible's
            // float-to-integer case relies on. So RET PIC keeps the strict has_x87 equality on
            // top of the relaxed rule; static successors (target_eip == None, resolved above by
            // resolve_successors/resolve_waiting) get the full relaxed rule instead.
            //
            // This equality is ALSO what keeps the shared x87 re-entry pad safe on that path.
            // `emit_completed_dynamic_path` loads `BlockPortal::body` unconditionally, not
            // `integer_entry`, so it would bypass the pad. Same class on both ends means the two
            // fields are equal for every target it can bind, so the bypass is unobservable.
            // Relaxing this line without teaching that path the pad is a silent wrong-entry bug.
            || (target_eip.is_some() && source_block.has_x87 != target_block.has_x87)
        {
            return false;
        }
        // An integer source reaching a float target goes through the shared pad. Without one there
        // is no correct address to publish: `body` would enter the target with an unloaded x87
        // register cache. Refusing here leaves the cell on the zero portal, so the exit reports
        // `StaticUnbound` exactly as it did before the pad existed.
        if !source_block.has_x87 && target_block.has_x87 && self.x87_pad_address().is_none() {
            return false;
        }
        let slot_index = usize::from(slot);
        if self.outbound[source_index][slot_index] == Some(target) {
            if let Some(target_eip) = target_eip {
                self.link_cells[source_index][slot_index]
                    .set_dynamic(target_eip, self.block_portals[target_index].as_ref());
            }
            return true;
        }
        self.unlink_outbound(source, slot);
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
        true
    }

    fn unlink_outbound(&mut self, source: BlockId, slot: u8) {
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
                    self.link_cells[source_index][usize::from(link.slot)].clear();
                    self.outbound[source_index][usize::from(link.slot)] = None;
                    if let Some(successor) =
                        self.blocks[source_index].successors[usize::from(link.slot)]
                    {
                        self.waiting.entry(successor).or_default().push(link);
                    }
                    self.stats.unlinks += 1;
                }
            }
        }
        for slot in 0..2 {
            self.unlink_outbound(id, slot);
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
        self.block_active[index] = false;
        self.blocks[index].entry = 0;
        self.blocks[index].body_entry = 0;
        self.free_block_slots
            .push(u16::try_from(index).expect("block slot index must fit its ID"));
        self.live_blocks -= 1;
        watch.release_range(span.key.physical, u32::from(span.guest_len));
    }

    fn track_physical_key(&mut self, key: BlockKey) {
        self.physical_keys
            .entry(key.physical >> BLOCK_PAGE_SHIFT)
            .or_default()
            .push(key);
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

pub(crate) type DirectEntryFn = unsafe extern "C" fn(*mut CpuGsw, u32, u32, *mut NativeExit);

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SideExitReason {
    None = 0,
    CrossPageOrAlignment = 1,
    UnavailableOrKind = 2,
    Permission = 3,
    CodeWatch = 4,
    Other = 5,
}

#[repr(u32)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnresolvedReason {
    #[default]
    None = 0,
    StaticUnbound,
    StaticHidden,
    DynamicMissOrUnbound,
    DynamicHidden,
    /// The shared x87 re-entry pad refused the crossing: the target float block's baked entry TOP
    /// does not match the CPU's live TOP, so its register cache cannot be entered for it.
    X87TopMismatch,
}

/// Fetch replay retained for buses that observe individual code addresses. Production RAM timing
/// uses the aggregate counters in `NativeExit` and leaves this trace disabled.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NativeBlockTrace {
    pub(crate) linear: u32,
    pub(crate) physical: u32,
    pub(crate) repetitions: u32,
    pub(crate) prefix_instructions: u32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NativeExit {
    pub(crate) instructions: u64,
    pub(crate) raw_clocks: u64,
    // Byte counters use the low lane and word counters use the high lane. Native chain bounds
    // keep both 32-bit lanes well below overflow while preserving the original exit layout.
    pub(crate) byte_reads: u64,
    pub(crate) dword_reads: u64,
    pub(crate) weighted_fp_clocks: u64,
    pub(crate) mode13_byte_reads: u64,
    pub(crate) mode13_dword_reads: u64,
    pub(crate) ram_byte_writes: u64,
    pub(crate) ram_dword_writes: u64,
    pub(crate) mode13_byte_writes: u64,
    pub(crate) mode13_dword_writes: u64,
    pub(crate) mode13_dirty_pages: u64,
    pub(crate) side_exit: u64,
    pub(crate) side_exit_reason: u32,
    pub(crate) trace_len: u32,
    pub(crate) linked_transfers: u32,
    pub(crate) unresolved_reason: UnresolvedReason,
    pub(crate) trace_ptr: usize,
    pub(crate) dynamic_link_cell: usize,
    pub(crate) dynamic_target_eip: u32,
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
    link_cells: [Arc<LinkCell>; 2],
    body_offset: usize,
    pub code: Vec<u8>,
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
    MovImm {
        dst: u8,
        imm: u32,
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
    },
    AluByteImm {
        op: u8,
        dst: u8,
        imm: u8,
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
    /// ROR r/m32, register form (0xC1 /1 and 0xD1 /1). `count` is the RAW decoded immediate; the
    /// emitter applies the architectural five-bit mask, exactly as `Shift` does.
    ///
    /// Deliberately NOT folded into `Shift`. That variant is in clif's lowerable allowlist and its
    /// lowering falls through to an arithmetic shift right, so a rotate routed through it would be
    /// silently emitted as SAR. It also differs in the flag contract that matters here: a shift
    /// leaves AF, and OF above count 1, architecturally UNDEFINED, which is the only reason
    /// `emit_shift` may publish a possibly stale RBP to eflags. A rotate PRESERVES SF, ZF, PF and
    /// AF, so it must not.
    RotateRightReg {
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
    Shift {
        op: u8,
        dst: u8,
        count: u8,
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
    /// MOVZX/MOVSX r32, r/m8 or r/m16, MEMORY form (0x0FB6, 0x0FB7, 0x0FBE, 0x0FBF).
    ///
    /// `width` is the SOURCE width and is only ever Byte or Word. This differs from `Load`, where
    /// the source and destination widths are the same: here the destination is always the full
    /// 32-bit register, which is the whole point of the instruction. Any shared code that reads
    /// this field must treat it as the memory access width, never as the write-back width.
    ///
    /// Deliberately NOT a flag on `Load`. `Load` is in clif's lowerable allowlist and `lower_slot`
    /// would lower an extending load as a plain move, silently and wrongly. A new discriminant is
    /// absent from that allowlist and so defaults to a growth-run stopper.
    LoadExtend {
        dst: u8,
        width: MemoryWidth,
        signed: bool,
        addr: DirectAddr,
        raw_clocks: u8,
    },
    /// MOVZX/MOVSX r32, r8 or r16, REGISTER form (0x0FB6, 0x0FB7, 0x0FBE, 0x0FBF, mod == 3).
    ///
    /// `width` is the SOURCE width and is only ever Byte or Word; the destination is always the
    /// full 32-bit register, which is the point of the instruction.
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
    /// A SEPARATE variant rather than a width field on `Push`, because `Push` is in clif's
    /// `lowerable()` allowlist and `lower_push` hard-codes `MemoryWidth::Dword` and
    /// `iadd_imm(esp, -4)`, so a field would be lowered as a 32-bit push there. The two widths
    /// it stands for are ORTHOGONAL: SS.B picks the stack-pointer width and `operand_size` picks
    /// how many bytes move (386 PRM 16.2, restated at `memory.rs:1218`). This variant is the
    /// (SS.B = 0, Word) cell only; the compile loop refuses the other two new cells.
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
    /// Separate variant for the same reason as `Push16`: `Pop` is in clif's `lowerable()`
    /// allowlist and `lower_pop` hard-codes the 32-bit width, the +4 advance AND a full 32-bit
    /// destination write.
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
    ///
    /// Absent from clif's `lowerable()` allowlist, which does NOT stop unit growth (that is
    /// `unit_growth_classify`, which shares this classifier): it stops LOWERING, so a unit whose
    /// entry slot is a NOP parks with `plan.leading == 0` and stays on the interpreter, exactly
    /// as it did while the opcode was unclassifiable.
    Nop,
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
    X87 {
        insn: NativeX87Insn,
        addr: Option<DirectAddr>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemoryWidth {
    Byte,
    Word,
    Dword,
}

/// The `MemoryWidth` of one x87 memory access, and the SINGLE source of truth for it.
///
/// Every consumer routes through here: `word_reads`, `dword_reads`, `word_stores`,
/// `dword_stores`, `has_dword_read`, `has_dword_store`, `dynamic_counter_mask` and the emitter.
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
        }
    }

    pub(crate) const fn needs_alignment_guard(self) -> bool {
        !matches!(self, Self::Byte)
    }
}

#[derive(Clone, Copy)]
pub(crate) enum StoreSource {
    Reg(u8),
    Imm(u32),
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
            Self::Call { .. } | Self::Call16 { .. } | Self::Jmp { .. } => 7,
            // Both widths charge the same: 0xc2 and 0xc3 return clocks(10) irrespective of
            // operand size. An omitted arm here falls to `_ => 2` and undercharges by 8.
            Self::Ret { .. } | Self::Ret16 { .. } => 10,
            Self::DoubleShiftReg { .. } | Self::DoubleShiftMem { .. } => 3,
            Self::Load { raw_clocks, .. }
            | Self::LoadExtend { raw_clocks, .. }
            | Self::Store { raw_clocks, .. } => u32::from(raw_clocks),
            // All four MOVZX/MOVSX interpreter arms return clocks(3) for BOTH operand forms
            // (execute.rs, and the hot-cached path in run.rs charges the same), against a default
            // of 2. The memory forms carry it as a field because Load and Store do; the register
            // form has no other field worth carrying, so it is a constant arm.
            Self::MovExtendReg { .. } => 3,
            Self::X87 { .. } => 0,
            // Matches the interpreter's clocks(9) for 0x0FAF at execute_extended.rs. The default
            // arm below returns 2, which would under-charge this instruction by 7. Both operand
            // forms share the arm because the interpreter charges them from one `Ok(clocks(9))`.
            Self::Imul { .. } | Self::ImulMem { .. } => 9,
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
            ) || x87_memory_access_is(self, NativeX87MemoryDirection::Read, MemoryWidth::Dword),
        )
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
            ) || x87_memory_access_is(self, NativeX87MemoryDirection::Write, MemoryWidth::Word),
        )
    }

    pub(crate) fn dword_stores(self) -> u8 {
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
                    | Self::PushMem { .. }
            ) || matches!(
                self,
                Self::AluMemDest {
                    op: 0..=6,
                    width: MemoryWidth::Dword,
                    ..
                }
            ) || x87_memory_access_is(self, NativeX87MemoryDirection::Write, MemoryWidth::Dword),
        )
    }

    #[cfg(test)]
    fn dynamic_counter_mask(self) -> u16 {
        match self {
            Self::Load {
                width: MemoryWidth::Byte,
                ..
            } => COUNTER_MODE13_BYTE_READ,
            Self::Load {
                width: MemoryWidth::Word,
                ..
            } => COUNTER_MODE13_BYTE_READ,
            Self::Load {
                width: MemoryWidth::Dword,
                ..
            } => COUNTER_MODE13_DWORD_READ,
            // The extending loads mirror Load's read arms exactly. Only the SOURCE width matters
            // to the counters, and it is only ever Byte or Word, so both land on the byte-read
            // counter the way Load's Byte and Word arms do.
            Self::LoadExtend { width, .. } => match width {
                MemoryWidth::Byte | MemoryWidth::Word => COUNTER_MODE13_BYTE_READ,
                MemoryWidth::Dword => COUNTER_MODE13_DWORD_READ,
            },
            // One dword read and nothing else, the same as AluMemSource's Dword arm.
            Self::ImulMem { .. } | Self::ImulMemAcc { .. } => COUNTER_MODE13_DWORD_READ,
            Self::Store {
                width: MemoryWidth::Byte,
                ..
            } => COUNTER_RAM_BYTE_WRITE | COUNTER_MODE13_BYTE_WRITE | COUNTER_MODE13_DIRTY,
            Self::Store {
                width: MemoryWidth::Word,
                ..
            } => COUNTER_RAM_BYTE_WRITE | COUNTER_MODE13_BYTE_WRITE | COUNTER_MODE13_DIRTY,
            Self::Store {
                width: MemoryWidth::Dword,
                ..
            } => COUNTER_RAM_DWORD_WRITE | COUNTER_MODE13_DWORD_WRITE | COUNTER_MODE13_DIRTY,
            Self::AluMemSource { width, .. } => match width {
                MemoryWidth::Byte => COUNTER_MODE13_BYTE_READ,
                MemoryWidth::Word => COUNTER_MODE13_BYTE_READ,
                MemoryWidth::Dword => COUNTER_MODE13_DWORD_READ,
            },
            Self::AluMemDest { op, width, .. } => {
                let read = match width {
                    MemoryWidth::Byte => COUNTER_MODE13_BYTE_READ,
                    MemoryWidth::Word => COUNTER_MODE13_BYTE_READ,
                    MemoryWidth::Dword => COUNTER_MODE13_DWORD_READ,
                };
                if op == 7 {
                    read
                } else {
                    read | match width {
                        MemoryWidth::Byte => COUNTER_RAM_BYTE_WRITE | COUNTER_MODE13_BYTE_WRITE,
                        MemoryWidth::Word => COUNTER_RAM_BYTE_WRITE | COUNTER_MODE13_BYTE_WRITE,
                        MemoryWidth::Dword => COUNTER_RAM_DWORD_WRITE | COUNTER_MODE13_DWORD_WRITE,
                    } | COUNTER_MODE13_DIRTY
                }
            }
            Self::DoubleShiftMem { .. } => {
                COUNTER_RAM_DWORD_WRITE
                    | COUNTER_MODE13_DWORD_READ
                    | COUNTER_MODE13_DWORD_WRITE
                    | COUNTER_MODE13_DIRTY
            }
            Self::TestImmMem {
                width: MemoryWidth::Byte,
                ..
            } => COUNTER_MODE13_BYTE_READ,
            Self::TestImmMem {
                width: MemoryWidth::Word,
                ..
            } => COUNTER_MODE13_BYTE_READ,
            Self::TestImmMem {
                width: MemoryWidth::Dword,
                ..
            } => COUNTER_MODE13_DWORD_READ,
            // A Word access lands on the BYTE counter slots, because `emit_dynamic_word_increment`
            // packs the word count into the upper 32 bits of the byte slot. Same convention as
            // Load, Store and AluMemSource above.
            Self::X87 { insn, .. } => match insn
                .metadata()
                .memory
                .map(|access| (access.direction, x87_memory_width(access)))
            {
                Some((NativeX87MemoryDirection::Read, MemoryWidth::Dword)) => {
                    COUNTER_MODE13_DWORD_READ
                }
                Some((NativeX87MemoryDirection::Read, _)) => COUNTER_MODE13_BYTE_READ,
                Some((NativeX87MemoryDirection::Write, MemoryWidth::Dword)) => {
                    COUNTER_RAM_DWORD_WRITE | COUNTER_MODE13_DWORD_WRITE | COUNTER_MODE13_DIRTY
                }
                Some((NativeX87MemoryDirection::Write, _)) => {
                    COUNTER_RAM_BYTE_WRITE | COUNTER_MODE13_BYTE_WRITE | COUNTER_MODE13_DIRTY
                }
                None => 0,
            },
            Self::RmwIncDec { width, .. } => match width {
                MemoryWidth::Byte => COUNTER_RAM_BYTE_WRITE,
                MemoryWidth::Word => {
                    COUNTER_RAM_BYTE_WRITE
                        | COUNTER_MODE13_BYTE_READ
                        | COUNTER_MODE13_BYTE_WRITE
                        | COUNTER_MODE13_DIRTY
                }
                MemoryWidth::Dword => COUNTER_RAM_DWORD_WRITE,
            },
            Self::Pop { .. } | Self::Leave | Self::Ret { .. } => COUNTER_MODE13_DWORD_READ,
            // A Word read lands on the BYTE counter lane, matching `Load { Word }`:
            // `emit_mode13_read_completion` routes Word to the byte-read slot.
            Self::Pop16 { .. } | Self::Ret16 { .. } => COUNTER_MODE13_BYTE_READ,
            Self::Push { .. } | Self::Call { .. } => {
                COUNTER_RAM_DWORD_WRITE | COUNTER_MODE13_DWORD_WRITE | COUNTER_MODE13_DIRTY
            }
            // A Word store lands on the BYTE counter slots, matching `Store { Word }` above:
            // `emit_dynamic_word_increment` packs the word count into the upper 32 bits of the
            // byte slot. Mirroring the Word store is the rule here rather than reasoning from
            // "the bus ignores width", which is true of `BusCycle::clocks_for` but NOT of these
            // counter lanes.
            Self::Push16 { .. } | Self::Call16 { .. } => {
                COUNTER_RAM_BYTE_WRITE | COUNTER_MODE13_BYTE_WRITE | COUNTER_MODE13_DIRTY
            }
            // No read lane. The source read is RAM-only, and `run.rs` derives RAM reads by
            // subtracting the mode-13 dynamic count from the static count, so no dynamic read
            // counter is emitted. `emit_rmw_inc_dec_dword` is the precedent: it reads and writes
            // and increments only the write lane.
            Self::PushMem { .. } => COUNTER_RAM_DWORD_WRITE,
            _ => 0,
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
            | Self::RmwIncDec { addr, .. }
            | Self::PushMem { addr, .. } => Some(addr.segment),
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
            | Self::PushMem { .. } => Some(SegmentIndex::Ss),
            _ => None,
        }
    }

    fn has_dword_read(self) -> bool {
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
        ) || x87_memory_access_is(self, NativeX87MemoryDirection::Read, MemoryWidth::Dword)
    }

    fn has_dword_store(self) -> bool {
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
                | Self::PushMem { .. }
        ) || matches!(
            self,
            Self::AluMemDest {
                op: 0..=6,
                width: MemoryWidth::Dword,
                ..
            }
        ) || x87_memory_access_is(self, NativeX87MemoryDirection::Write, MemoryWidth::Dword)
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
        )
    }

    fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Call { .. }
                | Self::Call16 { .. }
                | Self::Jmp { .. }
                | Self::Ret { .. }
                | Self::Ret16 { .. }
                | Self::Jcc { .. }
        )
    }

    fn is_x87(self) -> bool {
        matches!(self, Self::X87 { .. })
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

const GUEST_HOMES: [Reg; 8] = [
    Reg::R8,
    Reg::R9,
    Reg::R10,
    Reg::R11,
    Reg::R12,
    Reg::R13,
    Reg::R14,
    Reg::RBX,
];
const SAVED_HOST_REGS: [Reg; 7] = [
    Reg::RBX,
    Reg::RBP,
    Reg::RDI,
    Reg::R12,
    Reg::R13,
    Reg::R14,
    Reg::R15,
];
const ARITH_FLAGS: u32 = crate::FLAG_CF
    | crate::FLAG_PF
    | crate::FLAG_AF
    | crate::FLAG_ZF
    | crate::FLAG_SF
    | crate::FLAG_OF;
const LOGIC_FLAGS: u32 = ARITH_FLAGS & !crate::FLAG_AF;

#[cfg(target_os = "windows")]
const CPU_ARG: Reg = Reg::RCX;
#[cfg(not(target_os = "windows"))]
const CPU_ARG: Reg = Reg::RDI;
#[cfg(target_os = "windows")]
const FLAGS_ARG: Reg = Reg::RDX;
#[cfg(not(target_os = "windows"))]
const FLAGS_ARG: Reg = Reg::RSI;
#[cfg(target_os = "windows")]
const QUOTA_ARG: Reg = Reg::R8;
#[cfg(not(target_os = "windows"))]
const QUOTA_ARG: Reg = Reg::RDX;
#[cfg(target_os = "windows")]
const EXIT_ARG: Reg = Reg::R9;
#[cfg(not(target_os = "windows"))]
const EXIT_ARG: Reg = Reg::RCX;

// Base frame: 20 accounting and scratch slots at 8 bytes each, offsets 0 to
// 152 below (STACK_QUOTA through STACK_SHIFT_COUNT), filling 160 bytes.
const BASE_STACK_LEN: u32 = 160;
// One frame shape for every block, x87-bearing or not. A chained native
// transfer jumps straight into a target block's body, skipping its
// prologue, so the target's own epilogue always runs against whatever
// frame the entering block's prologue built. If the two frame shapes
// differ, that teardown pops the wrong bytes. On Windows the frame also
// carries the saved-RSI slot below and the x87 XMM6-11 save area; RSI is
// callee-saved there and doubles as the x87 tag-cache scratch register, and
// none of the XMM6-11 registers are. On non-Windows RSI is not
// callee-saved and there is no non-volatile XMM to save, so the frame is
// just the base.
#[cfg(target_os = "windows")]
const NATIVE_STACK_LEN: u32 = BASE_STACK_LEN + 8 + 6 * 16;
#[cfg(not(target_os = "windows"))]
const NATIVE_STACK_LEN: u32 = BASE_STACK_LEN;
const STACK_QUOTA: i8 = 0;
const STACK_ITERATIONS: i8 = 8;
const STACK_RAM_BYTE_WRITES: i8 = 16;
const STACK_RAM_DWORD_WRITES: i8 = 24;
const STACK_MODE13_BYTE_WRITES: i8 = 32;
const STACK_MODE13_DWORD_WRITES: i8 = 40;
const STACK_MODE13_DIRTY_PAGES: i8 = 48;
const STACK_EXIT: i8 = 56;
const STACK_MODE13_BYTE_READS: i8 = 64;
const STACK_MODE13_DWORD_READS: i8 = 72;
const STACK_READ_KIND: i8 = 80;
const STACK_WATCH_PAGE: i8 = STACK_READ_KIND;
const STACK_WEIGHTED_FP_CLOCKS: i8 = 88;
const STACK_INSTRUCTIONS: i8 = 96;
const STACK_RAW_CLOCKS: i8 = 104;
const STACK_BYTE_READS: i8 = 112;
const STACK_DWORD_READS: i8 = 120;
const STACK_ALU_ADDRESS_KIND: i32 = 128;
const STACK_ALU_OLD_RESULT: i32 = 136;
/// Where `emit_push_mem` parks the dword it read from the source operand, across the stack
/// store's own address and kind path, which clobbers RAX, RCX, RDX and RDI. Those four are the
/// whole scratch set: `GUEST_HOMES` is R8 to R14 plus RBX, R15 is the CPU pointer, RBP is the
/// guest flag shadow, and RSI is host callee-saved and spilled only for x87 blocks.
///
/// Aliased onto the ALU slot deliberately. `PushMem` is not an ALU kind, the two never appear in
/// one slot's emission, and every use of either is written and read inside a single slot. It must
/// NOT be `STACK_READ_KIND`: `emit_code_watch_branch` writes `STACK_WATCH_PAGE`, which is the
/// same slot, on the store's path.
///
/// 136 is outside disp8 range, so this slot is reached with the disp32 load and store forms.
const STACK_PUSH_MEM_VALUE: i32 = STACK_ALU_OLD_RESULT;
const STACK_ALU_FLAGS: i32 = 144;
const STACK_SHIFT_COUNT: i32 = 152;
// Beyond the base frame: the saved host RSI slot, then the x87 XMM6-11
// save area right after it. Both Windows only, see NATIVE_STACK_LEN above.
#[cfg(target_os = "windows")]
const STACK_SAVED_RSI: i32 = BASE_STACK_LEN as i32;
#[cfg(target_os = "windows")]
const STACK_X87_XMM_BASE: i32 = STACK_SAVED_RSI + 8;
// The saved-RSI slot and the XMM6-11 save area must both land inside the frame NATIVE_STACK_LEN
// actually allocates. A wrong STACK_X87_XMM_BASE (a stale copy of an old constant, say) would
// make the first XMM save overwrite the saved RSI slot and hand garbage RSI back to the Rust
// caller, silently, since the frame-size test only checks the sub rsp / add rsp immediates
// agree, not that the areas inside the frame do not collide.
#[cfg(target_os = "windows")]
const _: () = {
    assert!(STACK_SAVED_RSI as u32 + 8 <= STACK_X87_XMM_BASE as u32);
    assert!(
        STACK_X87_XMM_BASE as u32 + emit::X87_NONVOLATILE_XMMS.len() as u32 * 16
            <= NATIVE_STACK_LEN
    );
};
const COUNTER_RAM_BYTE_WRITE: u16 = 1 << 0;
const COUNTER_RAM_DWORD_WRITE: u16 = 1 << 1;
const COUNTER_MODE13_BYTE_WRITE: u16 = 1 << 2;
const COUNTER_MODE13_DWORD_WRITE: u16 = 1 << 3;
const COUNTER_MODE13_DIRTY: u16 = 1 << 4;
const COUNTER_MODE13_BYTE_READ: u16 = 1 << 5;
const COUNTER_MODE13_DWORD_READ: u16 = 1 << 6;
const COUNTER_ALL: u16 = COUNTER_RAM_BYTE_WRITE
    | COUNTER_RAM_DWORD_WRITE
    | COUNTER_MODE13_BYTE_WRITE
    | COUNTER_MODE13_DWORD_WRITE
    | COUNTER_MODE13_DIRTY
    | COUNTER_MODE13_BYTE_READ
    | COUNTER_MODE13_DWORD_READ;

#[cfg(test)]
fn dynamic_counter_mask(slots: &[DirectInsn]) -> u16 {
    slots
        .iter()
        .fold(0, |mask, slot| mask | slot.kind.dynamic_counter_mask())
}

fn dynamic_counter_fields() -> [(u16, i8, usize); 7] {
    [
        (
            COUNTER_RAM_BYTE_WRITE,
            STACK_RAM_BYTE_WRITES,
            core::mem::offset_of!(NativeExit, ram_byte_writes),
        ),
        (
            COUNTER_RAM_DWORD_WRITE,
            STACK_RAM_DWORD_WRITES,
            core::mem::offset_of!(NativeExit, ram_dword_writes),
        ),
        (
            COUNTER_MODE13_BYTE_WRITE,
            STACK_MODE13_BYTE_WRITES,
            core::mem::offset_of!(NativeExit, mode13_byte_writes),
        ),
        (
            COUNTER_MODE13_DWORD_WRITE,
            STACK_MODE13_DWORD_WRITES,
            core::mem::offset_of!(NativeExit, mode13_dword_writes),
        ),
        (
            COUNTER_MODE13_DIRTY,
            STACK_MODE13_DIRTY_PAGES,
            core::mem::offset_of!(NativeExit, mode13_dirty_pages),
        ),
        (
            COUNTER_MODE13_BYTE_READ,
            STACK_MODE13_BYTE_READS,
            core::mem::offset_of!(NativeExit, mode13_byte_reads),
        ),
        (
            COUNTER_MODE13_DWORD_READ,
            STACK_MODE13_DWORD_READS,
            core::mem::offset_of!(NativeExit, mode13_dword_reads),
        ),
    ]
}

/// Terminal kinds for the clif unit-boundary growth walker (Track C C1a, F-A5).
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(crate) enum UnitTerminal {
    Jcc { taken_delta: u32 },
    Jmp,
    Call,
    Ret,
}

/// One growth-walk classification step for the clif walker (Track C C1a, F-A5).
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(crate) struct UnitGrowthStep {
    pub(crate) terminal: Option<UnitTerminal>,
    pub(crate) wide_access: bool,
    pub(crate) read_segments: u8,
    pub(crate) write_segments: u8,
    /// The full classification, carried so the C1b lowering compiles the same shape the
    /// walker admitted (the walker's reduced fields above stay authoritative for layout).
    pub(crate) kind: DirectKind,
}

/// Classify one decoded instruction for clif unit growth with the SAME classifier the
/// Direct compiler uses (`classify::classify`, reused unchanged), reduced to the fields
/// the walker needs. `None` is the stop-growth signal: the first structurally
/// unclassifiable opcode ends the unit before it (plan Q1 resolution). Wide-access uses
/// Direct's exact rule (the `has_wide_accesses` accumulation in `compile`); the word-gate
/// persona restriction there is a compile heuristic, not a classification fact, so it is
/// deliberately NOT replicated here.
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(crate) fn unit_growth_classify(
    insn: &DecodedInsn,
    lin: u32,
    entry_lin: u32,
) -> Option<UnitGrowthStep> {
    let kind = classify::classify(insn, lin, entry_lin)?;
    let wide_access = kind.has_word_access() || kind.has_dword_read() || kind.has_dword_store();
    let read_segments = kind.read_segment().map_or(0, segment_bit);
    let write_segments = kind.write_segment().map_or(0, segment_bit);
    let terminal = match kind {
        DirectKind::Jcc { taken_delta, .. } => Some(UnitTerminal::Jcc { taken_delta }),
        DirectKind::Jmp { .. } => Some(UnitTerminal::Jmp),
        DirectKind::Call { .. } => Some(UnitTerminal::Call),
        DirectKind::Ret { .. } => Some(UnitTerminal::Ret),
        _ => None,
    };
    Some(UnitGrowthStep {
        terminal,
        wide_access,
        read_segments,
        write_segments,
        kind,
    })
}

/// Whether this backend supports `prefixes` for an instruction decoded at `operand_size` in a
/// code segment whose default size is `d`.
///
/// The operand-size override is the ONLY prefix the backend supports, and whether it is present
/// for a given `operand_size` depends on the segment width, because `decode` computes
/// `operand_size = default_32 XOR operand_size_override`. Deriving the expected override from `d`
/// keeps this exact in both widths.
///
/// Under `d == true` this is byte-identical to the hard-coded form it replaced: Dword expects no
/// override, Word expects one. Under `d == false` the mapping INVERTS, and the old form rejected
/// BOTH arms, so every 16-bit instruction was refused here as `PrefixesUnsupported` no matter what
/// the classifier could lower. Nothing 16-bit reaches this today (`key_for` refuses on `!d`), so
/// this is a precondition for that work rather than a behaviour change.
fn prefixes_supported_for(prefixes: Prefixes, operand_size: OperandSize, d: bool) -> bool {
    prefixes
        == Prefixes {
            operand_size_override: (operand_size == OperandSize::Dword) != d,
            ..Prefixes::default()
        }
}

pub(crate) fn key_for(cpu: &CpuGsw, lin: u32, d: bool) -> Option<BlockKey> {
    if !super::host_supported() || !matches!(cpu.persona(), CpuPersona::I486 | CpuPersona::I586) {
        return None;
    }
    // A 16-bit code segment is admitted on I586 only, and the persona clause is load-bearing
    // rather than tidiness. Every instruction in such a segment decodes at `OperandSize::Word`
    // (the size follows CS.D, not the opcode), and the compile loop structurally rejects Word on
    // any persona but I586. On I486 the whole population would therefore reach `classify`, fail
    // on its FIRST slot, and return a `StructuralReject`, which installs a rejected span and a
    // physical-page watch for every hot 16-bit boundary. That is a real cost for a yield that is
    // exactly zero. Refusing here keeps a 486 guest byte-identical by construction rather than
    // by measurement.
    //
    // The 16-bit population is real mode, V86 and 16-bit protected mode. V86 is deliberately IN:
    // no V86-sensitive opcode is classifiable at all (`classify` has no PUSHF/POPF, CLI/STI,
    // INT/IRET or IN/OUT arm), and V86 blocks are key-separated by mode-key bit 2.
    if !d && cpu.persona() != CpuPersona::I586 {
        return None;
    }
    if lin.wrapping_sub(0x000f_f000) < 0x400 {
        return None;
    }
    let physical = cpu.decode_cache.line_phys_start(lin, d)?;
    // The first direct slice has no page-kind guard in emitted code. Keep video and ROM code on
    // the interpreter until the shared fast map can prove a page is ordinary RAM.
    if (0x000a_0000..0x0010_0000).contains(&physical) {
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

fn compile_with_instruction_limit(
    cpu: &mut CpuGsw,
    entry_lin: u32,
    d: bool,
    instruction_limit: usize,
) -> CompileOutcome {
    let Some(key) = key_for(cpu, entry_lin, d) else {
        return CompileOutcome::Retry;
    };
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
    let mut has_wide_accesses = false;
    let mut stack_accesses = 0u8;
    let mut x87_slots = 0u8;
    let x87_entry_top = cpu.fpu.top();
    let mut x87_exit_top = x87_entry_top;
    let mut memory_alu_slots = 0u8;
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
        if !prefixes_supported || !insn.continuable {
            stop = structural_span.map_or(CompileStop::Retry, CompileStop::Structural);
            break;
        }
        // Quake's 586 renderer benefits from native word operations. Doom's 486 self-patching
        // renderer recompiles the wider blocks often enough to lose throughput, so keep word
        // instructions as precise interpreter barriers in that mode.
        // Follow-up (dev_docs/specs/2026-07-15-smc-hardening-design.md, G1): with heat demotion
        // landed, A/B re-enabling 486 word ops - heat should now bound the churn this defends.
        if insn.operand_size == OperandSize::Word && cpu.persona() != CpuPersona::I586 {
            stop = structural_span.map_or(CompileStop::Retry, CompileStop::Structural);
            break;
        }
        let Some(kind) = classify::classify(&insn, lin, entry_lin) else {
            stop = structural_span.map_or(CompileStop::Retry, CompileStop::Structural);
            break;
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
        let kind = match (kind, cpu.stack_is_32bit(), insn.operand_size) {
            (kind, _, _) if !kind.uses_stack() => kind,
            (kind, true, OperandSize::Dword) => kind,
            (DirectKind::Push { source }, false, OperandSize::Word) => {
                DirectKind::Push16 { source }
            }
            (DirectKind::Pop { dst }, false, OperandSize::Word) => DirectKind::Pop16 { dst },
            (DirectKind::Ret { release }, false, OperandSize::Word) => {
                DirectKind::Ret16 { release }
            }
            (
                DirectKind::Call {
                    return_delta,
                    target_delta,
                },
                false,
                OperandSize::Word,
            ) => DirectKind::Call16 {
                return_delta,
                target_delta,
            },
            _ => {
                stop = CompileStop::Retry;
                break;
            }
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
        if kind.is_x87()
            && (x87_slots == MAX_X87_SLOTS || slots.len() >= MAX_X87_BLOCK_INSTRUCTIONS)
        {
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
        if let Some(segment) = kind.read_segment() {
            read_segments |= segment_bit(segment);
        }
        if let Some(segment) = kind.write_segment() {
            write_segments |= segment_bit(segment);
        }
        has_wide_accesses |=
            kind.has_word_access() || kind.has_dword_read() || kind.has_dword_store();
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
    let Some(segment_layout) = SegmentLayout::capture(cpu, read_segments, write_segments) else {
        return CompileOutcome::Retry;
    };
    let self_loop = matches!(
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
    let fallthrough = LinkTarget {
        linear: entry_lin.wrapping_add(u32::from(span.guest_len)),
        mode_key: key.mode_key,
    };
    let dynamic_successor = matches!(
        slots.last().map(|slot| slot.kind),
        Some(DirectKind::Ret { .. } | DirectKind::Ret16 { .. })
    );
    let successors = match slots.last().map(|slot| slot.kind) {
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
        Some(DirectKind::Ret { .. } | DirectKind::Ret16 { .. }) => [None, None],
        _ => [Some(fallthrough), None],
    };
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
            segments: segment_layout,
            address_wrap: if d {
                emit::AddressWrap::None
            } else {
                emit::AddressWrap::Word
            },
        },
        link_cell_ptrs: link_cells.each_ref().map(|cell| cell.address()),
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
        x87_entry_top,
        x87_exit_top,
        dynamic_successor,
        successors,
        link_cells,
        body_offset: emitted.body_offset,
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

struct EmitInput<'a> {
    slots: &'a [DirectInsn],
    span: BlockSpan,
    raw_clocks: u32,
    weighted_fp_clocks: u32,
    byte_reads: u8,
    word_reads: u8,
    dword_reads: u8,
    byte_stores: u8,
    word_stores: u8,
    dword_stores: u8,
    self_loop: bool,
    x87_entry_top: Option<u8>,
    memory: MemoryEmitContext,
    link_cell_ptrs: [usize; 2],
}

struct EmittedCode {
    code: Vec<u8>,
    body_offset: usize,
}

#[derive(Clone, Copy)]
struct MemoryEmitContext {
    map: Option<NativeMapBases>,
    code_watch_tables: Option<[usize; 2]>,
    cpl3: bool,
    segments: SegmentLayout,
    /// Whether a ModRM-derived effective address wraps at 64K.
    ///
    /// A BLOCK property, not an address one. `decode` computes
    /// `address_size = cs.default_size_32 XOR address_size_override`, and `prefixes_supported_for`
    /// refuses the override outright, so within an admitted block the address size is a pure
    /// function of CS.D, which the mode key pins.
    ///
    /// It lives here rather than on `DirectAddr` because that struct rides inside `Load`,
    /// `Store`, `AluMemSource` and other kinds in clif's lowerable set, which would lower them
    /// without the mask.
    ///
    /// **It does NOT govern stack addresses.** Those follow SS.B, which is independent of CS.D
    /// and is keyed separately, so all nine `stack_addr` call sites pass a literal.
    address_wrap: emit::AddressWrap,
}

#[cfg(test)]
#[path = "direct_test.rs"]
mod tests;
