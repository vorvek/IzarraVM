// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Admission and storage for page-local direct-code blocks.

mod classify;
mod emit;

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicUsize, Ordering},
    },
};

use izarravm_core::CpuPersona;

use super::code_watch::NativeCodeWatch;
#[cfg(target_os = "windows")]
use super::encoder::Xmm;
use super::encoder::{Encoder, Label, Reg};
use super::exec_mem::ExecutableArena;
use super::native_x87::{NativeX87Insn, NativeX87MemoryDirection};
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
const MAX_X87_BLOCK_INSTRUCTIONS: usize = 12;
const MAX_X87_SLOTS: u8 = 8;
const MAX_MEMORY_ALU_BLOCK_INSTRUCTIONS: usize = 4;
const MAX_MEMORY_ALU_SLOTS: u8 = 3;
pub(crate) const MAX_X87_BLOCK_CORE_CLOCKS: u64 = 3_928;
const DEFAULT_ENTRY_CAP: usize = 131_072;
const BLOCK_PAGE_SHIFT: u32 = 12;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct LinkTarget {
    linear: u32,
    mode_key: u32,
}

#[repr(C)]
struct LinkCell {
    body: AtomicUsize,
    target_eip: AtomicU32,
}

impl LinkCell {
    fn new() -> Self {
        Self {
            body: AtomicUsize::new(0),
            target_eip: AtomicU32::new(0),
        }
    }

    fn address(&self) -> usize {
        std::ptr::from_ref(&self.body) as usize
    }

    fn clear(&self) {
        self.body.store(0, Ordering::Release);
    }

    fn set(&self, body: usize) {
        self.body.store(body, Ordering::Release);
    }

    fn set_dynamic(&self, target_eip: u32, body: usize) {
        self.target_eip.store(target_eip, Ordering::Relaxed);
        self.body.store(body, Ordering::Release);
    }

    fn linked(&self) -> bool {
        self.body.load(Ordering::Acquire) != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LinkSource {
    block: BlockId,
    slot: u8,
}

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
struct SegmentLayout {
    cs: SegmentRegister,
    data: [SegmentRegister; 6],
    used: u8,
}

impl SegmentLayout {
    fn capture(cpu: &CpuGsw, read_segments: u8, write_segments: u8) -> Option<Self> {
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

    fn cs_matches(self, cpu: &CpuGsw) -> bool {
        self.cs == cpu.registers.cs()
    }

    fn data_matches(self, cpu: &CpuGsw) -> bool {
        SEGMENT_ORDER.into_iter().all(|segment| {
            self.used & segment_bit(segment) == 0
                || self.data[segment_index(segment)] == cpu.registers.segment(segment)
        })
    }

    fn all_data_matches(self, cpu: &CpuGsw) -> bool {
        SEGMENT_ORDER
            .into_iter()
            .all(|segment| self.data[segment_index(segment)] == cpu.registers.segment(segment))
    }

    fn link_compatible(self, target: Self) -> bool {
        self.cs == target.cs && self.data == target.data
    }

    fn descriptor(self, segment: SegmentIndex) -> SegmentRegister {
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
    decode_residency_epoch: u64,
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

    pub(crate) fn decode_residency_epoch(&self) -> u64 {
        self.decode_residency_epoch
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

    fn link_compatible(self, target: Self) -> bool {
        self.span.key.mode_key == target.span.key.mode_key
            && self.memory_cpl3 == target.memory_cpl3
            && self.has_x87 == target.has_x87
            && (!self.has_x87 || self.x87_exit_top == target.x87_entry_top)
            && self.segment_layout.link_compatible(target.segment_layout)
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
    link_cells: Vec<[Arc<LinkCell>; 2]>,
    link_sources: HashMap<usize, LinkSource>,
    outbound: Vec<[Option<BlockId>; 2]>,
    dynamic_next_slots: Vec<u8>,
    inbound: HashMap<BlockId, Vec<LinkSource>>,
    waiting: HashMap<LinkTarget, Vec<LinkSource>>,
    linear_blocks: HashMap<LinkTarget, BlockId>,
    block_link_epochs: Vec<u64>,
    link_epoch: u64,
    block_active: Vec<bool>,
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
    stats: BlockCacheStats,
    code_watch: Box<NativeCodeWatch>,
    #[cfg(test)]
    defer_short_for_test: bool,
}

impl Default for BlockCache {
    fn default() -> Self {
        // Executable arena pressure normally resets compiled code first. Keep a separate, much
        // larger bound for seen and rejected keys so unsupported one-shot code cannot grow the
        // metadata maps without limit during a long-running guest.
        Self::with_entry_cap(DEFAULT_ENTRY_CAP)
    }
}

impl BlockCache {
    fn with_entry_cap(entry_cap: usize) -> Self {
        Self {
            entries: HashMap::new(),
            physical_keys: HashMap::default(),
            blocks: Vec::new(),
            link_cells: Vec::new(),
            link_sources: HashMap::new(),
            outbound: Vec::new(),
            dynamic_next_slots: Vec::new(),
            inbound: HashMap::new(),
            waiting: HashMap::new(),
            linear_blocks: HashMap::new(),
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
            stats: BlockCacheStats::default(),
            code_watch: Box::default(),
            #[cfg(test)]
            defer_short_for_test: false,
        }
    }

    pub(crate) fn auto_admit(&self) -> bool {
        self.auto_admit
    }

    pub(crate) fn backend_enabled(&self) -> bool {
        self.backend_enabled
    }

    pub(crate) fn set_backend_enabled(&mut self, on: bool) {
        self.backend_enabled = on && super::host_supported();
    }

    pub(crate) fn set_auto_admit(&mut self, on: bool) {
        self.auto_admit = on;
    }

    pub(crate) fn probe(&mut self, key: BlockKey) -> BlockProbe {
        if self.disabled {
            return BlockProbe::Rejected;
        }
        let hot_index = key.hot_index();
        if let Some(hit) = self.hot[hot_index] {
            if hit.generation == self.hot_generation && hit.key == key {
                self.stats.hot_hits += 1;
                return BlockProbe::Ready(hit.id);
            }
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
                    self.reset_storage();
                }
                self.entries.insert(key, BlockState::Seen);
                self.track_physical_key(key);
                BlockProbe::Interpret
            }
        }
    }

    /// Install bytes produced after `probe` returned `Compile`.
    pub(crate) fn install(&mut self, compilation: &Compilation) -> Option<BlockId> {
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
                self.reset_storage();
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
            decode_residency_epoch: compilation.decode_residency_epoch,
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
        self.code_watch
            .acquire_range(span.key.physical, u32::from(span.guest_len));
        if index == self.blocks.len() {
            self.blocks.push(block);
            self.link_cells.push(compilation.link_cells.clone());
            self.outbound.push([None, None]);
            self.dynamic_next_slots.push(0);
            self.block_link_epochs.push(0);
            self.block_active.push(true);
        } else {
            debug_assert!(!self.block_active[index]);
            self.blocks[index] = block;
            self.link_cells[index] = compilation.link_cells.clone();
            self.outbound[index] = [None, None];
            self.dynamic_next_slots[index] = 0;
            self.block_link_epochs[index] = 0;
            self.block_active[index] = true;
        }
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
    pub(crate) fn reject(&mut self, span: RejectedSpan) {
        if self.entries.get(&span.key) == Some(&BlockState::Seen) {
            self.code_watch
                .acquire_range(span.key.physical, u32::from(span.guest_len));
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

    /// Retire one descriptor-specialized block while keeping its key in the observed state. The
    /// current encounter falls back to the interpreter; the next encounter recompiles directly
    /// for the then-current segment layout instead of paying another first-seen pass.
    pub(crate) fn retire_key_for_recompile(&mut self, key: BlockKey) -> bool {
        let Some(BlockState::Compiled(id)) = self.entries.get(&key).copied() else {
            return false;
        };
        let hot_index = key.hot_index();
        if self.hot[hot_index].is_some_and(|hot| hot.key == key) {
            self.hot[hot_index] = None;
        }
        self.entries.insert(key, BlockState::Seen);
        self.retire_block(id);
        true
    }

    pub(crate) fn clear(&mut self) {
        // CS reloads and monitor transitions can invalidate code millions of times while the
        // direct cache is unused. Avoid clearing the 65,536-entry hot table when it is already
        // empty.
        if self.entries.is_empty() && self.blocks.is_empty() && self.arena.is_none() {
            if self.code_watch.has_resident_pages() {
                self.code_watch.clear();
            }
            self.disabled = false;
            return;
        }
        self.reset_storage();
        self.disabled = false;
    }

    /// Drop translation-dependent links while retaining physical compiled code. A block is not
    /// eligible as a successor again until its decode lines have been checked in the new decode
    /// residency epoch.
    pub(crate) fn invalidate_translation(&mut self) {
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

    /// Remove direct-cache entries whose translated physical bytes overlap a guest write. Block
    /// IDs and executable pages stay in place until the arena's normal whole-cache reset.
    pub(crate) fn invalidate_physical_range(&mut self, physical: u32, width: u32) -> usize {
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
                        BlockState::Rejected(span) => self
                            .code_watch
                            .release_range(span.key.physical, u32::from(span.guest_len)),
                        BlockState::Compiled(id) => self.retire_block(id),
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

    pub(crate) fn native_code_watch_table(&mut self) -> usize {
        self.code_watch.table_base()
    }

    #[cfg(test)]
    pub(crate) fn mark_code_range(&mut self, physical: u32, len: u8) {
        self.code_watch.acquire_range(physical, u32::from(len));
    }

    pub(crate) fn range_hits_compiled_code(&self, physical: u32, width: u32) -> bool {
        self.code_watch.range_watched(physical, width)
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

    /// Record that every decode slot in `key`'s block was revalidated against `epoch`.
    pub(crate) fn refresh_decode_residency(
        &mut self,
        key: BlockKey,
        epoch: u64,
    ) -> Option<CompiledBlock> {
        let BlockState::Compiled(id) = self.entries.get(&key).copied()? else {
            return None;
        };
        let index = self.active_index(id)?;
        let block = self.blocks.get_mut(index)?;
        block.decode_residency_epoch = epoch;
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

        for cells in &self.link_cells {
            cells[0].clear();
            cells[1].clear();
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
        for (source_index, targets) in self.outbound.iter().enumerate() {
            if !self.block_active[source_index] {
                continue;
            }
            for (slot, target) in targets.iter().copied().enumerate() {
                if let Some(target) = target {
                    let target_index = self
                        .active_index(target)
                        .expect("outbound target was validated before relocation");
                    self.link_cells[source_index][slot].set(self.blocks[target_index].body_ptr());
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

    fn reset_storage(&mut self) {
        let links = self
            .outbound
            .iter()
            .flatten()
            .filter(|target| target.is_some())
            .count() as u64;
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
        self.block_link_epochs.clear();
        self.code_watch.clear();
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
        if self.block_link_epochs.get(source_index).copied() != Some(self.link_epoch)
            || self.block_link_epochs.get(target_index).copied() != Some(self.link_epoch)
            || !self.blocks[source_index].link_compatible(self.blocks[target_index])
        {
            return false;
        }
        let slot_index = usize::from(slot);
        if self.outbound[source_index][slot_index] == Some(target) {
            if let Some(target_eip) = target_eip {
                self.link_cells[source_index][slot_index]
                    .set_dynamic(target_eip, self.blocks[target_index].body_ptr());
            }
            return true;
        }
        self.unlink_outbound(source, slot);
        if let Some(target_eip) = target_eip {
            self.link_cells[source_index][slot_index]
                .set_dynamic(target_eip, self.blocks[target_index].body_ptr());
        } else {
            self.link_cells[source_index][slot_index].set(self.blocks[target_index].body_ptr());
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

    fn retire_block(&mut self, id: BlockId) {
        let Some(index) = self.active_index(id) else {
            return;
        };
        let span = self.blocks[index].span;
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
        self.code_watch
            .release_range(span.key.physical, u32::from(span.guest_len));
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
            return;
        }
        self.block_link_epochs[index] = self.link_epoch;
        let span = self.blocks[index].span;
        let target = LinkTarget {
            linear: span.key.linear,
            mode_key: span.key.mode_key,
        };
        self.linear_blocks.insert(target, id);
        self.resolve_successors(id);
        self.resolve_waiting(target, id);
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

impl Clone for BlockCache {
    fn clone(&self) -> Self {
        Self {
            backend_enabled: self.backend_enabled,
            ..Self::default()
        }
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
    pub(crate) unresolved_exits: u32,
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
    pub decode_residency_epoch: u64,
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
    dynamic_successor: bool,
    successors: [Option<LinkTarget>; 2],
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
enum DirectKind {
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
    Pop {
        dst: u8,
    },
    Call {
        return_delta: u32,
        target_delta: u32,
    },
    Jmp {
        target_delta: u32,
    },
    Ret {
        release: u16,
    },
    Jcc {
        condition: u8,
        taken_delta: u32,
    },
    X87 {
        insn: NativeX87Insn,
        addr: Option<DirectAddr>,
    },
}

#[derive(Clone, Copy)]
enum MemoryWidth {
    Byte,
    Word,
    Dword,
}

impl MemoryWidth {
    const fn bytes(self) -> u32 {
        match self {
            Self::Byte => 1,
            Self::Word => 2,
            Self::Dword => 4,
        }
    }

    const fn needs_alignment_guard(self) -> bool {
        !matches!(self, Self::Byte)
    }
}

#[derive(Clone, Copy)]
enum StoreSource {
    Reg(u8),
    Imm(u32),
    EipDelta(u32),
}

#[derive(Clone, Copy)]
enum ShiftCount {
    Immediate(u8),
    Cl,
}

#[derive(Clone, Copy)]
struct DirectAddr {
    segment: SegmentIndex,
    base: Option<u8>,
    index: Option<u8>,
    scale: u8,
    disp: u32,
}

impl DirectKind {
    fn raw_clocks(self) -> u32 {
        match self {
            Self::Jcc { .. } => 3,
            Self::Pop { .. } => 4,
            Self::Call { .. } | Self::Jmp { .. } => 7,
            Self::Ret { .. } => 10,
            Self::DoubleShiftReg { .. } | Self::DoubleShiftMem { .. } => 3,
            Self::Load { raw_clocks, .. } | Self::Store { raw_clocks, .. } => u32::from(raw_clocks),
            Self::X87 { .. } => 0,
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

    fn byte_reads(self) -> u8 {
        u8::from(matches!(
            self,
            Self::Load {
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

    fn word_reads(self) -> u8 {
        u8::from(matches!(
            self,
            Self::Load {
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
            } | Self::TestImmMem {
                width: MemoryWidth::Word,
                ..
            }
        ))
    }

    fn dword_reads(self) -> u8 {
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
                    | Self::TestImmMem {
                        width: MemoryWidth::Dword,
                        ..
                    }
                    | Self::Pop { .. }
                    | Self::Ret { .. }
            ) || matches!(
                self,
                Self::X87 { insn, .. }
                    if matches!(
                        insn.metadata().memory,
                        Some(access) if access.direction == NativeX87MemoryDirection::Read
                    )
            ),
        )
    }

    fn byte_stores(self) -> u8 {
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

    fn word_stores(self) -> u8 {
        u8::from(
            matches!(
                self,
                Self::Store {
                    width: MemoryWidth::Word,
                    ..
                } | Self::RmwIncDec {
                    width: MemoryWidth::Word,
                    ..
                }
            ) || matches!(
                self,
                Self::AluMemDest {
                    op: 0..=6,
                    width: MemoryWidth::Word,
                    ..
                }
            ),
        )
    }

    fn dword_stores(self) -> u8 {
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
            ) || matches!(
                self,
                Self::AluMemDest {
                    op: 0..=6,
                    width: MemoryWidth::Dword,
                    ..
                }
            ) || matches!(
                self,
                Self::X87 { insn, .. }
                    if matches!(
                        insn.metadata().memory,
                        Some(access) if access.direction == NativeX87MemoryDirection::Write
                    )
            ),
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
            Self::X87 { insn, .. } => match insn.metadata().memory.map(|access| access.direction) {
                Some(NativeX87MemoryDirection::Read) => COUNTER_MODE13_DWORD_READ,
                Some(NativeX87MemoryDirection::Write) => {
                    COUNTER_RAM_DWORD_WRITE | COUNTER_MODE13_DWORD_WRITE | COUNTER_MODE13_DIRTY
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
            Self::Pop { .. } | Self::Ret { .. } => COUNTER_MODE13_DWORD_READ,
            Self::Push { .. } | Self::Call { .. } => {
                COUNTER_RAM_DWORD_WRITE | COUNTER_MODE13_DWORD_WRITE | COUNTER_MODE13_DIRTY
            }
            _ => 0,
        }
    }

    fn read_segment(self) -> Option<SegmentIndex> {
        match self {
            Self::Load { addr, .. }
            | Self::AluMemSource { addr, .. }
            | Self::AluMemDest { addr, .. }
            | Self::DoubleShiftMem { addr, .. }
            | Self::TestImmMem { addr, .. }
            | Self::RmwIncDec { addr, .. } => Some(addr.segment),
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
            Self::Pop { .. } | Self::Ret { .. } => Some(SegmentIndex::Ss),
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
            Self::Push { .. } | Self::Call { .. } => Some(SegmentIndex::Ss),
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
                | Self::TestImmMem {
                    width: MemoryWidth::Dword,
                    ..
                }
                | Self::Pop { .. }
                | Self::Ret { .. }
        ) || matches!(
            self,
            Self::X87 { insn, .. }
                if matches!(
                    insn.metadata().memory,
                    Some(access) if access.direction == NativeX87MemoryDirection::Read
                )
        )
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
        ) || matches!(
            self,
            Self::AluMemDest {
                op: 0..=6,
                width: MemoryWidth::Dword,
                ..
            }
        ) || matches!(
            self,
            Self::X87 { insn, .. }
                if matches!(
                    insn.metadata().memory,
                    Some(access) if access.direction == NativeX87MemoryDirection::Write
                )
        )
    }

    fn has_word_access(self) -> bool {
        self.word_reads() != 0 || self.word_stores() != 0
    }

    fn uses_stack(self) -> bool {
        matches!(
            self,
            Self::Push { .. } | Self::Pop { .. } | Self::Call { .. } | Self::Ret { .. }
        )
    }

    fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Call { .. } | Self::Jmp { .. } | Self::Ret { .. } | Self::Jcc { .. }
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

const NATIVE_STACK_LEN: u32 = 160;
#[cfg(target_os = "windows")]
const AVX2_X87_STACK_LEN: u32 = NATIVE_STACK_LEN + 8 + 6 * 16;
#[cfg(not(target_os = "windows"))]
const AVX2_X87_STACK_LEN: u32 = NATIVE_STACK_LEN;
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
const STACK_ALU_FLAGS: i32 = 144;
const STACK_SHIFT_COUNT: i32 = 152;
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

pub(crate) fn key_for(cpu: &CpuGsw, lin: u32, d: bool) -> Option<BlockKey> {
    if !super::host_supported()
        || !d
        || !matches!(cpu.persona(), CpuPersona::I486 | CpuPersona::I586)
    {
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
        let word_prefixes = Prefixes {
            operand_size_override: true,
            ..Prefixes::default()
        };
        let prefixes_supported = match insn.operand_size {
            OperandSize::Word => insn.prefixes == word_prefixes,
            OperandSize::Dword => insn.prefixes == Prefixes::default(),
        };
        if !prefixes_supported || !insn.continuable {
            stop = structural_span.map_or(CompileStop::Retry, CompileStop::Structural);
            break;
        }
        // Quake's 586 renderer benefits from native word operations. Doom's 486 self-patching
        // renderer recompiles the wider blocks often enough to lose throughput, so keep word
        // instructions as precise interpreter barriers in that mode.
        if insn.operand_size == OperandSize::Word && cpu.persona() != CpuPersona::I586 {
            stop = structural_span.map_or(CompileStop::Retry, CompileStop::Structural);
            break;
        }
        let Some(kind) = classify::classify(&insn, lin, entry_lin) else {
            stop = structural_span.map_or(CompileStop::Retry, CompileStop::Structural);
            break;
        };
        if !static_control_target_within_limit(kind, entry_eip, cs.limit)
            || !kind_segment_access_supported(cpu, kind)
        {
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
        if kind.uses_stack() && !cpu.stack_is_32bit() {
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
        Some(DirectKind::Ret { .. })
    );
    let successors = match slots.last().map(|slot| slot.kind) {
        Some(DirectKind::Jcc { taken_delta, .. }) => [
            Some(fallthrough),
            (!self_loop).then_some(LinkTarget {
                linear: entry_lin.wrapping_add(taken_delta),
                mode_key: key.mode_key,
            }),
        ],
        Some(DirectKind::Call { target_delta, .. } | DirectKind::Jmp { target_delta }) => [
            Some(LinkTarget {
                linear: entry_lin.wrapping_add(target_delta),
                mode_key: key.mode_key,
            }),
            None,
        ],
        Some(DirectKind::Ret { .. }) => [None, None],
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
        },
        link_cell_ptrs: link_cells.each_ref().map(|cell| cell.address()),
    });
    CompileOutcome::Compiled(Compilation {
        span,
        decode_residency_epoch: cpu.decode_cache.residency_epoch(),
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

fn static_control_target_within_limit(kind: DirectKind, entry_eip: u32, limit: u32) -> bool {
    let target_delta = match kind {
        DirectKind::Call { target_delta, .. } | DirectKind::Jmp { target_delta } => {
            Some(target_delta)
        }
        DirectKind::Jcc { taken_delta, .. } => Some(taken_delta),
        _ => None,
    };
    target_delta.is_none_or(|delta| entry_eip.wrapping_add(delta) <= limit)
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
}

#[cfg(test)]
#[path = "direct_test.rs"]
mod tests;
