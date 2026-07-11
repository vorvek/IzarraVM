// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Admission and storage for page-local direct-code blocks.

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
    Rejected,
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
            Some(BlockState::Rejected) => BlockProbe::Rejected,
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
        self.code_watch
            .mark_refcounted_range(span.key.physical, u32::from(span.guest_len));
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
    pub(crate) fn reject(&mut self, key: BlockKey) {
        if self.entries.get(&key) == Some(&BlockState::Seen) {
            self.entries.insert(key, BlockState::Rejected);
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
        self.retire_block(id);
        self.entries.insert(key, BlockState::Seen);
        true
    }

    pub(crate) fn clear(&mut self) {
        // CS reloads and monitor transitions can invalidate code millions of times while the
        // direct cache is unused. Avoid clearing the 65,536-entry hot table when it is already
        // empty.
        if self.entries.is_empty() && self.blocks.is_empty() && self.arena.is_none() {
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
                        BlockState::Seen | BlockState::Rejected => {
                            physical_range_contains(physical, width, key.physical)
                        }
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
                    if let BlockState::Compiled(id) = state {
                        self.retire_block(id);
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
        self.code_watch.mark(physical, len);
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
        self.code_watch
            .unmark_refcounted_range(span.key.physical, u32::from(span.guest_len));
        self.unlink_block(id);
        self.block_active[index] = false;
        self.blocks[index].entry = 0;
        self.blocks[index].body_entry = 0;
        self.free_block_slots
            .push(u16::try_from(index).expect("block slot index must fit its ID"));
        self.live_blocks -= 1;
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
        Self::default()
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

pub(crate) fn compile(cpu: &mut CpuGsw, entry_lin: u32, d: bool) -> Option<Compilation> {
    let key = key_for(cpu, entry_lin, d)?;
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

    while slots.len() < MAX_BLOCK_INSTRUCTIONS {
        if x87_slots != 0 && slots.len() == MAX_X87_BLOCK_INSTRUCTIONS {
            break;
        }
        if memory_alu_slots != 0 && slots.len() == MAX_MEMORY_ALU_BLOCK_INSTRUCTIONS {
            break;
        }
        let Some(insn) = cpu.decode_cache.get(lin, d) else {
            break;
        };
        let word_prefixes = Prefixes {
            operand_size_override: true,
            ..Prefixes::default()
        };
        let prefixes_supported = match insn.operand_size {
            OperandSize::Word => insn.prefixes == word_prefixes,
            OperandSize::Dword => insn.prefixes == Prefixes::default(),
        };
        if !prefixes_supported || !insn.continuable {
            break;
        }
        let Some(next) = lin.checked_add(u32::from(insn.len)) else {
            break;
        };
        let slot_eip = lin.wrapping_sub(cs.base);
        if slot_eip
            .checked_add(u32::from(insn.len) - 1)
            .is_none_or(|last| last > cs.limit)
        {
            break;
        }
        if entry_lin >> 12 != next.wrapping_sub(1) >> 12 {
            break;
        }
        let Some(expected_phys) = key.physical.checked_add(lin.wrapping_sub(entry_lin)) else {
            break;
        };
        if cpu.decode_cache.line_phys_start(lin, d) != Some(expected_phys) {
            break;
        }
        // Quake's 586 renderer benefits from native word operations. Doom's 486 self-patching
        // renderer recompiles the wider blocks often enough to lose throughput, so keep word
        // instructions as precise interpreter barriers in that mode.
        if insn.operand_size == OperandSize::Word && cpu.persona() != CpuPersona::I586 {
            break;
        }
        let Some(kind) = classify(&insn, lin, entry_lin) else {
            break;
        };
        if !static_control_target_within_limit(kind, entry_eip, cs.limit)
            || !kind_segment_access_supported(cpu, kind)
        {
            break;
        }
        if kind.is_x87()
            && (x87_slots == MAX_X87_SLOTS || slots.len() >= MAX_X87_BLOCK_INSTRUCTIONS)
        {
            break;
        }
        if kind.is_memory_alu()
            && (memory_alu_slots == MAX_MEMORY_ALU_SLOTS
                || slots.len() >= MAX_MEMORY_ALU_BLOCK_INSTRUCTIONS)
        {
            break;
        }
        if kind.uses_stack() && !cpu.stack_is_32bit() {
            break;
        }
        if kind.uses_stack() && stack_accesses == MAX_BLOCK_STACK_ACCESSES {
            break;
        }
        stack_accesses += u8::from(kind.uses_stack());
        x87_slots += u8::from(kind.is_x87());
        if let DirectKind::X87 { insn, .. } = kind {
            x87_exit_top = insn.advance_top(x87_exit_top);
        }
        memory_alu_slots += u8::from(kind.is_memory_alu());
        raw_clocks += kind.raw_clocks();
        let slot_weighted_fp_clocks = kind.weighted_fp_clocks(cpu.persona());
        weighted_fp_clocks = weighted_fp_clocks.checked_add(slot_weighted_fp_clocks)?;
        byte_reads = byte_reads.checked_add(kind.byte_reads())?;
        word_reads = word_reads.checked_add(kind.word_reads())?;
        dword_reads = dword_reads.checked_add(kind.dword_reads())?;
        byte_stores = byte_stores.checked_add(kind.byte_stores())?;
        word_stores = word_stores.checked_add(kind.word_stores())?;
        dword_stores = dword_stores.checked_add(kind.dword_stores())?;
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
        return None;
    }
    let last = slots.last()?;
    let guest_len = last
        .lin
        .wrapping_add(u32::from(last.len))
        .wrapping_sub(entry_lin) as usize;
    let span = BlockSpan::new(key, guest_len, slots.len())?;
    let segment_layout = SegmentLayout::capture(cpu, read_segments, write_segments)?;
    let self_loop = matches!(
        slots.last().map(|slot| slot.kind),
        Some(DirectKind::Jcc { taken_delta: 0, .. })
    );
    if self_loop && x87_slots != 0 && x87_entry_top != x87_exit_top {
        return None;
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
        Some(cpu.jit_fast_map.native_bases()?)
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
        return None;
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
        return None;
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
    let emitted = emit(EmitInput {
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
    if emitted.code.len() > super::exec_mem::host_page_len() {
        return None;
    }
    Some(Compilation {
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

fn classify(insn: &DecodedInsn, lin: u32, entry_lin: u32) -> Option<DirectKind> {
    if insn.group == DecodeGroup::Fpu {
        if insn.operand_size != OperandSize::Dword {
            return None;
        }
        let native = NativeX87Insn::classify(insn)?;
        let addr = match native {
            NativeX87Insn::BinaryMemory { addr, .. }
            | NativeX87Insn::LoadF32 { addr }
            | NativeX87Insn::StoreF32 { addr, .. }
            | NativeX87Insn::LoadI32 { addr }
            | NativeX87Insn::StoreI32 { addr } => Some(direct_addr(addr)?),
            _ => None,
        };
        return Some(DirectKind::X87 { insn: native, addr });
    }
    let operand_width = match insn.operand_size {
        OperandSize::Word => MemoryWidth::Word,
        OperandSize::Dword => MemoryWidth::Dword,
    };
    if insn.operand_size == OperandSize::Word
        && !matches!(insn.opcode, 0x39 | 0x3b | 0x40..=0x4f | 0x89 | 0x8b | 0xff)
    {
        return None;
    }
    if matches!(insn.opcode, 0x0fa4 | 0x0fa5 | 0x0fac | 0x0fad) {
        let m = insn.modrm?;
        let count = if matches!(insn.opcode, 0x0fa4 | 0x0fac) {
            ShiftCount::Immediate(insn.imm as u8)
        } else {
            ShiftCount::Cl
        };
        let left = matches!(insn.opcode, 0x0fa4 | 0x0fa5);
        return match insn.operand? {
            DecodedOperand::Reg(dst) => Some(DirectKind::DoubleShiftReg {
                left,
                dst,
                src: m.reg,
                count,
            }),
            DecodedOperand::Mem(addr) => Some(DirectKind::DoubleShiftMem {
                left,
                src: m.reg,
                count,
                addr: direct_addr(addr)?,
            }),
        };
    }
    let opcode = u8::try_from(insn.opcode).ok();
    if let Some(opcode) = opcode {
        if opcode < 0x40 {
            let op = (opcode >> 3) & 7;
            let form = opcode & 7;
            match form {
                1 => {
                    let m = insn.modrm?;
                    return match insn.operand? {
                        DecodedOperand::Reg(dst) => Some(DirectKind::AluReg {
                            op,
                            dst,
                            src: m.reg,
                            width: operand_width,
                        }),
                        DecodedOperand::Mem(addr) => Some(DirectKind::AluMemDest {
                            op,
                            source: StoreSource::Reg(m.reg),
                            width: operand_width,
                            addr: direct_addr(addr)?,
                        }),
                    };
                }
                3 => {
                    let m = insn.modrm?;
                    return match insn.operand? {
                        DecodedOperand::Reg(src) => Some(DirectKind::AluReg {
                            op,
                            dst: m.reg,
                            src,
                            width: operand_width,
                        }),
                        DecodedOperand::Mem(addr) => Some(DirectKind::AluMemSource {
                            op,
                            dst: m.reg,
                            width: operand_width,
                            addr: direct_addr(addr)?,
                        }),
                    };
                }
                5 => {
                    return Some(DirectKind::AluImm {
                        op,
                        dst: 0,
                        imm: insn.imm,
                    });
                }
                _ => {}
            }
        }
        match opcode {
            0x40..=0x4f => {
                return Some(DirectKind::IncDecReg {
                    dst: opcode & 7,
                    is_dec: opcode >= 0x48,
                    width: operand_width,
                });
            }
            0x50..=0x57 => {
                return Some(DirectKind::Push {
                    source: StoreSource::Reg(opcode - 0x50),
                });
            }
            0x58..=0x5f => {
                return Some(DirectKind::Pop { dst: opcode - 0x58 });
            }
            0x68 => {
                return Some(DirectKind::Push {
                    source: StoreSource::Imm(insn.imm),
                });
            }
            0x6a => {
                return Some(DirectKind::Push {
                    source: StoreSource::Imm(crate::sign_extend_u8(insn.imm as u8)),
                });
            }
            0x80 => {
                let m = insn.modrm?;
                return match insn.operand? {
                    DecodedOperand::Reg(dst) => Some(DirectKind::AluByteImm {
                        op: m.reg,
                        dst,
                        imm: insn.imm as u8,
                    }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::AluMemDest {
                        op: m.reg,
                        source: StoreSource::Imm(insn.imm),
                        width: MemoryWidth::Byte,
                        addr: direct_addr(addr)?,
                    }),
                };
            }
            0x81 | 0x83 => {
                let m = insn.modrm?;
                return match insn.operand? {
                    DecodedOperand::Reg(dst) => Some(DirectKind::AluImm {
                        op: m.reg,
                        dst,
                        imm: insn.imm,
                    }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::AluMemDest {
                        op: m.reg,
                        source: StoreSource::Imm(insn.imm),
                        width: MemoryWidth::Dword,
                        addr: direct_addr(addr)?,
                    }),
                };
            }
            0x85 => {
                let m = insn.modrm?;
                let DecodedOperand::Reg(a) = insn.operand? else {
                    return None;
                };
                return Some(DirectKind::Test { a, b: m.reg });
            }
            0xa8 => {
                return Some(DirectKind::TestImmReg {
                    dst: 0,
                    imm: insn.imm,
                    width: MemoryWidth::Byte,
                });
            }
            0xa9 => {
                return Some(DirectKind::TestImmReg {
                    dst: 0,
                    imm: insn.imm,
                    width: MemoryWidth::Dword,
                });
            }
            0x88 => {
                let m = insn.modrm?;
                return match insn.operand? {
                    DecodedOperand::Reg(dst) => Some(DirectKind::MovRegByte { dst, src: m.reg }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::Store {
                        source: StoreSource::Reg(m.reg),
                        width: MemoryWidth::Byte,
                        addr: direct_addr(addr)?,
                        raw_clocks: 2,
                    }),
                };
            }
            0x89 => {
                let m = insn.modrm?;
                return match insn.operand? {
                    DecodedOperand::Reg(dst) => Some(DirectKind::MovReg {
                        dst,
                        src: m.reg,
                        width: operand_width,
                    }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::Store {
                        source: StoreSource::Reg(m.reg),
                        width: operand_width,
                        addr: direct_addr(addr)?,
                        raw_clocks: 2,
                    }),
                };
            }
            0x8a => {
                let m = insn.modrm?;
                return match insn.operand? {
                    DecodedOperand::Reg(src) => Some(DirectKind::MovRegByte { dst: m.reg, src }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::Load {
                        dst: m.reg,
                        width: MemoryWidth::Byte,
                        addr: direct_addr(addr)?,
                        raw_clocks: 2,
                    }),
                };
            }
            0x8b => {
                let m = insn.modrm?;
                return match insn.operand? {
                    DecodedOperand::Reg(src) => Some(DirectKind::MovReg {
                        dst: m.reg,
                        src,
                        width: operand_width,
                    }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::Load {
                        dst: m.reg,
                        width: operand_width,
                        addr: direct_addr(addr)?,
                        raw_clocks: 2,
                    }),
                };
            }
            0x8d => {
                let m = insn.modrm?;
                let DecodedOperand::Mem(addr) = insn.operand? else {
                    return None;
                };
                return Some(DirectKind::Lea {
                    dst: m.reg,
                    addr: direct_addr(addr)?,
                });
            }
            0xa0 => {
                return Some(DirectKind::Load {
                    dst: 0,
                    width: MemoryWidth::Byte,
                    addr: DirectAddr {
                        segment: insn.prefixes.segment_override.unwrap_or(SegmentIndex::Ds),
                        base: None,
                        index: None,
                        scale: 1,
                        disp: insn.imm,
                    },
                    raw_clocks: 4,
                });
            }
            0xa1 => {
                return Some(DirectKind::Load {
                    dst: 0,
                    width: MemoryWidth::Dword,
                    addr: DirectAddr {
                        segment: insn.prefixes.segment_override.unwrap_or(SegmentIndex::Ds),
                        base: None,
                        index: None,
                        scale: 1,
                        disp: insn.imm,
                    },
                    raw_clocks: 4,
                });
            }
            0xa2 => {
                return Some(DirectKind::Store {
                    source: StoreSource::Reg(0),
                    width: MemoryWidth::Byte,
                    addr: DirectAddr {
                        segment: insn.prefixes.segment_override.unwrap_or(SegmentIndex::Ds),
                        base: None,
                        index: None,
                        scale: 1,
                        disp: insn.imm,
                    },
                    raw_clocks: 4,
                });
            }
            0xa3 => {
                return Some(DirectKind::Store {
                    source: StoreSource::Reg(0),
                    width: MemoryWidth::Dword,
                    addr: DirectAddr {
                        segment: insn.prefixes.segment_override.unwrap_or(SegmentIndex::Ds),
                        base: None,
                        index: None,
                        scale: 1,
                        disp: insn.imm,
                    },
                    raw_clocks: 4,
                });
            }
            0xb0..=0xb7 => {
                return Some(DirectKind::MovImmByte {
                    dst: opcode - 0xb0,
                    imm: insn.imm as u8,
                });
            }
            0xb8..=0xbf => {
                return Some(DirectKind::MovImm {
                    dst: opcode - 0xb8,
                    imm: insn.imm,
                });
            }
            0xc6 | 0xc7 => {
                let m = insn.modrm?;
                if m.reg != 0 {
                    return None;
                }
                let width = if opcode == 0xc6 {
                    MemoryWidth::Byte
                } else {
                    MemoryWidth::Dword
                };
                return match insn.operand? {
                    DecodedOperand::Reg(dst) if opcode == 0xc6 => Some(DirectKind::MovImmByte {
                        dst,
                        imm: insn.imm as u8,
                    }),
                    DecodedOperand::Reg(dst) => Some(DirectKind::MovImm { dst, imm: insn.imm }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::Store {
                        source: StoreSource::Imm(insn.imm),
                        width,
                        addr: direct_addr(addr)?,
                        raw_clocks: 2,
                    }),
                };
            }
            0xc1 | 0xd1 => {
                let m = insn.modrm?;
                if !matches!(m.reg, 4..=7) {
                    return None;
                }
                let DecodedOperand::Reg(dst) = insn.operand? else {
                    return None;
                };
                return Some(DirectKind::Shift {
                    op: m.reg,
                    dst,
                    count: if opcode == 0xd1 { 1 } else { insn.imm as u8 },
                });
            }
            0xf6 | 0xf7 => {
                let m = insn.modrm?;
                if m.reg != 0 {
                    return None;
                }
                let width = if opcode == 0xf6 {
                    MemoryWidth::Byte
                } else {
                    MemoryWidth::Dword
                };
                return match insn.operand? {
                    DecodedOperand::Reg(dst) => Some(DirectKind::TestImmReg {
                        dst,
                        imm: insn.imm,
                        width,
                    }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::TestImmMem {
                        imm: insn.imm,
                        width,
                        addr: direct_addr(addr)?,
                    }),
                };
            }
            0xc2 | 0xc3 => {
                if !matches!(operand_width, MemoryWidth::Dword) {
                    return None;
                }
                return Some(DirectKind::Ret {
                    release: if opcode == 0xc2 { insn.imm as u16 } else { 0 },
                });
            }
            0xff => {
                let m = insn.modrm?;
                if !matches!(m.reg, 0 | 1) {
                    return None;
                }
                return match insn.operand? {
                    DecodedOperand::Reg(dst) => Some(DirectKind::IncDecReg {
                        dst,
                        is_dec: m.reg == 1,
                        width: operand_width,
                    }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::RmwIncDec {
                        is_dec: m.reg == 1,
                        width: operand_width,
                        addr: direct_addr(addr)?,
                    }),
                };
            }
            0x70..=0x7f if insn.group == DecodeGroup::Branch => {
                let end_delta = lin
                    .wrapping_add(u32::from(insn.len))
                    .wrapping_sub(entry_lin);
                return Some(DirectKind::Jcc {
                    condition: opcode & 0x0f,
                    taken_delta: end_delta.wrapping_add(insn.imm),
                });
            }
            0xe8 if insn.group == DecodeGroup::Branch => {
                let return_delta = lin
                    .wrapping_add(u32::from(insn.len))
                    .wrapping_sub(entry_lin);
                return Some(DirectKind::Call {
                    return_delta,
                    target_delta: return_delta.wrapping_add(insn.imm),
                });
            }
            0xe9 | 0xeb if insn.group == DecodeGroup::Branch => {
                let end_delta = lin
                    .wrapping_add(u32::from(insn.len))
                    .wrapping_sub(entry_lin);
                return Some(DirectKind::Jmp {
                    target_delta: end_delta.wrapping_add(insn.imm),
                });
            }
            _ => {}
        }
    }
    if matches!(insn.opcode, 0x0f80..=0x0f8f) && insn.group == DecodeGroup::Branch {
        let end_delta = lin
            .wrapping_add(u32::from(insn.len))
            .wrapping_sub(entry_lin);
        return Some(DirectKind::Jcc {
            condition: (insn.opcode & 0x0f) as u8,
            taken_delta: end_delta.wrapping_add(insn.imm),
        });
    }
    None
}

fn direct_addr(addr: crate::AddrMode) -> Option<DirectAddr> {
    if addr.address_size != AddressSize::Dword || !matches!(addr.scale, 1 | 2 | 4 | 8) {
        return None;
    }
    Some(DirectAddr {
        segment: addr.segment,
        base: addr.base,
        index: addr.index,
        scale: addr.scale,
        disp: addr.disp as u32,
    })
}

fn stack_addr(disp: u32) -> DirectAddr {
    DirectAddr {
        segment: SegmentIndex::Ss,
        base: Some(4),
        index: None,
        scale: 1,
        disp,
    }
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

#[derive(Clone, Copy)]
struct MemorySideExits {
    cross_page_or_alignment: Label,
    unavailable_or_kind: Label,
    permission: Label,
    code_watch: Label,
    segment_limit: Option<Label>,
}

impl MemorySideExits {
    fn new(e: &mut Encoder, memory: MemoryEmitContext, addr: Option<DirectAddr>) -> Self {
        Self {
            cross_page_or_alignment: e.label(),
            unavailable_or_kind: e.label(),
            permission: e.label(),
            code_watch: e.label(),
            segment_limit: addr
                .filter(|addr| memory.segments.descriptor(addr.segment).limit != u32::MAX)
                .map(|_| e.label()),
        }
    }

    fn append_stubs(
        self,
        stubs: &mut Vec<(Label, Label, SideExitReason)>,
        common: Label,
        cross_page: bool,
        permission: bool,
        code_watch: bool,
    ) {
        if cross_page {
            stubs.push((
                self.cross_page_or_alignment,
                common,
                SideExitReason::CrossPageOrAlignment,
            ));
        }
        stubs.push((
            self.unavailable_or_kind,
            common,
            SideExitReason::UnavailableOrKind,
        ));
        if permission {
            stubs.push((self.permission, common, SideExitReason::Permission));
        }
        if code_watch {
            stubs.push((self.code_watch, common, SideExitReason::CodeWatch));
        }
        if let Some(segment_limit) = self.segment_limit {
            stubs.push((segment_limit, common, SideExitReason::Other));
        }
    }
}

fn emit(input: EmitInput<'_>) -> EmittedCode {
    let EmitInput {
        slots,
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
        x87_entry_top,
        memory,
        link_cell_ptrs,
    } = input;
    let full_accounting = StaticAccounting {
        instructions: span.instructions,
        raw_clocks: raw_clocks as u16,
        byte_reads,
        word_reads,
        dword_reads,
        weighted_fp_clocks,
    };
    let mut e = Encoder::new();
    for reg in SAVED_HOST_REGS {
        e.push(reg);
    }
    #[cfg(target_os = "windows")]
    if x87_entry_top.is_some() {
        e.push(Reg::RSI);
    }
    let native_stack_len = if x87_entry_top.is_some() {
        AVX2_X87_STACK_LEN
    } else {
        NATIVE_STACK_LEN
    };
    e.sub_r64_imm32(Reg::RSP, native_stack_len);
    #[cfg(target_os = "windows")]
    if x87_entry_top.is_some() {
        emit_save_x87_host_xmms(&mut e);
    }
    e.mov_r64_r64(Reg::R15, CPU_ARG);
    e.mov_r32_r32(Reg::RBP, FLAGS_ARG);
    e.mov_r64_r64(Reg::RAX, EXIT_ARG);
    e.store_r64_disp8(Reg::RSP, STACK_EXIT, Reg::RAX);
    e.mov_r32_r32(Reg::RAX, QUOTA_ARG);
    e.store_r64_disp8(Reg::RSP, STACK_QUOTA, Reg::RAX);
    e.xor_r64_self(Reg::RAX);
    e.store_r64_disp8(Reg::RSP, STACK_ITERATIONS, Reg::RAX);
    for (_, stack_offset, _) in dynamic_counter_fields() {
        e.store_r64_disp8(Reg::RSP, stack_offset, Reg::RAX);
    }
    for stack_offset in [
        STACK_INSTRUCTIONS,
        STACK_RAW_CLOCKS,
        STACK_BYTE_READS,
        STACK_DWORD_READS,
        STACK_WEIGHTED_FP_CLOCKS,
    ] {
        e.store_r64_disp8(Reg::RSP, stack_offset, Reg::RAX);
    }
    for (index, home) in GUEST_HOMES.into_iter().enumerate() {
        e.load_r32_disp32(home, Reg::R15, gpr_offset(index));
    }
    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    if x87_entry_top.is_some() {
        emit_x87_enter(&mut e, Reg::R15);
    }
    #[cfg(not(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    )))]
    debug_assert!(x87_entry_top.is_none());
    let loop_entry = e.label();
    let body_offset = e.position();
    e.place(loop_entry);

    let mut completed = 0u8;
    let mut completed_raw = 0u16;
    let mut completed_weighted_fp_clocks = 0u32;
    let mut completed_byte_reads = 0u8;
    let mut completed_word_reads = 0u8;
    let mut completed_dword_reads = 0u8;
    let mut side_exits = Vec::new();
    let mut side_exit_reason_stubs = Vec::new();
    let shared_return = e.label();
    let self_loop_return = self_loop.then(|| e.label());
    let mut terminal = false;
    let mut x87_gate_emitted = false;
    let mut current_x87_top = x87_entry_top;
    for slot in slots {
        match slot.kind {
            DirectKind::MovReg { dst, src, width } => match width {
                MemoryWidth::Word => e.mov_r16_r16(home(dst), home(src)),
                MemoryWidth::Dword => e.mov_r32_r32(home(dst), home(src)),
                MemoryWidth::Byte => unreachable!("byte register moves use MovRegByte"),
            },
            DirectKind::MovRegByte { dst, src } => {
                emit_read_store_value(&mut e, StoreSource::Reg(src), MemoryWidth::Byte, Reg::RDX);
                emit_write_gpr8(&mut e, dst, Reg::RDX);
            }
            DirectKind::MovImm { dst, imm } => e.mov_r32_imm32(home(dst), imm),
            DirectKind::MovImmByte { dst, imm } => {
                e.mov_r32_imm32(Reg::RDX, u32::from(imm));
                emit_write_gpr8(&mut e, dst, Reg::RDX);
            }
            DirectKind::Lea { dst, addr } => {
                emit_effective_address(&mut e, addr);
                e.mov_r32_r32(home(dst), Reg::RAX);
            }
            DirectKind::IncDecReg { dst, is_dec, width } => {
                emit_inc_dec_reg(&mut e, dst, is_dec, width);
            }
            DirectKind::AluReg {
                op,
                dst,
                src,
                width,
            } => {
                emit_alu(&mut e, op, dst, Some(src), None, width);
            }
            DirectKind::AluImm { op, dst, imm } => {
                emit_alu(&mut e, op, dst, None, Some(imm), MemoryWidth::Dword);
            }
            DirectKind::AluByteImm { op, dst, imm } => {
                emit_alu_byte_imm(&mut e, op, dst, imm);
            }
            DirectKind::AluMemSource {
                op,
                dst,
                width,
                addr,
            } => {
                let side = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, Some(addr));
                emit_alu_mem_source(&mut e, op, dst, width, addr, memory, reasons);
                reasons.append_stubs(
                    &mut side_exit_reason_stubs,
                    side,
                    width.needs_alignment_guard(),
                    memory.cpl3,
                    false,
                );
                side_exits.push((
                    side,
                    slot.lin.wrapping_sub(span.key.linear),
                    side_exit(
                        completed,
                        completed_raw,
                        completed_byte_reads,
                        completed_word_reads,
                        completed_dword_reads,
                        completed_weighted_fp_clocks,
                    ),
                ));
            }
            DirectKind::AluMemDest {
                op,
                source,
                width,
                addr,
            } => {
                let side = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, Some(addr));
                emit_alu_mem_dest(&mut e, op, source, width, addr, memory, reasons);
                reasons.append_stubs(
                    &mut side_exit_reason_stubs,
                    side,
                    width.needs_alignment_guard(),
                    memory.cpl3,
                    op != 7,
                );
                side_exits.push((
                    side,
                    slot.lin.wrapping_sub(span.key.linear),
                    side_exit(
                        completed,
                        completed_raw,
                        completed_byte_reads,
                        completed_word_reads,
                        completed_dword_reads,
                        completed_weighted_fp_clocks,
                    ),
                ));
            }
            DirectKind::Test { a, b } => emit_test(&mut e, a, b),
            DirectKind::TestImmReg { dst, imm, width } => {
                emit_test_imm_reg(&mut e, dst, imm, width);
            }
            DirectKind::TestImmMem { imm, width, addr } => {
                let side = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, Some(addr));
                emit_test_imm_mem(&mut e, imm, width, addr, memory, reasons);
                reasons.append_stubs(
                    &mut side_exit_reason_stubs,
                    side,
                    width.needs_alignment_guard(),
                    memory.cpl3,
                    false,
                );
                side_exits.push((
                    side,
                    slot.lin.wrapping_sub(span.key.linear),
                    side_exit(
                        completed,
                        completed_raw,
                        completed_byte_reads,
                        completed_word_reads,
                        completed_dword_reads,
                        completed_weighted_fp_clocks,
                    ),
                ));
            }
            DirectKind::Shift { op, dst, count } => emit_shift(&mut e, op, dst, count),
            DirectKind::DoubleShiftReg {
                left,
                dst,
                src,
                count,
            } => emit_double_shift_reg(&mut e, left, dst, src, count),
            DirectKind::DoubleShiftMem {
                left,
                src,
                count,
                addr,
            } => {
                let side = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, Some(addr));
                emit_double_shift_mem(&mut e, left, src, count, addr, memory, reasons);
                reasons.append_stubs(&mut side_exit_reason_stubs, side, true, memory.cpl3, true);
                side_exits.push((
                    side,
                    slot.lin.wrapping_sub(span.key.linear),
                    side_exit(
                        completed,
                        completed_raw,
                        completed_byte_reads,
                        completed_word_reads,
                        completed_dword_reads,
                        completed_weighted_fp_clocks,
                    ),
                ));
            }
            DirectKind::Load {
                dst, width, addr, ..
            } => {
                let side = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, Some(addr));
                emit_load(&mut e, dst, width, addr, memory, reasons);
                reasons.append_stubs(
                    &mut side_exit_reason_stubs,
                    side,
                    width.needs_alignment_guard(),
                    memory.cpl3,
                    false,
                );
                side_exits.push((
                    side,
                    slot.lin.wrapping_sub(span.key.linear),
                    side_exit(
                        completed,
                        completed_raw,
                        completed_byte_reads,
                        completed_word_reads,
                        completed_dword_reads,
                        completed_weighted_fp_clocks,
                    ),
                ));
            }
            DirectKind::Store {
                source,
                width,
                addr,
                ..
            } => {
                let side = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, Some(addr));
                emit_store(&mut e, source, width, addr, memory, reasons);
                reasons.append_stubs(
                    &mut side_exit_reason_stubs,
                    side,
                    width.needs_alignment_guard(),
                    memory.cpl3,
                    true,
                );
                side_exits.push((
                    side,
                    slot.lin.wrapping_sub(span.key.linear),
                    side_exit(
                        completed,
                        completed_raw,
                        completed_byte_reads,
                        completed_word_reads,
                        completed_dword_reads,
                        completed_weighted_fp_clocks,
                    ),
                ));
            }
            DirectKind::RmwIncDec {
                is_dec,
                width,
                addr,
            } => {
                let side = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, Some(addr));
                emit_rmw_inc_dec(&mut e, is_dec, width, addr, memory, reasons);
                reasons.append_stubs(
                    &mut side_exit_reason_stubs,
                    side,
                    width.needs_alignment_guard(),
                    memory.cpl3,
                    true,
                );
                side_exits.push((
                    side,
                    slot.lin.wrapping_sub(span.key.linear),
                    side_exit(
                        completed,
                        completed_raw,
                        completed_byte_reads,
                        completed_word_reads,
                        completed_dword_reads,
                        completed_weighted_fp_clocks,
                    ),
                ));
            }
            DirectKind::Push { source } => {
                let side = e.label();
                let reasons =
                    MemorySideExits::new(&mut e, memory, Some(stack_addr(0u32.wrapping_sub(4))));
                emit_store(
                    &mut e,
                    source,
                    MemoryWidth::Dword,
                    stack_addr(0u32.wrapping_sub(4)),
                    memory,
                    reasons,
                );
                reasons.append_stubs(&mut side_exit_reason_stubs, side, true, memory.cpl3, true);
                side_exits.push((
                    side,
                    slot.lin.wrapping_sub(span.key.linear),
                    side_exit(
                        completed,
                        completed_raw,
                        completed_byte_reads,
                        completed_word_reads,
                        completed_dword_reads,
                        completed_weighted_fp_clocks,
                    ),
                ));
                e.alu_r32_imm32(5, home(4), 4);
            }
            DirectKind::Pop { dst } => {
                let side = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, Some(stack_addr(0)));
                emit_ram_read_pointer(&mut e, MemoryWidth::Dword, stack_addr(0), memory, reasons);
                e.load_r32_disp8(Reg::RDX, Reg::RDI, 0);
                e.add_r32_imm32(home(4), 4);
                e.mov_r32_r32(home(dst), Reg::RDX);
                reasons.append_stubs(&mut side_exit_reason_stubs, side, true, memory.cpl3, false);
                side_exits.push((
                    side,
                    slot.lin.wrapping_sub(span.key.linear),
                    side_exit(
                        completed,
                        completed_raw,
                        completed_byte_reads,
                        completed_word_reads,
                        completed_dword_reads,
                        completed_weighted_fp_clocks,
                    ),
                ));
            }
            DirectKind::X87 { insn, addr } => {
                let side = e.label();
                let eligibility = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, addr);
                let top = current_x87_top.expect("x87 block must carry an entry TOP");
                // Every exceptional fast-path result exits before changing x87 state, so a
                // successful x87 instruction cannot make #MF pending for the next slot.
                emit_x87_slot(
                    &mut e,
                    insn,
                    addr,
                    memory,
                    reasons,
                    X87SlotEmitState {
                        eligibility_side: eligibility,
                        check_gate: !x87_gate_emitted,
                        top,
                    },
                );
                current_x87_top = Some(insn.advance_top(top));
                x87_gate_emitted = true;
                if let Some(access) = insn.metadata().memory {
                    reasons.append_stubs(
                        &mut side_exit_reason_stubs,
                        side,
                        true,
                        memory.cpl3,
                        access.direction == NativeX87MemoryDirection::Write,
                    );
                }
                side_exit_reason_stubs.push((eligibility, side, SideExitReason::Other));
                side_exits.push((
                    side,
                    slot.lin.wrapping_sub(span.key.linear),
                    side_exit(
                        completed,
                        completed_raw,
                        completed_byte_reads,
                        completed_word_reads,
                        completed_dword_reads,
                        completed_weighted_fp_clocks,
                    ),
                ));
            }
            DirectKind::Call {
                return_delta,
                target_delta,
            } => {
                let side = e.label();
                let reasons =
                    MemorySideExits::new(&mut e, memory, Some(stack_addr(0u32.wrapping_sub(4))));
                emit_store(
                    &mut e,
                    StoreSource::EipDelta(return_delta),
                    MemoryWidth::Dword,
                    stack_addr(0u32.wrapping_sub(4)),
                    memory,
                    reasons,
                );
                reasons.append_stubs(&mut side_exit_reason_stubs, side, true, memory.cpl3, true);
                side_exits.push((
                    side,
                    slot.lin.wrapping_sub(span.key.linear),
                    side_exit(
                        completed,
                        completed_raw,
                        completed_byte_reads,
                        completed_word_reads,
                        completed_dword_reads,
                        completed_weighted_fp_clocks,
                    ),
                ));
                e.alu_r32_imm32(5, home(4), 4);
                completed += 1;
                completed_raw += slot.kind.raw_clocks() as u16;
                completed_weighted_fp_clocks += slot.weighted_fp_clocks;
                completed_byte_reads += slot.kind.byte_reads();
                completed_word_reads += slot.kind.word_reads();
                completed_dword_reads += slot.kind.dword_reads();
                emit_completed_path(
                    &mut e,
                    span,
                    false,
                    target_delta,
                    Some(link_cell_ptrs[0]),
                    shared_return,
                    full_accounting,
                );
                terminal = true;
                break;
            }
            DirectKind::Jmp { target_delta } => {
                completed += 1;
                completed_raw += slot.kind.raw_clocks() as u16;
                completed_weighted_fp_clocks += slot.weighted_fp_clocks;
                completed_byte_reads += slot.kind.byte_reads();
                completed_word_reads += slot.kind.word_reads();
                completed_dword_reads += slot.kind.dword_reads();
                emit_completed_path(
                    &mut e,
                    span,
                    false,
                    target_delta,
                    Some(link_cell_ptrs[0]),
                    shared_return,
                    full_accounting,
                );
                terminal = true;
                break;
            }
            DirectKind::Ret { release } => {
                let side = e.label();
                let reasons = MemorySideExits::new(&mut e, memory, Some(stack_addr(0)));
                let limit = memory.segments.cs.limit;
                let limit_exit = (limit != u32::MAX).then(|| e.label());
                emit_ram_read_pointer_inner(
                    &mut e,
                    MemoryWidth::Dword,
                    stack_addr(0),
                    memory,
                    reasons,
                );
                e.load_r32_disp8(Reg::RDX, Reg::RDI, 0);
                if let Some(limit_exit) = limit_exit {
                    e.cmp_r32_imm32(Reg::RDX, limit);
                    e.jcc(7, limit_exit);
                    side_exit_reason_stubs.push((limit_exit, side, SideExitReason::Other));
                }
                emit_mode13_read_completion(&mut e, MemoryWidth::Dword);
                e.add_r32_imm32(home(4), 4 + u32::from(release));
                reasons.append_stubs(&mut side_exit_reason_stubs, side, true, memory.cpl3, false);
                side_exits.push((
                    side,
                    slot.lin.wrapping_sub(span.key.linear),
                    side_exit(
                        completed,
                        completed_raw,
                        completed_byte_reads,
                        completed_word_reads,
                        completed_dword_reads,
                        completed_weighted_fp_clocks,
                    ),
                ));
                completed += 1;
                completed_raw += slot.kind.raw_clocks() as u16;
                completed_weighted_fp_clocks += slot.weighted_fp_clocks;
                completed_byte_reads += slot.kind.byte_reads();
                completed_word_reads += slot.kind.word_reads();
                completed_dword_reads += slot.kind.dword_reads();
                emit_completed_dynamic_path(
                    &mut e,
                    span,
                    Reg::RDX,
                    link_cell_ptrs,
                    shared_return,
                    full_accounting,
                );
                terminal = true;
                break;
            }
            DirectKind::Jcc {
                condition,
                taken_delta,
            } => {
                completed += 1;
                completed_raw += slot.kind.raw_clocks() as u16;
                completed_weighted_fp_clocks += slot.weighted_fp_clocks;
                completed_byte_reads += slot.kind.byte_reads();
                completed_word_reads += slot.kind.word_reads();
                completed_dword_reads += slot.kind.dword_reads();
                emit_load_host_flags(&mut e);
                let taken = e.label();
                e.jcc(condition, taken);
                if self_loop {
                    emit_dynamic_increment(&mut e, STACK_ITERATIONS);
                    emit_advance_eip(&mut e, u32::from(span.guest_len));
                    e.jmp(self_loop_return.expect("self loop must have a return stub"));
                } else {
                    emit_completed_path(
                        &mut e,
                        span,
                        false,
                        u32::from(span.guest_len),
                        Some(link_cell_ptrs[0]),
                        shared_return,
                        full_accounting,
                    );
                }
                e.place(taken);
                if self_loop {
                    emit_dynamic_increment(&mut e, STACK_ITERATIONS);
                    debug_assert_eq!(taken_delta, 0);
                    e.load_r64_disp8(Reg::RAX, Reg::RSP, STACK_QUOTA);
                    e.sub_r64_imm32(Reg::RAX, 1);
                    e.store_r64_disp8(Reg::RSP, STACK_QUOTA, Reg::RAX);
                    e.jnz(loop_entry);
                    emit_advance_eip(&mut e, taken_delta);
                    e.jmp(self_loop_return.expect("self loop must have a return stub"));
                } else {
                    emit_completed_path(
                        &mut e,
                        span,
                        false,
                        taken_delta,
                        Some(link_cell_ptrs[1]),
                        shared_return,
                        full_accounting,
                    );
                }
                terminal = true;
                break;
            }
        }
        completed += 1;
        completed_raw += slot.kind.raw_clocks() as u16;
        completed_weighted_fp_clocks += slot.weighted_fp_clocks;
        completed_byte_reads += slot.kind.byte_reads();
        completed_word_reads += slot.kind.word_reads();
        completed_dword_reads += slot.kind.dword_reads();
    }
    if !terminal {
        emit_completed_path(
            &mut e,
            span,
            false,
            u32::from(span.guest_len),
            Some(link_cell_ptrs[0]),
            shared_return,
            full_accounting,
        );
    }
    if let Some(self_loop_return) = self_loop_return {
        e.place(self_loop_return);
        emit_accounting(
            &mut e,
            span,
            true,
            StaticAccounting::default(),
            true,
            full_accounting,
        );
        e.jmp(shared_return);
    }
    let side_return = (!side_exits.is_empty()).then(|| e.label());
    for (common, eip_delta, exit) in side_exits {
        let stub_count = side_exit_reason_stubs
            .iter()
            .filter(|(_, target, _)| *target == common)
            .count();
        let mut stub_index = 0;
        for &(label, target, reason) in &side_exit_reason_stubs {
            if target != common {
                continue;
            }
            stub_index += 1;
            e.place(label);
            e.mov_r8_imm8(Reg::RDX, reason as u8);
            if stub_index != stub_count {
                e.jmp(common);
            }
        }
        debug_assert_ne!(stub_count, 0);
        e.place(common);
        e.load_r64_disp8(Reg::RAX, Reg::RSP, STACK_EXIT);
        let reason_offset = u32::try_from(core::mem::offset_of!(NativeExit, side_exit_reason))
            .expect("native side-exit reason offset must fit a u32");
        e.add_r64_imm32(Reg::RAX, reason_offset);
        e.store_r8_disp8(Reg::RAX, 0, Reg::RDX);
        emit_add_static_accounting(&mut e, exit);
        e.mov_r64_imm64(Reg::RAX, u64::from(exit.instructions));
        e.store_r64_disp8(Reg::RSP, STACK_READ_KIND, Reg::RAX);
        emit_advance_eip(&mut e, eip_delta);
        e.load_r64_disp8(Reg::RAX, Reg::RSP, STACK_EXIT);
        e.store_imm32_disp32(
            Reg::RAX,
            core::mem::offset_of!(NativeExit, side_exit) as i32,
            1,
        );
        e.jmp(side_return.expect("side exit must have shared accounting"));
    }
    if let Some(side_return) = side_return {
        e.place(side_return);
        if self_loop {
            emit_add_repeated_accounting(&mut e, full_accounting);
        }
        emit_fetch_trace(
            &mut e,
            span,
            self_loop,
            TracePrefix::Stack(STACK_READ_KIND),
            false,
        );
        e.jmp(shared_return);
    }
    e.place(shared_return);
    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    if x87_entry_top.is_some() {
        emit_x87_spill(&mut e, Reg::R15);
    }
    #[cfg(target_os = "windows")]
    if x87_entry_top.is_some() {
        emit_restore_x87_host_xmms(&mut e);
    }
    emit_store_homes(&mut e);
    emit_return(&mut e, COUNTER_ALL, x87_entry_top.is_some());
    debug_assert_eq!(usize::from(completed), slots.len());
    debug_assert_eq!(u32::from(completed_raw), raw_clocks);
    debug_assert_eq!(completed_weighted_fp_clocks, weighted_fp_clocks);
    debug_assert_eq!(completed_byte_reads, byte_reads);
    debug_assert_eq!(completed_word_reads, word_reads);
    debug_assert_eq!(completed_dword_reads, dword_reads);
    debug_assert_eq!(
        slots.iter().map(|slot| slot.kind.byte_stores()).sum::<u8>(),
        byte_stores
    );
    debug_assert_eq!(
        slots.iter().map(|slot| slot.kind.word_stores()).sum::<u8>(),
        word_stores
    );
    debug_assert_eq!(
        slots
            .iter()
            .map(|slot| slot.kind.dword_stores())
            .sum::<u8>(),
        dword_stores
    );
    EmittedCode {
        code: e.finish(),
        body_offset,
    }
}

fn emit_accounting(
    e: &mut Encoder,
    span: BlockSpan,
    self_loop: bool,
    prefix: StaticAccounting,
    completed: bool,
    full: StaticAccounting,
) {
    if self_loop {
        emit_add_repeated_accounting(e, full);
    } else if completed {
        emit_add_static_accounting(e, full);
    }
    emit_add_static_accounting(e, prefix);
    emit_fetch_trace(
        e,
        span,
        self_loop,
        TracePrefix::Immediate(u32::from(prefix.instructions)),
        completed,
    );
}

fn accounting_fields(accounting: StaticAccounting) -> [(i8, u32); 5] {
    [
        (STACK_INSTRUCTIONS, u32::from(accounting.instructions)),
        (STACK_RAW_CLOCKS, u32::from(accounting.raw_clocks)),
        (STACK_BYTE_READS, u32::from(accounting.byte_reads)),
        (STACK_DWORD_READS, u32::from(accounting.dword_reads)),
        (STACK_WEIGHTED_FP_CLOCKS, accounting.weighted_fp_clocks),
    ]
}

fn emit_add_static_accounting(e: &mut Encoder, accounting: StaticAccounting) {
    for (stack_offset, value) in accounting_fields(accounting) {
        if value != 0 {
            e.mov_r32_imm32(Reg::RDX, value);
            e.add_r64_to_mem_disp8(Reg::RSP, stack_offset, Reg::RDX);
        }
    }
    if accounting.word_reads != 0 {
        e.mov_r64_imm64(Reg::RDX, u64::from(accounting.word_reads) << 32);
        e.add_r64_to_mem_disp8(Reg::RSP, STACK_BYTE_READS, Reg::RDX);
    }
}

fn emit_add_repeated_accounting(e: &mut Encoder, accounting: StaticAccounting) {
    for (stack_offset, value) in accounting_fields(accounting) {
        if value == 0 {
            continue;
        }
        e.load_r64_disp8(Reg::RDX, Reg::RSP, STACK_ITERATIONS);
        if value != 1 {
            e.imul_r64_imm32(Reg::RDX, value);
        }
        e.add_r64_to_mem_disp8(Reg::RSP, stack_offset, Reg::RDX);
    }
    if accounting.word_reads != 0 {
        e.load_r64_disp8(Reg::RDX, Reg::RSP, STACK_ITERATIONS);
        if accounting.word_reads != 1 {
            e.imul_r64_imm32(Reg::RDX, u32::from(accounting.word_reads));
        }
        e.shift_r64_imm8(4, Reg::RDX, 32);
        e.add_r64_to_mem_disp8(Reg::RSP, STACK_BYTE_READS, Reg::RDX);
    }
}

#[derive(Clone, Copy)]
enum TracePrefix {
    Immediate(u32),
    Stack(i8),
}

fn emit_fetch_trace(
    e: &mut Encoder,
    span: BlockSpan,
    self_loop: bool,
    prefix: TracePrefix,
    completed: bool,
) {
    let trace_len_offset = core::mem::offset_of!(NativeExit, trace_len) as i32;
    let trace_ptr_offset = core::mem::offset_of!(NativeExit, trace_ptr) as i32;
    e.load_r64_disp8(Reg::RAX, Reg::RSP, STACK_EXIT);
    e.load_r64_disp32(Reg::RCX, Reg::RAX, trace_ptr_offset);
    e.cmp_r64_imm32(Reg::RCX, 0);
    let done = e.label();
    e.jz(done);
    e.load_r32_disp32(Reg::RDI, Reg::RAX, trace_len_offset);
    e.mov_r64_r64(Reg::RDX, Reg::RDI);
    e.shift_r64_imm8(4, Reg::RDX, 4);
    e.add_r64_r64(Reg::RCX, Reg::RDX);
    e.store_u32_imm_disp32(
        Reg::RCX,
        core::mem::offset_of!(NativeBlockTrace, linear) as i32,
        span.key.linear,
    );
    e.store_u32_imm_disp32(
        Reg::RCX,
        core::mem::offset_of!(NativeBlockTrace, physical) as i32,
        span.key.physical,
    );
    if self_loop {
        e.load_r64_disp8(Reg::RDX, Reg::RSP, STACK_ITERATIONS);
        e.store_r32_disp32(
            Reg::RCX,
            core::mem::offset_of!(NativeBlockTrace, repetitions) as i32,
            Reg::RDX,
        );
    } else {
        e.store_u32_imm_disp32(
            Reg::RCX,
            core::mem::offset_of!(NativeBlockTrace, repetitions) as i32,
            u32::from(completed),
        );
    }
    match prefix {
        TracePrefix::Immediate(prefix) => e.store_u32_imm_disp32(
            Reg::RCX,
            core::mem::offset_of!(NativeBlockTrace, prefix_instructions) as i32,
            prefix,
        ),
        TracePrefix::Stack(offset) => {
            e.load_r64_disp8(Reg::RDX, Reg::RSP, offset);
            e.store_r32_disp32(
                Reg::RCX,
                core::mem::offset_of!(NativeBlockTrace, prefix_instructions) as i32,
                Reg::RDX,
            );
        }
    }
    e.add_r32_imm32(Reg::RDI, 1);
    e.load_r64_disp8(Reg::RAX, Reg::RSP, STACK_EXIT);
    e.store_r32_disp32(Reg::RAX, trace_len_offset, Reg::RDI);
    e.place(done);
}

fn emit_increment_exit_u32(e: &mut Encoder, offset: usize) {
    e.load_r64_disp8(Reg::RAX, Reg::RSP, STACK_EXIT);
    e.load_r32_disp32(Reg::RDI, Reg::RAX, offset as i32);
    e.add_r32_imm32(Reg::RDI, 1);
    e.store_r32_disp32(Reg::RAX, offset as i32, Reg::RDI);
}

fn emit_advance_eip(e: &mut Encoder, delta: u32) {
    if delta == 0 {
        return;
    }
    e.load_r32_disp32(Reg::RAX, Reg::R15, eip_offset());
    e.add_r32_imm32(Reg::RAX, delta);
    e.store_r32_disp32(Reg::R15, eip_offset(), Reg::RAX);
}

fn emit_completed_path(
    e: &mut Encoder,
    span: BlockSpan,
    self_loop: bool,
    eip_delta: u32,
    link_cell: Option<usize>,
    shared_return: Label,
    accounting: StaticAccounting,
) {
    emit_accounting(
        e,
        span,
        self_loop,
        StaticAccounting::default(),
        true,
        accounting,
    );
    emit_advance_eip(e, eip_delta);
    if let Some(link_cell) = link_cell {
        let unresolved = e.label();
        let returning = e.label();
        e.load_r64_disp8(Reg::RAX, Reg::RSP, STACK_QUOTA);
        e.sub_r64_imm32(Reg::RAX, 1);
        e.store_r64_disp8(Reg::RSP, STACK_QUOTA, Reg::RAX);
        e.jz(returning);
        e.mov_r64_imm64(Reg::RAX, link_cell as u64);
        e.load_r64_disp32(Reg::RAX, Reg::RAX, 0);
        e.cmp_r64_imm32(Reg::RAX, 0);
        e.jz(unresolved);
        e.mov_r64_r64(Reg::RDX, Reg::RAX);
        emit_increment_exit_u32(e, core::mem::offset_of!(NativeExit, linked_transfers));
        e.xor_r64_self(Reg::RDI);
        e.store_r64_disp8(Reg::RSP, STACK_ITERATIONS, Reg::RDI);
        e.jmp_r64(Reg::RDX);
        e.place(unresolved);
        emit_increment_exit_u32(e, core::mem::offset_of!(NativeExit, unresolved_exits));
        e.place(returning);
    }
    e.jmp(shared_return);
}

fn emit_completed_dynamic_path(
    e: &mut Encoder,
    span: BlockSpan,
    target: Reg,
    link_cells: [usize; 2],
    shared_return: Label,
    accounting: StaticAccounting,
) {
    e.store_r32_disp32(Reg::R15, eip_offset(), target);
    emit_accounting(
        e,
        span,
        false,
        StaticAccounting::default(),
        true,
        accounting,
    );
    e.load_r32_disp32(Reg::RDX, Reg::R15, eip_offset());
    let bind = e.label();
    e.load_r64_disp8(Reg::RDI, Reg::RSP, STACK_QUOTA);
    e.sub_r64_imm32(Reg::RDI, 1);
    e.store_r64_disp8(Reg::RSP, STACK_QUOTA, Reg::RDI);
    for link_cell in link_cells {
        let next = e.label();
        e.mov_r64_imm64(Reg::RAX, link_cell as u64);
        e.load_r64_disp8(
            Reg::RCX,
            Reg::RAX,
            core::mem::offset_of!(LinkCell, body) as i8,
        );
        e.cmp_r64_imm32(Reg::RCX, 0);
        e.jz(next);
        e.cmp_r32_disp8(
            Reg::RDX,
            Reg::RAX,
            core::mem::offset_of!(LinkCell, target_eip) as i8,
        );
        e.jnz(next);
        e.cmp_r64_imm32(Reg::RDI, 0);
        e.jz(shared_return);
        emit_increment_exit_u32(e, core::mem::offset_of!(NativeExit, linked_transfers));
        e.xor_r64_self(Reg::RDI);
        e.store_r64_disp8(Reg::RSP, STACK_ITERATIONS, Reg::RDI);
        e.jmp_r64(Reg::RCX);
        e.place(next);
    }
    e.cmp_r64_imm32(Reg::RDI, 0);
    e.jz(bind);
    emit_increment_exit_u32(e, core::mem::offset_of!(NativeExit, unresolved_exits));
    e.place(bind);
    e.load_r64_disp8(Reg::RAX, Reg::RSP, STACK_EXIT);
    e.mov_r64_imm64(Reg::RCX, link_cells[0] as u64);
    e.store_r64_disp32(
        Reg::RAX,
        core::mem::offset_of!(NativeExit, dynamic_link_cell) as i32,
        Reg::RCX,
    );
    e.store_r32_disp32(
        Reg::RAX,
        core::mem::offset_of!(NativeExit, dynamic_target_eip) as i32,
        Reg::RDX,
    );
    e.jmp(shared_return);
}

#[derive(Clone, Copy, Default)]
struct StaticAccounting {
    instructions: u8,
    raw_clocks: u16,
    byte_reads: u8,
    word_reads: u8,
    dword_reads: u8,
    weighted_fp_clocks: u32,
}

fn side_exit(
    instructions: u8,
    raw_clocks: u16,
    byte_reads: u8,
    word_reads: u8,
    dword_reads: u8,
    weighted_fp_clocks: u32,
) -> StaticAccounting {
    StaticAccounting {
        instructions,
        raw_clocks,
        byte_reads,
        word_reads,
        dword_reads,
        weighted_fp_clocks,
    }
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_load(
    e: &mut Encoder,
    dst: u8,
    width: MemoryWidth,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    emit_ram_read_pointer(e, width, addr, memory, sides);
    match width {
        MemoryWidth::Byte => {
            e.movzx_r32_byte_disp8(Reg::RDX, Reg::RDI, 0);
            emit_write_gpr8(e, dst, Reg::RDX);
        }
        MemoryWidth::Word => {
            e.movzx_r32_word_disp8(Reg::RDX, Reg::RDI, 0);
            emit_write_gpr16(e, dst, Reg::RDX);
        }
        MemoryWidth::Dword => {
            e.load_r32_disp8(Reg::RDX, Reg::RDI, 0);
            e.mov_r32_r32(home(dst), Reg::RDX);
        }
    }
}

#[derive(Clone, Copy)]
struct X87SlotEmitState {
    eligibility_side: Label,
    check_gate: bool,
    top: u8,
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_x87_slot(
    e: &mut Encoder,
    insn: NativeX87Insn,
    addr: Option<DirectAddr>,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
    state: X87SlotEmitState,
) {
    let access = insn.metadata().memory;
    if let Some(access) = access {
        emit_x87_memory_pointer(
            e,
            addr.expect("x87 memory operation has a direct address"),
            memory,
            sides,
            access.direction == NativeX87MemoryDirection::Write,
        );
    }
    emit_native_x87(
        e,
        insn,
        Avx2X87EmitContext {
            cpu: Reg::R15,
            memory: access.map(|_| Reg::RDI),
            side_exit: state.eligibility_side,
            check_gate: state.check_gate,
            top: state.top,
        },
    );
    if let Some(access) = access {
        emit_x87_memory_completion(
            e,
            access.direction,
            memory.map.expect("x87 memory block has fast-map bases"),
        );
    }
}

#[cfg(not(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
fn emit_x87_slot(
    _: &mut Encoder,
    _: NativeX87Insn,
    _: Option<DirectAddr>,
    _: MemoryEmitContext,
    _: MemorySideExits,
    _: X87SlotEmitState,
) {
    unreachable!("direct x87 lowering is x86-64-only")
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_x87_memory_pointer(
    e: &mut Encoder,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
    write: bool,
) {
    let map = memory.map.expect("x87 memory block has fast-map bases");
    emit_segmented_linear_address(e, addr, MemoryWidth::Dword, memory, sides);
    emit_wide_page_guard(e, MemoryWidth::Dword, sides.cross_page_or_alignment);

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDX, map.flags() as u64);
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    let valid = e.label();
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jz(valid);
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_MODE13_KIND));
    e.jnz(sides.unavailable_or_kind);
    e.place(valid);

    if write {
        emit_write_permission_check(e, memory.cpl3, sides.permission);
        emit_write_pointer(e, map, sides.unavailable_or_kind);
        let unwatched = e.label();
        emit_code_watch_branch(
            e,
            MemoryWidth::Dword,
            map,
            memory
                .code_watch_tables
                .expect("x87 store has code-watch tables"),
            sides.code_watch,
            unwatched,
        );
        e.place(unwatched);
    } else {
        emit_read_permission_check(e, memory.cpl3, sides.permission);
        emit_read_pointer(e, map, sides.unavailable_or_kind);
    }

    // Preserve the guest address and page kind across the x87 emitter while RDI remains the host
    // memory pointer. This stack slot is no longer needed by the completed code-watch probe.
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDX, map.flags() as u64);
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.and_r32_imm32(Reg::RDX, u32::from(NATIVE_KIND_MASK));
    e.shift_r64_imm8(4, Reg::RDX, 32);
    e.or_r64_r64(Reg::RDX, Reg::RAX);
    e.store_r64_disp8(Reg::RSP, STACK_READ_KIND, Reg::RDX);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_x87_memory_completion(
    e: &mut Encoder,
    direction: NativeX87MemoryDirection,
    map: NativeMapBases,
) {
    e.load_r64_disp8(Reg::RAX, Reg::RSP, STACK_READ_KIND);
    e.mov_r64_r64(Reg::RCX, Reg::RAX);
    e.shift_r64_imm8(5, Reg::RCX, 32);
    e.mov_r32_r32(Reg::RAX, Reg::RAX);
    e.cmp_r32_imm32(Reg::RCX, u32::from(NATIVE_MODE13_KIND));
    let mode13 = e.label();
    let done = e.label();
    e.jz(mode13);
    if direction == NativeX87MemoryDirection::Write {
        emit_dynamic_increment(e, STACK_RAM_DWORD_WRITES);
    }
    e.jmp(done);
    e.place(mode13);
    match direction {
        NativeX87MemoryDirection::Read => {
            emit_dynamic_increment(e, STACK_MODE13_DWORD_READS);
        }
        NativeX87MemoryDirection::Write => {
            emit_dynamic_increment(e, STACK_MODE13_DWORD_WRITES);
            emit_mode13_dirty_bit(e, map);
        }
    }
    e.place(done);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_ram_read_pointer(
    e: &mut Encoder,
    width: MemoryWidth,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    emit_ram_read_pointer_inner(e, width, addr, memory, sides);
    emit_mode13_read_completion(e, width);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_ram_read_pointer_inner(
    e: &mut Encoder,
    width: MemoryWidth,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    let map = memory.map.expect("native read has fast-map bases");
    emit_segmented_linear_address(e, addr, width, memory, sides);
    if width.needs_alignment_guard() {
        emit_wide_page_guard(e, width, sides.cross_page_or_alignment);
    }

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);

    e.mov_r64_imm64(Reg::RDX, map.flags() as u64);
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));

    let valid = e.label();
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jz(valid);
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_MODE13_KIND));
    e.jnz(sides.unavailable_or_kind);
    e.place(valid);
    e.store_r64_disp8(Reg::RSP, STACK_READ_KIND, Reg::RDI);
    emit_read_permission_check(e, memory.cpl3, sides.permission);
    emit_read_pointer(e, map, sides.unavailable_or_kind);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_mode13_read_completion(e: &mut Encoder, width: MemoryWidth) {
    let done = e.label();
    e.load_r64_disp8(Reg::RCX, Reg::RSP, STACK_READ_KIND);
    e.cmp_r32_imm32(Reg::RCX, u32::from(NATIVE_MODE13_KIND));
    e.jnz(done);
    match width {
        MemoryWidth::Byte => emit_dynamic_increment(e, STACK_MODE13_BYTE_READS),
        MemoryWidth::Word => emit_dynamic_word_increment(e, STACK_MODE13_BYTE_READS),
        MemoryWidth::Dword => emit_dynamic_increment(e, STACK_MODE13_DWORD_READS),
    }
    e.place(done);
}

#[cfg(not(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
fn emit_load(
    _: &mut Encoder,
    _: u8,
    _: MemoryWidth,
    _: DirectAddr,
    _: MemoryEmitContext,
    _: MemorySideExits,
) {
    unreachable!("direct memory lowering is x86-64-only")
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_alu_mem_source(
    e: &mut Encoder,
    op: u8,
    dst: u8,
    width: MemoryWidth,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    emit_ram_read_pointer(e, width, addr, memory, sides);
    match width {
        MemoryWidth::Byte => e.movzx_r32_byte_disp8(Reg::RCX, Reg::RDI, 0),
        MemoryWidth::Word => e.movzx_r32_word_disp8(Reg::RCX, Reg::RDI, 0),
        MemoryWidth::Dword => e.load_r32_disp8(Reg::RCX, Reg::RDI, 0),
    }
    e.mov_r32_r32(Reg::RAX, home(dst));
    emit_alu_preloaded(e, op, dst, width);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_test_imm_mem(
    e: &mut Encoder,
    imm: u32,
    width: MemoryWidth,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    emit_ram_read_pointer(e, width, addr, memory, sides);
    match width {
        MemoryWidth::Byte => e.movzx_r32_byte_disp8(Reg::RAX, Reg::RDI, 0),
        MemoryWidth::Word => e.movzx_r32_word_disp8(Reg::RAX, Reg::RDI, 0),
        MemoryWidth::Dword => e.load_r32_disp8(Reg::RAX, Reg::RDI, 0),
    }
    e.mov_r32_imm32(Reg::RCX, imm);
    emit_test_preloaded(e, width);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_alu_mem_dest(
    e: &mut Encoder,
    op: u8,
    source: StoreSource,
    width: MemoryWidth,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    let map = memory.map.expect("memory ALU has fast-map bases");
    if op == 7 {
        emit_ram_read_pointer(e, width, addr, memory, sides);
        match width {
            MemoryWidth::Byte => e.movzx_r32_byte_disp8(Reg::RAX, Reg::RDI, 0),
            MemoryWidth::Word => e.movzx_r32_word_disp8(Reg::RAX, Reg::RDI, 0),
            MemoryWidth::Dword => e.load_r32_disp8(Reg::RAX, Reg::RDI, 0),
        }
        emit_read_store_value(e, source, width, Reg::RCX);
        match width {
            MemoryWidth::Byte => emit_alu_byte_preloaded(e, op),
            MemoryWidth::Word | MemoryWidth::Dword => emit_alu_preloaded(e, op, 0, width),
        }
        return;
    }

    let code_watch_tables = memory
        .code_watch_tables
        .expect("writing memory ALU has code-watch tables");
    emit_segmented_linear_address(e, addr, width, memory, sides);
    if width.needs_alignment_guard() {
        emit_wide_page_guard(e, width, sides.cross_page_or_alignment);
    }

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDX, map.flags() as u64);
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    let valid = e.label();
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jz(valid);
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_MODE13_KIND));
    e.jnz(sides.unavailable_or_kind);
    e.place(valid);
    emit_write_permission_check(e, memory.cpl3, sides.permission);

    // Save the effective address and page kind before ADC/SBB load host flags into RAX. Nothing
    // below this point mutates architectural state until every pointer and code-watch check passes.
    e.shift_r64_imm8(4, Reg::RDI, 32);
    e.or_r64_r64(Reg::RDI, Reg::RAX);
    e.store_r64_disp32(Reg::RSP, STACK_ALU_ADDRESS_KIND, Reg::RDI);
    emit_read_pointer(e, map, sides.unavailable_or_kind);
    match width {
        MemoryWidth::Byte => e.movzx_r32_byte_disp8(Reg::RDX, Reg::RDI, 0),
        MemoryWidth::Word => e.movzx_r32_word_disp8(Reg::RDX, Reg::RDI, 0),
        MemoryWidth::Dword => e.load_r32_disp8(Reg::RDX, Reg::RDI, 0),
    }
    emit_read_store_value(e, source, width, Reg::RCX);
    emit_alu_candidate(e, op, width);

    e.load_r32_disp32(Reg::RAX, Reg::RSP, STACK_ALU_ADDRESS_KIND);
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_write_pointer(e, map, sides.unavailable_or_kind);
    emit_watched_alu_result_guard(e, width, map, code_watch_tables, sides.code_watch);

    emit_commit_alu_candidate(e, op, source, width);
    e.load_r32_disp32(Reg::RAX, Reg::RSP, STACK_ALU_ADDRESS_KIND);
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_write_pointer(e, map, sides.unavailable_or_kind);
    match width {
        MemoryWidth::Byte => e.store_r8_disp8(Reg::RDI, 0, Reg::RDX),
        MemoryWidth::Word => e.store_r16_disp8(Reg::RDI, 0, Reg::RDX),
        MemoryWidth::Dword => e.store_r32_disp8(Reg::RDI, 0, Reg::RDX),
    }

    e.load_r64_disp32(Reg::RCX, Reg::RSP, STACK_ALU_ADDRESS_KIND);
    e.shift_r64_imm8(5, Reg::RCX, 32);
    let mode13 = e.label();
    let done = e.label();
    e.cmp_r32_imm32(Reg::RCX, u32::from(NATIVE_MODE13_KIND));
    e.jz(mode13);
    match width {
        MemoryWidth::Byte => emit_dynamic_increment(e, STACK_RAM_BYTE_WRITES),
        MemoryWidth::Word => emit_dynamic_word_increment(e, STACK_RAM_BYTE_WRITES),
        MemoryWidth::Dword => emit_dynamic_increment(e, STACK_RAM_DWORD_WRITES),
    }
    e.jmp(done);
    e.place(mode13);
    match width {
        MemoryWidth::Byte => {
            emit_dynamic_increment(e, STACK_MODE13_BYTE_READS);
            emit_dynamic_increment(e, STACK_MODE13_BYTE_WRITES);
        }
        MemoryWidth::Word => {
            emit_dynamic_word_increment(e, STACK_MODE13_BYTE_READS);
            emit_dynamic_word_increment(e, STACK_MODE13_BYTE_WRITES);
        }
        MemoryWidth::Dword => {
            emit_dynamic_increment(e, STACK_MODE13_DWORD_READS);
            emit_dynamic_increment(e, STACK_MODE13_DWORD_WRITES);
        }
    }
    emit_mode13_dirty_bit(e, map);
    e.place(done);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_double_shift_mem(
    e: &mut Encoder,
    left: bool,
    src: u8,
    count: ShiftCount,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    let map = memory.map.expect("memory double shift has fast-map bases");
    let code_watch_tables = memory
        .code_watch_tables
        .expect("memory double shift has code-watch tables");
    emit_segmented_linear_address(e, addr, MemoryWidth::Dword, memory, sides);
    emit_wide_page_guard(e, MemoryWidth::Dword, sides.cross_page_or_alignment);

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDX, map.flags() as u64);
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    let valid = e.label();
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jz(valid);
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_MODE13_KIND));
    e.jnz(sides.unavailable_or_kind);
    e.place(valid);
    emit_write_permission_check(e, memory.cpl3, sides.permission);

    // Save the effective address and page kind before computing the candidate. Architectural
    // flags, registers, and memory remain untouched until every pointer and code-watch check passes.
    e.shift_r64_imm8(4, Reg::RDI, 32);
    e.or_r64_r64(Reg::RDI, Reg::RAX);
    e.store_r64_disp32(Reg::RSP, STACK_ALU_ADDRESS_KIND, Reg::RDI);
    emit_read_pointer(e, map, sides.unavailable_or_kind);
    e.load_r32_disp8(Reg::RDX, Reg::RDI, 0);
    e.store_r32_disp32(Reg::RSP, STACK_ALU_OLD_RESULT, Reg::RDX);
    emit_double_shift_candidate(e, left, src, count, Reg::RDX);
    e.store_r32_disp32(Reg::RSP, STACK_ALU_OLD_RESULT + 4, Reg::RDX);

    e.load_r32_disp32(Reg::RAX, Reg::RSP, STACK_ALU_ADDRESS_KIND);
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_write_pointer(e, map, sides.unavailable_or_kind);
    emit_watched_alu_result_guard(
        e,
        MemoryWidth::Dword,
        map,
        code_watch_tables,
        sides.code_watch,
    );

    emit_commit_double_shift_flags(e, count);
    e.load_r32_disp32(Reg::RAX, Reg::RSP, STACK_ALU_ADDRESS_KIND);
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_write_pointer(e, map, sides.unavailable_or_kind);
    e.load_r32_disp32(Reg::RDX, Reg::RSP, STACK_ALU_OLD_RESULT + 4);
    e.store_r32_disp8(Reg::RDI, 0, Reg::RDX);

    e.load_r64_disp32(Reg::RCX, Reg::RSP, STACK_ALU_ADDRESS_KIND);
    e.shift_r64_imm8(5, Reg::RCX, 32);
    let mode13 = e.label();
    let done = e.label();
    e.cmp_r32_imm32(Reg::RCX, u32::from(NATIVE_MODE13_KIND));
    e.jz(mode13);
    emit_dynamic_increment(e, STACK_RAM_DWORD_WRITES);
    e.jmp(done);
    e.place(mode13);
    emit_dynamic_increment(e, STACK_MODE13_DWORD_READS);
    emit_dynamic_increment(e, STACK_MODE13_DWORD_WRITES);
    emit_mode13_dirty_bit(e, map);
    e.place(done);
}

#[cfg(not(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
fn emit_alu_mem_source(
    _: &mut Encoder,
    _: u8,
    _: u8,
    _: MemoryWidth,
    _: DirectAddr,
    _: MemoryEmitContext,
    _: MemorySideExits,
) {
    unreachable!("direct memory lowering is x86-64-only")
}

#[cfg(not(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
fn emit_test_imm_mem(
    _: &mut Encoder,
    _: u32,
    _: MemoryWidth,
    _: DirectAddr,
    _: MemoryEmitContext,
    _: MemorySideExits,
) {
    unreachable!("direct memory lowering is x86-64-only")
}

#[cfg(not(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
fn emit_alu_mem_dest(
    _: &mut Encoder,
    _: u8,
    _: StoreSource,
    _: MemoryWidth,
    _: DirectAddr,
    _: MemoryEmitContext,
    _: MemorySideExits,
) {
    unreachable!("direct memory lowering is x86-64-only")
}

#[cfg(not(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
fn emit_double_shift_mem(
    _: &mut Encoder,
    _: bool,
    _: u8,
    _: ShiftCount,
    _: DirectAddr,
    _: MemoryEmitContext,
    _: MemorySideExits,
) {
    unreachable!("direct memory lowering is x86-64-only")
}

fn emit_effective_address(e: &mut Encoder, addr: DirectAddr) {
    e.mov_r32_imm32(Reg::RAX, addr.disp);
    if let Some(base) = addr.base {
        e.add_r32_r32(Reg::RAX, home(base));
    }
    if let Some(index) = addr.index {
        if addr.scale == 1 {
            e.add_r32_r32(Reg::RAX, home(index));
        } else {
            e.mov_r32_r32(Reg::RCX, home(index));
            e.shl_r32_imm8(Reg::RCX, addr.scale.trailing_zeros() as u8);
            e.add_r32_r32(Reg::RAX, Reg::RCX);
        }
    }
}

fn emit_segmented_linear_address(
    e: &mut Encoder,
    addr: DirectAddr,
    width: MemoryWidth,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    emit_effective_address(e, addr);
    let descriptor = memory.segments.descriptor(addr.segment);
    if descriptor.limit != u32::MAX {
        let Some(max_start) = descriptor.limit.checked_sub(width.bytes() - 1) else {
            e.jmp(
                sides
                    .segment_limit
                    .expect("finite native segment has a limit side exit"),
            );
            return;
        };
        e.cmp_r32_imm32(Reg::RAX, max_start);
        e.jcc(
            7,
            sides
                .segment_limit
                .expect("finite native segment has a limit side exit"),
        );
    }
    if descriptor.base != 0 {
        e.add_r32_imm32(Reg::RAX, descriptor.base);
    }
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_store(
    e: &mut Encoder,
    source: StoreSource,
    width: MemoryWidth,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    let map = memory.map.expect("native store has fast-map bases");
    let code_watch_tables = memory
        .code_watch_tables
        .expect("native store has code-watch tables");
    emit_segmented_linear_address(e, addr, width, memory, sides);
    if width.needs_alignment_guard() {
        emit_wide_page_guard(e, width, sides.cross_page_or_alignment);
    }

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDX, map.flags() as u64);
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));

    let ram = e.label();
    let mode13 = e.label();
    let done = e.label();
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jz(ram);
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_MODE13_KIND));
    e.jz(mode13);
    e.jmp(sides.unavailable_or_kind);

    e.place(ram);
    emit_write_permission_check(e, memory.cpl3, sides.permission);
    emit_write_pointer(e, map, sides.unavailable_or_kind);
    emit_watched_store_guard(e, source, width, map, code_watch_tables, sides.code_watch);
    emit_store_value(e, source, width);
    match width {
        MemoryWidth::Byte => emit_dynamic_increment(e, STACK_RAM_BYTE_WRITES),
        MemoryWidth::Word => emit_dynamic_word_increment(e, STACK_RAM_BYTE_WRITES),
        MemoryWidth::Dword => emit_dynamic_increment(e, STACK_RAM_DWORD_WRITES),
    }
    e.jmp(done);

    e.place(mode13);
    emit_write_permission_check(e, memory.cpl3, sides.permission);
    emit_write_pointer(e, map, sides.unavailable_or_kind);
    emit_watched_store_guard(e, source, width, map, code_watch_tables, sides.code_watch);
    emit_store_value(e, source, width);
    match width {
        MemoryWidth::Byte => emit_dynamic_increment(e, STACK_MODE13_BYTE_WRITES),
        MemoryWidth::Word => emit_dynamic_word_increment(e, STACK_MODE13_BYTE_WRITES),
        MemoryWidth::Dword => emit_dynamic_increment(e, STACK_MODE13_DWORD_WRITES),
    }
    emit_mode13_dirty_bit(e, map);
    e.place(done);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_rmw_inc_dec(
    e: &mut Encoder,
    is_dec: bool,
    width: MemoryWidth,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    if matches!(width, MemoryWidth::Dword) {
        emit_rmw_inc_dec_dword(e, is_dec, addr, memory, sides);
        return;
    }
    debug_assert!(matches!(width, MemoryWidth::Word));
    let map = memory.map.expect("native RMW has fast-map bases");
    let code_watch_tables = memory
        .code_watch_tables
        .expect("native RMW has code-watch tables");
    emit_segmented_linear_address(e, addr, width, memory, sides);
    emit_wide_page_guard(e, width, sides.cross_page_or_alignment);

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDX, map.flags() as u64);
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    let valid = e.label();
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jz(valid);
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_MODE13_KIND));
    e.jnz(sides.unavailable_or_kind);
    e.place(valid);
    emit_write_permission_check(e, memory.cpl3, sides.permission);

    // INC/DEC always changes its operand, so a watched chunk exits before any mutation.
    let unwatched = e.label();
    emit_code_watch_branch(
        e,
        width,
        map,
        code_watch_tables,
        sides.code_watch,
        unwatched,
    );
    e.place(unwatched);

    e.shift_r64_imm8(4, Reg::RDI, 32);
    e.or_r64_r64(Reg::RDI, Reg::RAX);
    e.store_r64_disp32(Reg::RSP, STACK_ALU_ADDRESS_KIND, Reg::RDI);

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDI, map.read_biases() as u64);
    e.load_r64_sib_scale8(Reg::RDI, Reg::RDI, Reg::RCX);
    e.cmp_r64_imm32(Reg::RDI, NATIVE_UNAVAILABLE_BIAS as u32);
    e.jz(sides.unavailable_or_kind);
    e.add_r64_r64(Reg::RDI, Reg::RAX);
    e.mov_r64_imm64(Reg::RDX, map.write_biases() as u64);
    e.load_r64_sib_scale8(Reg::RDX, Reg::RDX, Reg::RCX);
    e.cmp_r64_imm32(Reg::RDX, NATIVE_UNAVAILABLE_BIAS as u32);
    e.jz(sides.unavailable_or_kind);
    e.add_r64_r64(Reg::RDX, Reg::RAX);

    match width {
        MemoryWidth::Byte => unreachable!("group 5 INC/DEC is word or dword"),
        MemoryWidth::Word => e.movzx_r32_word_disp8(Reg::RCX, Reg::RDI, 0),
        MemoryWidth::Dword => e.load_r32_disp8(Reg::RCX, Reg::RDI, 0),
    }
    e.mov_r32_r32(Reg::RAX, Reg::RCX);
    match width {
        MemoryWidth::Byte => unreachable!("group 5 INC/DEC is word or dword"),
        MemoryWidth::Word => {
            e.mov_r32_imm32(Reg::RDI, 1);
            e.alu_r16_r16(if is_dec { 5 } else { 0 }, Reg::RAX, Reg::RDI);
        }
        MemoryWidth::Dword => e.alu_r32_imm32(if is_dec { 5 } else { 0 }, Reg::RAX, 1),
    }
    emit_capture_flags(e, ARITH_FLAGS & !crate::FLAG_CF);
    emit_pending_inc_dec(e, is_dec, width, Reg::RCX, Reg::RAX);
    match width {
        MemoryWidth::Byte => unreachable!("group 5 INC/DEC is word or dword"),
        MemoryWidth::Word => e.store_r16_disp8(Reg::RDX, 0, Reg::RAX),
        MemoryWidth::Dword => e.store_r32_disp8(Reg::RDX, 0, Reg::RAX),
    }

    e.load_r64_disp32(Reg::RAX, Reg::RSP, STACK_ALU_ADDRESS_KIND);
    e.mov_r64_r64(Reg::RCX, Reg::RAX);
    e.shift_r64_imm8(5, Reg::RCX, 32);
    e.mov_r32_r32(Reg::RAX, Reg::RAX);
    let mode13 = e.label();
    let done = e.label();
    e.cmp_r32_imm32(Reg::RCX, u32::from(NATIVE_MODE13_KIND));
    e.jz(mode13);
    match width {
        MemoryWidth::Byte => unreachable!("group 5 INC/DEC is word or dword"),
        MemoryWidth::Word => emit_dynamic_word_increment(e, STACK_RAM_BYTE_WRITES),
        MemoryWidth::Dword => emit_dynamic_increment(e, STACK_RAM_DWORD_WRITES),
    }
    e.jmp(done);
    e.place(mode13);
    match width {
        MemoryWidth::Byte => unreachable!("group 5 INC/DEC is word or dword"),
        MemoryWidth::Word => {
            emit_dynamic_word_increment(e, STACK_MODE13_BYTE_READS);
            emit_dynamic_word_increment(e, STACK_MODE13_BYTE_WRITES);
        }
        MemoryWidth::Dword => {
            emit_dynamic_increment(e, STACK_MODE13_DWORD_READS);
            emit_dynamic_increment(e, STACK_MODE13_DWORD_WRITES);
        }
    }
    emit_mode13_dirty_bit(e, map);
    e.place(done);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_rmw_inc_dec_dword(
    e: &mut Encoder,
    is_dec: bool,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    let map = memory.map.expect("native RMW has fast-map bases");
    let code_watch_tables = memory
        .code_watch_tables
        .expect("native RMW has code-watch tables");
    emit_segmented_linear_address(e, addr, MemoryWidth::Dword, memory, sides);
    emit_wide_page_guard(e, MemoryWidth::Dword, sides.cross_page_or_alignment);

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDX, map.flags() as u64);
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jnz(sides.unavailable_or_kind);
    emit_write_permission_check(e, memory.cpl3, sides.permission);

    let unwatched = e.label();
    emit_code_watch_branch(
        e,
        MemoryWidth::Dword,
        map,
        code_watch_tables,
        sides.code_watch,
        unwatched,
    );
    e.place(unwatched);

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDI, map.read_biases() as u64);
    e.load_r64_sib_scale8(Reg::RDI, Reg::RDI, Reg::RCX);
    e.cmp_r64_imm32(Reg::RDI, NATIVE_UNAVAILABLE_BIAS as u32);
    e.jz(sides.unavailable_or_kind);
    e.add_r64_r64(Reg::RDI, Reg::RAX);
    e.mov_r64_imm64(Reg::RDX, map.write_biases() as u64);
    e.load_r64_sib_scale8(Reg::RDX, Reg::RDX, Reg::RCX);
    e.cmp_r64_imm32(Reg::RDX, NATIVE_UNAVAILABLE_BIAS as u32);
    e.jz(sides.unavailable_or_kind);
    e.add_r64_r64(Reg::RDX, Reg::RAX);

    e.load_r32_disp8(Reg::RCX, Reg::RDI, 0);
    e.mov_r32_r32(Reg::RAX, Reg::RCX);
    e.alu_r32_imm32(if is_dec { 5 } else { 0 }, Reg::RAX, 1);
    emit_capture_flags(e, ARITH_FLAGS & !crate::FLAG_CF);
    emit_pending_inc_dec(e, is_dec, MemoryWidth::Dword, Reg::RCX, Reg::RAX);
    e.store_r32_disp8(Reg::RDX, 0, Reg::RAX);
    emit_dynamic_increment(e, STACK_RAM_DWORD_WRITES);
}

fn emit_wide_page_guard(e: &mut Encoder, width: MemoryWidth, side: Label) {
    debug_assert!(width.needs_alignment_guard());
    e.mov_r32_r32(Reg::RDX, Reg::RAX);
    e.and_r32_imm32(Reg::RDX, width.bytes() - 1);
    e.cmp_r32_imm32(Reg::RDX, 0);
    e.jnz(side);
    e.mov_r32_r32(Reg::RDX, Reg::RAX);
    e.and_r32_imm32(Reg::RDX, 0x0fff);
    e.cmp_r32_imm32(Reg::RDX, 0x1000 - width.bytes());
    e.jcc(7, side);
}

fn emit_pending_inc_dec(e: &mut Encoder, is_dec: bool, width: MemoryWidth, old: Reg, result: Reg) {
    let base = pending_offset();
    e.mov_r32_r32(Reg::RDI, Reg::RBP);
    e.and_r32_imm32(Reg::RDI, crate::FLAG_CF);
    e.shl_r32_imm8(Reg::RDI, 17);
    let width_tag = match width {
        MemoryWidth::Byte => 0,
        MemoryWidth::Word => 0x100,
        MemoryWidth::Dword => 0x200,
    };
    e.or_r32_imm32(
        Reg::RDI,
        0x8001_0000 | width_tag | if is_dec { 1 } else { 0 },
    );
    e.store_r32_disp32(Reg::R15, base, Reg::RDI);
    e.store_r32_disp32(Reg::R15, base + 4, old);
    e.store_u32_imm_disp32(Reg::R15, base + 8, 1);
    e.store_r32_disp32(Reg::R15, base + 12, result);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_write_permission_check(e: &mut Encoder, memory_cpl3: bool, side: super::encoder::Label) {
    // A ring-0 write to a supervisor read-only PTE is valid while CR0.WP is clear. A populated
    // write bias already proves the page walk admitted the current context. Ring 3 additionally
    // requires both architectural permission bits.
    if memory_cpl3 {
        e.and_r32_imm32(Reg::RDX, u32::from(NATIVE_PAGE_USER | NATIVE_PAGE_WRITABLE));
        e.cmp_r32_imm32(Reg::RDX, u32::from(NATIVE_PAGE_USER | NATIVE_PAGE_WRITABLE));
        e.jnz(side);
    }
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_read_permission_check(e: &mut Encoder, memory_cpl3: bool, side: Label) {
    if memory_cpl3 {
        e.and_r32_imm32(Reg::RDX, u32::from(NATIVE_PAGE_USER));
        e.cmp_r32_imm32(Reg::RDX, u32::from(NATIVE_PAGE_USER));
        e.jnz(side);
    }
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_read_pointer(e: &mut Encoder, map: NativeMapBases, side: Label) {
    e.mov_r64_imm64(Reg::RDI, map.read_biases() as u64);
    e.load_r64_sib_scale8(Reg::RDI, Reg::RDI, Reg::RCX);
    e.cmp_r64_imm32(Reg::RDI, NATIVE_UNAVAILABLE_BIAS as u32);
    e.jz(side);
    e.add_r64_r64(Reg::RDI, Reg::RAX);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_write_pointer(e: &mut Encoder, map: NativeMapBases, side: super::encoder::Label) {
    e.mov_r64_imm64(Reg::RDI, map.write_biases() as u64);
    e.load_r64_sib_scale8(Reg::RDI, Reg::RDI, Reg::RCX);
    e.cmp_r64_imm32(Reg::RDI, NATIVE_UNAVAILABLE_BIAS as u32);
    e.jz(side);
    e.add_r64_r64(Reg::RDI, Reg::RAX);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_watched_store_guard(
    e: &mut Encoder,
    source: StoreSource,
    width: MemoryWidth,
    map: NativeMapBases,
    code_watch_tables: [usize; 2],
    side: super::encoder::Label,
) {
    let watched = e.label();
    let unwatched = e.label();
    emit_code_watch_branch(e, width, map, code_watch_tables, watched, unwatched);
    e.place(watched);
    emit_read_store_value(e, source, width, Reg::RDX);
    match width {
        MemoryWidth::Byte => e.cmp_r8_disp8(Reg::RDX, Reg::RDI, 0),
        MemoryWidth::Word => e.cmp_r16_disp8(Reg::RDX, Reg::RDI, 0),
        MemoryWidth::Dword => e.cmp_r32_disp8(Reg::RDX, Reg::RDI, 0),
    }
    e.jnz(side);
    e.place(unwatched);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_watched_alu_result_guard(
    e: &mut Encoder,
    width: MemoryWidth,
    map: NativeMapBases,
    code_watch_tables: [usize; 2],
    side: Label,
) {
    let watched = e.label();
    let unwatched = e.label();
    emit_code_watch_branch(e, width, map, code_watch_tables, watched, unwatched);
    e.place(watched);
    e.load_r32_disp32(Reg::RDX, Reg::RSP, STACK_ALU_OLD_RESULT + 4);
    match width {
        MemoryWidth::Byte => e.cmp_r8_disp8(Reg::RDX, Reg::RDI, 0),
        MemoryWidth::Word => e.cmp_r16_disp8(Reg::RDX, Reg::RDI, 0),
        MemoryWidth::Dword => e.cmp_r32_disp8(Reg::RDX, Reg::RDI, 0),
    }
    e.jnz(side);
    e.place(unwatched);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_code_watch_branch(
    e: &mut Encoder,
    width: MemoryWidth,
    map: NativeMapBases,
    code_watch_tables: [usize; 2],
    watched: super::encoder::Label,
    unwatched: super::encoder::Label,
) {
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDX, map.physical_pages() as u64);
    e.load_r32_sib_scale4(Reg::RCX, Reg::RDX, Reg::RCX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.store_r64_disp8(Reg::RSP, STACK_WATCH_PAGE, Reg::RCX);
    let second = e.label();
    emit_code_watch_table_branch(e, width, code_watch_tables[0], watched, second);
    e.place(second);
    emit_code_watch_table_branch(e, width, code_watch_tables[1], watched, unwatched);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_code_watch_table_branch(
    e: &mut Encoder,
    width: MemoryWidth,
    code_watch_table: usize,
    watched: Label,
    unwatched: Label,
) {
    e.load_r64_disp8(Reg::RCX, Reg::RSP, STACK_WATCH_PAGE);
    e.mov_r64_imm64(Reg::RDX, code_watch_table as u64);
    e.load_r64_sib_scale8(Reg::RDX, Reg::RDX, Reg::RCX);
    e.cmp_r64_imm32(Reg::RDX, 0);
    e.jz(unwatched);

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.and_r32_imm32(Reg::RCX, 0x0fff);
    e.shift_r32_imm8(5, Reg::RCX, 4);
    e.bt_r64_mem(Reg::RDX, Reg::RCX);
    e.jcc(2, watched);
    if width.needs_alignment_guard() {
        e.mov_r32_r32(Reg::RCX, Reg::RAX);
        e.and_r32_imm32(Reg::RCX, 0x0fff);
        e.add_r32_imm32(Reg::RCX, width.bytes() - 1);
        e.shift_r32_imm8(5, Reg::RCX, 4);
        e.bt_r64_mem(Reg::RDX, Reg::RCX);
        e.jcc(2, watched);
    }
    e.jmp(unwatched);
}

fn emit_read_store_value(e: &mut Encoder, source: StoreSource, width: MemoryWidth, value: Reg) {
    match source {
        StoreSource::Reg(src) => match width {
            MemoryWidth::Byte => {
                let lane = if src < 4 { src } else { src - 4 };
                e.mov_r32_r32(value, home(lane));
                if src >= 4 {
                    e.shift_r32_imm8(5, value, 8);
                }
                e.and_r32_imm32(value, 0xff);
            }
            MemoryWidth::Word => {
                e.mov_r32_r32(value, home(src));
                e.and_r32_imm32(value, 0xffff);
            }
            MemoryWidth::Dword => e.mov_r32_r32(value, home(src)),
        },
        StoreSource::Imm(imm) => e.mov_r32_imm32(
            value,
            match width {
                MemoryWidth::Byte => imm & 0xff,
                MemoryWidth::Word => imm & 0xffff,
                MemoryWidth::Dword => imm,
            },
        ),
        StoreSource::EipDelta(delta) => {
            debug_assert!(matches!(width, MemoryWidth::Dword));
            e.load_r32_disp32(value, Reg::R15, eip_offset());
            e.add_r32_imm32(value, delta);
        }
    }
}

fn emit_store_value(e: &mut Encoder, source: StoreSource, width: MemoryWidth) {
    match width {
        MemoryWidth::Byte => {
            emit_read_store_value(e, source, width, Reg::RDX);
            e.store_r8_disp8(Reg::RDI, 0, Reg::RDX);
        }
        MemoryWidth::Word => {
            emit_read_store_value(e, source, width, Reg::RDX);
            e.store_r16_disp8(Reg::RDI, 0, Reg::RDX);
        }
        MemoryWidth::Dword => {
            emit_read_store_value(e, source, width, Reg::RDX);
            e.store_r32_disp8(Reg::RDI, 0, Reg::RDX);
        }
    }
}

fn emit_dynamic_increment(e: &mut Encoder, offset: i8) {
    e.mov_r64_imm64(Reg::RDX, 1);
    e.add_r64_to_mem_disp8(Reg::RSP, offset, Reg::RDX);
}

fn emit_dynamic_word_increment(e: &mut Encoder, byte_counter_offset: i8) {
    e.mov_r64_imm64(Reg::RDX, 1u64 << 32);
    e.add_r64_to_mem_disp8(Reg::RSP, byte_counter_offset, Reg::RDX);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_mode13_dirty_bit(e: &mut Encoder, map: NativeMapBases) {
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDI, map.physical_pages() as u64);
    e.load_r32_sib_scale4(Reg::RDX, Reg::RDI, Reg::RCX);
    e.add_r32_imm32(Reg::RDX, 0u32.wrapping_sub(0x000a_0000));
    e.shift_r32_imm8(5, Reg::RDX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_r64(Reg::RDI, Reg::RSP);
    e.add_r64_imm32(Reg::RDI, u32::from(STACK_MODE13_DIRTY_PAGES as u8));
    e.bts_r64_mem(Reg::RDI, Reg::RDX);
}

#[cfg(not(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
fn emit_store(
    _: &mut Encoder,
    _: StoreSource,
    _: MemoryWidth,
    _: DirectAddr,
    _: MemoryEmitContext,
    _: MemorySideExits,
) {
    unreachable!("direct memory lowering is x86-64-only")
}

#[cfg(not(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
fn emit_rmw_inc_dec(
    _: &mut Encoder,
    _: bool,
    _: MemoryWidth,
    _: DirectAddr,
    _: MemoryEmitContext,
    _: MemorySideExits,
) {
    unreachable!("direct memory lowering is x86-64-only")
}

fn emit_write_gpr8(e: &mut Encoder, index: u8, value: Reg) {
    let (home, shift, mask) = if index < 4 {
        (home(index), 0, !0xff)
    } else {
        (home(index - 4), 8, !0xff00)
    };
    if shift != 0 {
        e.shl_r32_imm8(value, shift);
    }
    e.and_r32_imm32(home, mask);
    e.or_r32_r32(home, value);
}

fn emit_write_gpr16(e: &mut Encoder, index: u8, value: Reg) {
    e.mov_r16_r16(home(index), value);
}

fn home(index: u8) -> Reg {
    GUEST_HOMES[usize::from(index & 7)]
}

fn gpr_offset(index: usize) -> i32 {
    (core::mem::offset_of!(CpuGsw, registers)
        + core::mem::offset_of!(Registers, gpr)
        + index * core::mem::size_of::<u32>()) as i32
}

fn eip_offset() -> i32 {
    (core::mem::offset_of!(CpuGsw, registers) + core::mem::offset_of!(Registers, eip)) as i32
}

fn eflags_offset() -> i32 {
    (core::mem::offset_of!(CpuGsw, registers) + core::mem::offset_of!(Registers, eflags)) as i32
}

fn pending_offset() -> i32 {
    core::mem::offset_of!(CpuGsw, pending_flags) as i32
}

fn emit_alu_candidate(e: &mut Encoder, op: u8, width: MemoryWidth) {
    debug_assert_ne!(op, 7);
    e.store_r32_disp32(Reg::RSP, STACK_ALU_OLD_RESULT, Reg::RDX);
    if matches!(op, 2 | 3) {
        emit_load_host_flags(e);
    }
    match width {
        MemoryWidth::Byte => e.alu_r8_r8(op, Reg::RDX, Reg::RCX),
        MemoryWidth::Word => e.alu_r16_r16(op, Reg::RDX, Reg::RCX),
        MemoryWidth::Dword => e.alu_r32_r32(op, Reg::RDX, Reg::RCX),
    }
    e.pushfq();
    e.pop(Reg::RAX);
    e.store_r32_disp32(Reg::RSP, STACK_ALU_FLAGS, Reg::RAX);
    e.store_r32_disp32(Reg::RSP, STACK_ALU_OLD_RESULT + 4, Reg::RDX);
}

fn emit_commit_alu_candidate(e: &mut Encoder, op: u8, source: StoreSource, width: MemoryWidth) {
    let load_values = |e: &mut Encoder| {
        e.load_r32_disp32(Reg::RAX, Reg::RSP, STACK_ALU_OLD_RESULT);
        emit_read_store_value(e, source, width, Reg::RCX);
        e.load_r32_disp32(Reg::RDX, Reg::RSP, STACK_ALU_OLD_RESULT + 4);
    };
    let capture = |e: &mut Encoder, defined: u32| {
        e.load_r32_disp32(Reg::RDI, Reg::RSP, STACK_ALU_FLAGS);
        e.and_r32_imm32(Reg::RBP, !defined);
        e.and_r32_imm32(Reg::RDI, defined);
        e.or_r32_r32(Reg::RBP, Reg::RDI);
    };
    let width_tag = match width {
        MemoryWidth::Byte => 0,
        MemoryWidth::Word => 0x100,
        MemoryWidth::Dword => 0x200,
    };

    if matches!(op, 2 | 3) {
        e.mov_r32_r32(Reg::RDI, Reg::RBP);
        e.and_r32_imm32(Reg::RDI, crate::FLAG_CF);
        let carry = e.label();
        let done = e.label();
        e.jnz(carry);
        capture(e, ARITH_FLAGS);
        load_values(e);
        emit_pending(
            e,
            0x8000_0000 | width_tag | u32::from(op == 3),
            Some(Reg::RAX),
            Some(Reg::RCX),
            Reg::RDX,
        );
        e.jmp(done);
        e.place(carry);
        capture(e, ARITH_FLAGS);
        e.store_r32_disp32(Reg::R15, eflags_offset(), Reg::RBP);
        emit_clear_pending(e);
        e.load_r32_disp32(Reg::RDX, Reg::RSP, STACK_ALU_OLD_RESULT + 4);
        e.place(done);
        return;
    }

    if matches!(op, 1 | 4 | 6) {
        capture(e, LOGIC_FLAGS);
        load_values(e);
        emit_pending(e, 0x8000_0002 | width_tag, None, None, Reg::RDX);
        emit_logic_live_af(e);
        e.load_r32_disp32(Reg::RDX, Reg::RSP, STACK_ALU_OLD_RESULT + 4);
        return;
    }

    debug_assert!(matches!(op, 0 | 5));
    capture(e, ARITH_FLAGS);
    load_values(e);
    emit_pending(
        e,
        0x8000_0000 | width_tag | u32::from(op == 5),
        Some(Reg::RAX),
        Some(Reg::RCX),
        Reg::RDX,
    );
}

fn emit_alu(
    e: &mut Encoder,
    op: u8,
    dst: u8,
    src: Option<u8>,
    imm: Option<u32>,
    width: MemoryWidth,
) {
    e.mov_r32_r32(Reg::RAX, home(dst));
    if let Some(src) = src {
        e.mov_r32_r32(Reg::RCX, home(src));
    } else {
        e.mov_r32_imm32(Reg::RCX, imm.expect("register or immediate source"));
    }
    emit_alu_preloaded(e, op, dst, width);
}

/// Emit an ALU operation with the old destination in EAX and the source in ECX.
fn emit_alu_preloaded(e: &mut Encoder, op: u8, dst: u8, width: MemoryWidth) {
    if matches!(width, MemoryWidth::Word) {
        debug_assert_eq!(op, 7, "the current word ALU family only admits CMP");
        e.and_r32_imm32(Reg::RAX, 0xffff);
        e.and_r32_imm32(Reg::RCX, 0xffff);
        e.mov_r32_r32(Reg::RDX, Reg::RAX);
        e.alu_r16_r16(5, Reg::RDX, Reg::RCX);
        emit_capture_flags(e, ARITH_FLAGS);
        emit_pending(e, 0x8000_0101, Some(Reg::RAX), Some(Reg::RCX), Reg::RDX);
        return;
    }
    debug_assert!(matches!(width, MemoryWidth::Dword));
    if matches!(op, 2 | 3) {
        emit_carry_alu_preloaded(e, op, home(dst));
        return;
    }
    let writes = op != 7;
    let target = if writes {
        home(dst)
    } else {
        e.mov_r32_r32(Reg::RDX, Reg::RAX);
        Reg::RDX
    };
    let host_op = if op == 7 { 5 } else { op };
    e.alu_r32_r32(host_op, target, Reg::RCX);

    if matches!(op, 1 | 4 | 6) {
        emit_capture_flags(e, LOGIC_FLAGS);
        emit_pending(e, 0x8000_0202, None, None, target);
        emit_logic_live_af(e);
    } else {
        emit_capture_flags(e, ARITH_FLAGS);
        let tag = if op == 0 { 0x8000_0200 } else { 0x8000_0201 };
        emit_pending(e, tag, Some(Reg::RAX), Some(Reg::RCX), target);
    }
}

fn emit_carry_alu_preloaded(e: &mut Encoder, op: u8, target: Reg) {
    debug_assert!(matches!(op, 2 | 3));
    let carry = e.label();
    let done = e.label();
    e.mov_r32_r32(Reg::RDI, Reg::RBP);
    e.and_r32_imm32(Reg::RDI, crate::FLAG_CF);
    e.jnz(carry);

    e.alu_r32_r32(op, target, Reg::RCX);
    emit_capture_flags(e, ARITH_FLAGS);
    emit_pending(
        e,
        if op == 2 { 0x8000_0200 } else { 0x8000_0201 },
        Some(Reg::RAX),
        Some(Reg::RCX),
        target,
    );
    e.jmp(done);

    e.place(carry);
    emit_load_host_flags(e);
    e.alu_r32_r32(op, target, Reg::RCX);
    emit_capture_flags(e, ARITH_FLAGS);
    e.store_r32_disp32(Reg::R15, eflags_offset(), Reg::RBP);
    emit_clear_pending(e);
    e.place(done);
}

fn emit_inc_dec_reg(e: &mut Encoder, dst: u8, is_dec: bool, width: MemoryWidth) {
    e.mov_r32_r32(Reg::RAX, home(dst));
    match width {
        MemoryWidth::Byte => unreachable!("register INC/DEC is word or dword"),
        MemoryWidth::Word => {
            e.mov_r32_imm32(Reg::RDX, 1);
            e.alu_r16_r16(if is_dec { 5 } else { 0 }, home(dst), Reg::RDX);
        }
        MemoryWidth::Dword => e.alu_r32_imm32(if is_dec { 5 } else { 0 }, home(dst), 1),
    }
    emit_capture_flags(e, ARITH_FLAGS & !crate::FLAG_CF);
    if matches!(width, MemoryWidth::Word) {
        e.and_r32_imm32(Reg::RAX, 0xffff);
        e.mov_r32_r32(Reg::RDX, home(dst));
        e.and_r32_imm32(Reg::RDX, 0xffff);
        emit_pending_inc_dec(e, is_dec, width, Reg::RAX, Reg::RDX);
    } else {
        emit_pending_inc_dec(e, is_dec, width, Reg::RAX, home(dst));
    }
}

fn emit_alu_byte_imm(e: &mut Encoder, op: u8, dst: u8, imm: u8) {
    emit_read_store_value(e, StoreSource::Reg(dst), MemoryWidth::Byte, Reg::RAX);
    e.mov_r32_imm32(Reg::RCX, u32::from(imm));
    emit_alu_byte_preloaded(e, op);

    if op != 7 {
        emit_write_gpr8(e, dst, Reg::RDX);
    }
}

fn emit_alu_byte_preloaded(e: &mut Encoder, op: u8) {
    e.mov_r32_r32(Reg::RDX, Reg::RAX);

    if matches!(op, 2 | 3) {
        emit_carry_alu_byte(e, op);
    } else {
        let host_op = if op == 7 { 5 } else { op };
        e.alu_r8_r8(host_op, Reg::RDX, Reg::RCX);
        if matches!(op, 1 | 4 | 6) {
            emit_pending(e, 0x8000_0002, None, None, Reg::RDX);
            emit_capture_flags(e, LOGIC_FLAGS);
            emit_logic_live_af(e);
            e.load_r32_disp32(Reg::RDX, Reg::R15, pending_offset() + 12);
        } else {
            emit_pending(
                e,
                if op == 0 { 0x8000_0000 } else { 0x8000_0001 },
                Some(Reg::RAX),
                Some(Reg::RCX),
                Reg::RDX,
            );
            emit_capture_flags(e, ARITH_FLAGS);
        }
    }
}

fn emit_carry_alu_byte(e: &mut Encoder, op: u8) {
    debug_assert!(matches!(op, 2 | 3));
    let carry = e.label();
    let done = e.label();
    e.mov_r32_r32(Reg::RDI, Reg::RBP);
    e.and_r32_imm32(Reg::RDI, crate::FLAG_CF);
    e.jnz(carry);

    e.alu_r8_r8(op, Reg::RDX, Reg::RCX);
    emit_pending(
        e,
        if op == 2 { 0x8000_0000 } else { 0x8000_0001 },
        Some(Reg::RAX),
        Some(Reg::RCX),
        Reg::RDX,
    );
    emit_capture_flags(e, ARITH_FLAGS);
    e.jmp(done);

    e.place(carry);
    emit_load_host_flags(e);
    e.alu_r8_r8(op, Reg::RDX, Reg::RCX);
    emit_capture_flags(e, ARITH_FLAGS);
    e.store_r32_disp32(Reg::R15, eflags_offset(), Reg::RBP);
    emit_clear_pending(e);
    e.place(done);
}

fn emit_test(e: &mut Encoder, a: u8, b: u8) {
    e.mov_r32_r32(Reg::RDX, home(a));
    e.alu_r32_r32(4, Reg::RDX, home(b));
    emit_capture_flags(e, LOGIC_FLAGS);
    emit_pending(e, 0x8000_0202, None, None, Reg::RDX);
    emit_logic_live_af(e);
}

fn emit_test_imm_reg(e: &mut Encoder, dst: u8, imm: u32, width: MemoryWidth) {
    emit_read_store_value(e, StoreSource::Reg(dst), width, Reg::RAX);
    e.mov_r32_imm32(Reg::RCX, imm);
    emit_test_preloaded(e, width);
}

fn emit_test_preloaded(e: &mut Encoder, width: MemoryWidth) {
    e.mov_r32_r32(Reg::RDX, Reg::RAX);
    match width {
        MemoryWidth::Byte => e.alu_r8_r8(4, Reg::RDX, Reg::RCX),
        MemoryWidth::Word => e.alu_r16_r16(4, Reg::RDX, Reg::RCX),
        MemoryWidth::Dword => e.alu_r32_r32(4, Reg::RDX, Reg::RCX),
    }
    emit_capture_flags(e, LOGIC_FLAGS);
    emit_pending(
        e,
        match width {
            MemoryWidth::Byte => 0x8000_0002,
            MemoryWidth::Word => 0x8000_0102,
            MemoryWidth::Dword => 0x8000_0202,
        },
        None,
        None,
        Reg::RDX,
    );
    emit_logic_live_af(e);
}

fn emit_shift(e: &mut Encoder, op: u8, dst: u8, raw_count: u8) {
    let count = raw_count & 0x1f;
    if count == 0 {
        return;
    }
    e.shift_r32_imm8(op, home(dst), count);
    let mut defined = crate::FLAG_CF | crate::FLAG_PF | crate::FLAG_ZF | crate::FLAG_SF;
    if count == 1 {
        defined |= crate::FLAG_OF;
    }
    emit_capture_flags(e, defined);
    e.store_r32_disp32(Reg::R15, eflags_offset(), Reg::RBP);
    emit_clear_pending(e);
}

fn emit_double_shift_reg(e: &mut Encoder, left: bool, dst: u8, src: u8, count: ShiftCount) {
    emit_double_shift_candidate(e, left, src, count, home(dst));
    emit_commit_double_shift_flags(e, count);
}

fn emit_double_shift_candidate(
    e: &mut Encoder,
    left: bool,
    src: u8,
    count: ShiftCount,
    target: Reg,
) {
    let immediate = match count {
        ShiftCount::Immediate(count) => Some(count),
        ShiftCount::Cl => {
            e.mov_r32_r32(Reg::RCX, home(1));
            e.store_r32_disp32(Reg::RSP, STACK_SHIFT_COUNT, Reg::RCX);
            None
        }
    };
    e.double_shift_r32(left, target, home(src), immediate);
    e.pushfq();
    e.pop(Reg::RAX);
    e.store_r32_disp32(Reg::RSP, STACK_ALU_FLAGS, Reg::RAX);
}

fn emit_commit_double_shift_flags(e: &mut Encoder, count: ShiftCount) {
    const DEFINED: u32 = crate::FLAG_CF | crate::FLAG_PF | crate::FLAG_ZF | crate::FLAG_SF;
    match count {
        ShiftCount::Immediate(count) => match count & 0x1f {
            0 => {}
            1 => emit_merge_double_shift_flags(e, DEFINED | crate::FLAG_OF),
            _ => emit_merge_double_shift_flags(e, DEFINED),
        },
        ShiftCount::Cl => {
            let one = e.label();
            let done = e.label();
            e.load_r32_disp32(Reg::RAX, Reg::RSP, STACK_SHIFT_COUNT);
            e.and_r32_imm32(Reg::RAX, 0x1f);
            e.cmp_r32_imm32(Reg::RAX, 0);
            e.jz(done);
            e.cmp_r32_imm32(Reg::RAX, 1);
            e.jz(one);
            emit_merge_double_shift_flags(e, DEFINED);
            e.jmp(done);
            e.place(one);
            emit_merge_double_shift_flags(e, DEFINED | crate::FLAG_OF);
            e.place(done);
        }
    }
}

fn emit_merge_double_shift_flags(e: &mut Encoder, defined: u32) {
    e.load_r32_disp32(Reg::RDI, Reg::RSP, STACK_ALU_FLAGS);
    e.and_r32_imm32(Reg::RBP, !defined);
    e.and_r32_imm32(Reg::RDI, defined);
    e.or_r32_r32(Reg::RBP, Reg::RDI);
    e.store_r32_disp32(Reg::R15, eflags_offset(), Reg::RBP);
    emit_clear_pending(e);
}

fn emit_capture_flags(e: &mut Encoder, defined: u32) {
    e.pushfq();
    e.pop(Reg::RDI);
    e.and_r32_imm32(Reg::RBP, !defined);
    e.and_r32_imm32(Reg::RDI, defined);
    e.or_r32_r32(Reg::RBP, Reg::RDI);
}

fn emit_load_host_flags(e: &mut Encoder) {
    e.mov_r32_r32(Reg::RAX, Reg::RBP);
    e.and_r32_imm32(Reg::RAX, ARITH_FLAGS | 0x2);
    e.push(Reg::RAX);
    e.popfq();
}

fn emit_logic_live_af(e: &mut Encoder) {
    e.load_r32_disp32(Reg::RDI, Reg::R15, eflags_offset());
    e.and_r32_imm32(Reg::RDI, !crate::FLAG_AF);
    e.mov_r32_r32(Reg::RDX, Reg::RBP);
    e.and_r32_imm32(Reg::RDX, crate::FLAG_AF);
    e.or_r32_r32(Reg::RDI, Reg::RDX);
    e.or_r32_imm32(Reg::RDI, 0x2);
    e.store_r32_disp32(Reg::R15, eflags_offset(), Reg::RDI);
}

fn emit_pending(e: &mut Encoder, tag: u32, a: Option<Reg>, b: Option<Reg>, result: Reg) {
    let base = pending_offset();
    e.store_u32_imm_disp32(Reg::R15, base, tag);
    if let Some(a) = a {
        e.store_r32_disp32(Reg::R15, base + 4, a);
    } else {
        e.store_u32_imm_disp32(Reg::R15, base + 4, 0);
    }
    if let Some(b) = b {
        e.store_r32_disp32(Reg::R15, base + 8, b);
    } else {
        e.store_u32_imm_disp32(Reg::R15, base + 8, 0);
    }
    e.store_r32_disp32(Reg::R15, base + 12, result);
}

fn emit_clear_pending(e: &mut Encoder) {
    let base = pending_offset();
    for offset in [0, 4, 8, 12] {
        e.store_u32_imm_disp32(Reg::R15, base + offset, 0);
    }
}

fn emit_store_homes(e: &mut Encoder) {
    for (index, home) in GUEST_HOMES.into_iter().enumerate() {
        e.store_r32_disp32(Reg::R15, gpr_offset(index), home);
    }
}

#[cfg(target_os = "windows")]
const X87_NONVOLATILE_XMMS: [Xmm; 6] = [
    Xmm::XMM6,
    Xmm::XMM7,
    Xmm::XMM8,
    Xmm::XMM9,
    Xmm::XMM10,
    Xmm::XMM11,
];

#[cfg(target_os = "windows")]
fn emit_save_x87_host_xmms(e: &mut Encoder) {
    for (index, xmm) in X87_NONVOLATILE_XMMS.into_iter().enumerate() {
        e.vmovupd_disp32_xmm(Reg::RSP, NATIVE_STACK_LEN as i32 + (index as i32) * 16, xmm);
    }
}

#[cfg(target_os = "windows")]
fn emit_restore_x87_host_xmms(e: &mut Encoder) {
    for (index, xmm) in X87_NONVOLATILE_XMMS.into_iter().enumerate() {
        e.vmovupd_xmm_disp32(xmm, Reg::RSP, NATIVE_STACK_LEN as i32 + (index as i32) * 16);
    }
}

fn emit_return(e: &mut Encoder, counter_mask: u16, cached_x87: bool) {
    e.load_r64_disp8(Reg::RDI, Reg::RSP, STACK_EXIT);
    for (bit, stack_offset, output_offset) in dynamic_counter_fields() {
        if counter_mask & bit != 0 {
            e.load_r64_disp8(Reg::RAX, Reg::RSP, stack_offset);
            e.store_r64_disp32(Reg::RDI, output_offset as i32, Reg::RAX);
        }
    }
    for (stack_offset, output_offset) in [
        (
            STACK_INSTRUCTIONS,
            core::mem::offset_of!(NativeExit, instructions),
        ),
        (
            STACK_RAW_CLOCKS,
            core::mem::offset_of!(NativeExit, raw_clocks),
        ),
        (
            STACK_BYTE_READS,
            core::mem::offset_of!(NativeExit, byte_reads),
        ),
        (
            STACK_DWORD_READS,
            core::mem::offset_of!(NativeExit, dword_reads),
        ),
        (
            STACK_WEIGHTED_FP_CLOCKS,
            core::mem::offset_of!(NativeExit, weighted_fp_clocks),
        ),
    ] {
        e.load_r64_disp8(Reg::RAX, Reg::RSP, stack_offset);
        e.store_r64_disp32(Reg::RDI, output_offset as i32, Reg::RAX);
    }
    let native_stack_len = if cached_x87 {
        AVX2_X87_STACK_LEN
    } else {
        NATIVE_STACK_LEN
    };
    e.add_r64_imm32(Reg::RSP, native_stack_len);
    #[cfg(target_os = "windows")]
    if cached_x87 {
        e.pop(Reg::RSI);
    }
    for reg in SAVED_HOST_REGS.into_iter().rev() {
        e.pop(reg);
    }
    e.ret();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(linear: u32) -> BlockKey {
        BlockKey::new(linear, 0x20_000 + (linear & 0xfff), 7)
    }

    fn trivial_compilation(span: BlockSpan) -> Compilation {
        let mut fetch_lens = [0; MAX_BLOCK_INSTRUCTIONS];
        fetch_lens[0] = 1;
        Compilation {
            span,
            decode_residency_epoch: 0,
            fetch_lens,
            raw_clocks: 1,
            weighted_fp_clocks: 0,
            byte_reads: 0,
            word_reads: 0,
            dword_reads: 0,
            byte_stores: 0,
            word_stores: 0,
            dword_stores: 0,
            segment_layout: SegmentLayout::capture(&CpuGsw::default(), 0, 0)
                .expect("default segment layout"),
            memory_cpl3: false,
            has_wide_accesses: false,
            self_loop: false,
            has_x87: false,
            x87_entry_top: 0,
            x87_exit_top: 0,
            dynamic_successor: false,
            successors: [None, None],
            link_cells: [Arc::new(LinkCell::new()), Arc::new(LinkCell::new())],
            body_offset: 0,
            code: vec![0xc3],
        }
    }

    #[cfg(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    fn install_trivial(cache: &mut BlockCache, key: BlockKey, guest_len: usize) -> BlockId {
        assert!(matches!(cache.probe(key), BlockProbe::Interpret));
        assert!(matches!(cache.probe(key), BlockProbe::Compile));
        let span = BlockSpan::new(key, guest_len, 1).expect("test block must be page local");
        cache
            .install(&trivial_compilation(span))
            .expect("test block must install")
    }

    #[cfg(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    fn install_dynamic_trivial(cache: &mut BlockCache, key: BlockKey) -> BlockId {
        assert!(matches!(cache.probe(key), BlockProbe::Interpret));
        assert!(matches!(cache.probe(key), BlockProbe::Compile));
        let span = BlockSpan::new(key, 1, 1).expect("test block must be page local");
        let mut compilation = trivial_compilation(span);
        compilation.dynamic_successor = true;
        cache
            .install(&compilation)
            .expect("dynamic test block must install")
    }

    #[test]
    fn span_is_bounded_and_page_local() {
        assert!(BlockSpan::new(key(0x1234), 64, MAX_BLOCK_INSTRUCTIONS).is_some());
        assert!(BlockSpan::new(key(0x1ff0), 17, 1).is_none());
        assert!(BlockSpan::new(key(0x1234), 1, MAX_BLOCK_INSTRUCTIONS + 1).is_none());
        assert!(BlockSpan::new(key(0x1234), 0, 1).is_none());
    }

    #[test]
    fn default_metadata_is_bounded_above_the_executable_arena() {
        let cache = BlockCache::default();
        assert_eq!(cache.entry_cap, DEFAULT_ENTRY_CAP);
        let arena_slots =
            super::super::exec_mem::EXECUTABLE_ARENA_LEN / super::super::exec_mem::host_page_len();
        assert!(cache.entry_cap > arena_slots);
    }

    #[test]
    fn dynamic_counter_mask_tracks_only_reachable_outputs() {
        let addr = DirectAddr {
            segment: SegmentIndex::Ds,
            base: None,
            index: None,
            scale: 1,
            disp: 0,
        };
        let slot = |kind| DirectInsn {
            lin: 0,
            len: 1,
            weighted_fp_clocks: 0,
            kind,
        };
        let byte_store = slot(DirectKind::Store {
            source: StoreSource::Reg(0),
            width: MemoryWidth::Byte,
            addr,
            raw_clocks: 1,
        });
        let dword_store = slot(DirectKind::Store {
            source: StoreSource::Reg(0),
            width: MemoryWidth::Dword,
            addr,
            raw_clocks: 1,
        });
        let rmw = slot(DirectKind::RmwIncDec {
            is_dec: false,
            width: MemoryWidth::Dword,
            addr,
        });
        let byte_alu = slot(DirectKind::AluMemDest {
            op: 0,
            source: StoreSource::Imm(1),
            width: MemoryWidth::Byte,
            addr,
        });
        let dword_cmp = slot(DirectKind::AluMemDest {
            op: 7,
            source: StoreSource::Reg(0),
            width: MemoryWidth::Dword,
            addr,
        });

        assert_eq!(
            dynamic_counter_mask(&[byte_store]),
            COUNTER_RAM_BYTE_WRITE | COUNTER_MODE13_BYTE_WRITE | COUNTER_MODE13_DIRTY
        );
        assert_eq!(
            dynamic_counter_mask(&[dword_store]),
            COUNTER_RAM_DWORD_WRITE | COUNTER_MODE13_DWORD_WRITE | COUNTER_MODE13_DIRTY
        );
        assert_eq!(dynamic_counter_mask(&[rmw]), COUNTER_RAM_DWORD_WRITE);
        assert_eq!(
            dynamic_counter_mask(&[byte_alu]),
            COUNTER_MODE13_BYTE_READ
                | COUNTER_RAM_BYTE_WRITE
                | COUNTER_MODE13_BYTE_WRITE
                | COUNTER_MODE13_DIRTY
        );
        assert_eq!(
            dynamic_counter_mask(&[dword_cmp]),
            COUNTER_MODE13_DWORD_READ
        );
        assert_eq!(
            dynamic_counter_mask(&[byte_store, dword_store, rmw]),
            COUNTER_RAM_BYTE_WRITE
                | COUNTER_RAM_DWORD_WRITE
                | COUNTER_MODE13_BYTE_WRITE
                | COUNTER_MODE13_DWORD_WRITE
                | COUNTER_MODE13_DIRTY
        );
        assert_eq!(
            dynamic_counter_mask(&[slot(DirectKind::MovImm { dst: 0, imm: 0 })]),
            0
        );
    }

    #[test]
    fn first_observation_interprets_and_second_compiles() {
        let mut cache = BlockCache::default();
        let key = key(0x1234);
        assert!(matches!(cache.probe(key), BlockProbe::Interpret));
        assert!(matches!(cache.probe(key), BlockProbe::Compile));
        cache.reject(key);
        assert!(matches!(cache.probe(key), BlockProbe::Rejected));
    }

    #[test]
    fn capacity_pressure_clears_seen_entries() {
        let mut cache = BlockCache::with_entry_cap(2);
        let first = key(0x1000);
        assert!(matches!(cache.probe(first), BlockProbe::Interpret));
        assert!(matches!(cache.probe(key(0x1100)), BlockProbe::Interpret));
        assert!(matches!(cache.probe(key(0x1200)), BlockProbe::Interpret));
        assert!(matches!(cache.probe(first), BlockProbe::Interpret));
    }

    #[cfg(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    #[test]
    fn reset_ignores_stale_hot_entries_without_clearing_the_table() {
        let mut cache = BlockCache::with_entry_cap(2);
        let first = key(0x1000);
        assert!(matches!(cache.probe(first), BlockProbe::Interpret));
        assert!(matches!(cache.probe(first), BlockProbe::Compile));
        let span = BlockSpan::new(first, 1, 1).expect("one byte is page local");
        cache
            .install(&trivial_compilation(span))
            .expect("block must install");
        let hot_index = first.hot_index();
        let stale = cache.hot[hot_index].expect("install fills the hot slot");

        cache.clear();

        assert!(
            cache.hot[hot_index].is_some(),
            "reset must not scan the hot table"
        );
        assert_ne!(stale.generation, cache.hot_generation);
        assert!(matches!(cache.probe(first), BlockProbe::Interpret));
    }

    #[cfg(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    #[test]
    fn hash_fallback_preserves_hot_slot_collisions() {
        let mut cache = BlockCache::default();
        let first = key(0x1000);
        let second = (0x1001..)
            .map(key)
            .find(|candidate| candidate.hot_index() == first.hot_index())
            .expect("the finite hot table must collide");

        for candidate in [first, second] {
            assert!(matches!(cache.probe(candidate), BlockProbe::Interpret));
            assert!(matches!(cache.probe(candidate), BlockProbe::Compile));
            let span = BlockSpan::new(candidate, 1, 1).expect("one byte is page local");
            cache
                .install(&trivial_compilation(span))
                .expect("block must install");
        }

        assert!(matches!(cache.probe(first), BlockProbe::Ready(_)));
        assert!(matches!(cache.probe(second), BlockProbe::Ready(_)));
        assert_eq!(cache.len(), 2);
    }

    #[cfg(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    #[test]
    fn both_successor_cells_resolve_unlink_recompile_and_reset() {
        let mut cache = BlockCache::default();
        let source = key(0x1000);
        let fallthrough = key(0x1100);
        let taken = key(0x1200);
        assert!(matches!(cache.probe(source), BlockProbe::Interpret));
        let mut source_compilation =
            trivial_compilation(BlockSpan::new(source, 1, 1).expect("source span"));
        source_compilation.successors = [
            Some(LinkTarget {
                linear: fallthrough.linear,
                mode_key: source.mode_key,
            }),
            Some(LinkTarget {
                linear: taken.linear,
                mode_key: source.mode_key,
            }),
        ];
        let source_id = cache.install(&source_compilation).expect("source install");
        assert_eq!(cache.outbound[source_id.index()], [None, None]);

        install_trivial(&mut cache, taken, 1);
        assert!(cache.outbound[source_id.index()][0].is_none());
        assert!(cache.outbound[source_id.index()][1].is_some());
        install_trivial(&mut cache, fallthrough, 1);
        assert!(
            cache.outbound[source_id.index()]
                .iter()
                .all(Option::is_some)
        );

        let cells = cache.link_cells[source_id.index()].clone();
        assert_eq!(cache.invalidate_physical_range(taken.physical, 1), 1);
        assert!(cells[0].linked());
        assert!(!cells[1].linked());
        assert!(matches!(cache.probe(taken), BlockProbe::Interpret));
        let replacement = trivial_compilation(BlockSpan::new(taken, 1, 1).unwrap());
        cache.install(&replacement).expect("replacement install");
        assert!(cells[1].linked());

        cache.clear();
        assert!(!cells[0].linked());
        assert!(!cells[1].linked());
    }

    #[cfg(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    #[test]
    fn dynamic_ret_pic_keeps_two_targets_and_unlinks_replaced_or_retired_blocks() {
        let mut cache = BlockCache::default();
        let source = key(0x1000);
        let first = key(0x1100);
        let second = key(0x1200);
        let third = key(0x1300);
        let source_id = install_dynamic_trivial(&mut cache, source);
        install_trivial(&mut cache, first, 1);
        install_trivial(&mut cache, second, 1);
        install_trivial(&mut cache, third, 1);
        let cells = cache.link_cells[source_id.index()].clone();
        let site_cell = cells[0].address();

        assert!(cache.bind_dynamic_successor(
            site_cell,
            first.linear,
            first.linear,
            first.mode_key
        ));
        assert!(cache.bind_dynamic_successor(
            site_cell,
            second.linear,
            second.linear,
            second.mode_key
        ));
        assert!(cells[0].linked());
        assert!(cells[1].linked());
        assert_eq!(cells[0].target_eip.load(Ordering::Acquire), first.linear);
        assert_eq!(cells[1].target_eip.load(Ordering::Acquire), second.linear);
        let cell_addresses = [cells[0].address(), cells[1].address()];
        let old_bodies = [
            cells[0].body.load(Ordering::Acquire),
            cells[1].body.load(Ordering::Acquire),
        ];
        assert!(cache.compact_arena());
        assert_eq!([cells[0].address(), cells[1].address()], cell_addresses);
        assert_ne!(
            [
                cells[0].body.load(Ordering::Acquire),
                cells[1].body.load(Ordering::Acquire),
            ],
            old_bodies
        );
        assert_eq!(cells[0].target_eip.load(Ordering::Acquire), first.linear);
        assert_eq!(cells[1].target_eip.load(Ordering::Acquire), second.linear);

        assert!(cache.bind_dynamic_successor(
            site_cell,
            third.linear,
            third.linear,
            third.mode_key
        ));
        assert_eq!(cells[0].target_eip.load(Ordering::Acquire), third.linear);
        assert_eq!(cells[1].target_eip.load(Ordering::Acquire), second.linear);
        assert!(cells[0].linked());
        assert!(cells[1].linked());

        assert_eq!(cache.invalidate_physical_range(first.physical, 1), 1);
        assert!(cells[0].linked());
        assert!(cells[1].linked());
        assert_eq!(cache.invalidate_physical_range(second.physical, 1), 1);
        assert!(cells[0].linked());
        assert!(!cells[1].linked());
        assert_eq!(cache.invalidate_physical_range(third.physical, 1), 1);
        assert!(!cells[0].linked());

        assert_eq!(cache.invalidate_physical_range(source.physical, 1), 1);
        assert!(!cache.bind_dynamic_successor(
            site_cell,
            first.linear,
            first.linear,
            first.mode_key
        ));
        let stats = cache.take_stats();
        assert_eq!(stats.links, 3);
        assert_eq!(stats.unlinks, 3);
    }

    #[cfg(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    #[test]
    fn dynamic_ret_pic_requires_matching_x87_chain_top_and_kind() {
        let mut cache = BlockCache::default();
        let source = key(0x1000);
        let wrong_top = key(0x1100);
        let integer = key(0x1200);
        let matching_top = key(0x1300);

        assert!(matches!(cache.probe(source), BlockProbe::Interpret));
        assert!(matches!(cache.probe(source), BlockProbe::Compile));
        let mut source_compilation =
            trivial_compilation(BlockSpan::new(source, 1, 1).expect("source span"));
        source_compilation.has_x87 = true;
        source_compilation.x87_entry_top = 1;
        source_compilation.x87_exit_top = 3;
        source_compilation.dynamic_successor = true;
        let source_id = cache
            .install(&source_compilation)
            .expect("x87 source install");
        let site_cell = cache.link_cells[source_id.index()][0].address();

        assert!(matches!(cache.probe(wrong_top), BlockProbe::Interpret));
        assert!(matches!(cache.probe(wrong_top), BlockProbe::Compile));
        let mut wrong_top_compilation =
            trivial_compilation(BlockSpan::new(wrong_top, 1, 1).expect("wrong-top span"));
        wrong_top_compilation.has_x87 = true;
        wrong_top_compilation.x87_entry_top = 2;
        wrong_top_compilation.x87_exit_top = 2;
        cache
            .install(&wrong_top_compilation)
            .expect("wrong-top install");
        install_trivial(&mut cache, integer, 1);

        assert!(!cache.bind_dynamic_successor(
            site_cell,
            wrong_top.linear,
            wrong_top.linear,
            wrong_top.mode_key
        ));
        assert!(!cache.bind_dynamic_successor(
            site_cell,
            integer.linear,
            integer.linear,
            integer.mode_key
        ));

        assert!(matches!(cache.probe(matching_top), BlockProbe::Interpret));
        assert!(matches!(cache.probe(matching_top), BlockProbe::Compile));
        let mut matching_compilation =
            trivial_compilation(BlockSpan::new(matching_top, 1, 1).expect("matching span"));
        matching_compilation.has_x87 = true;
        matching_compilation.x87_entry_top = 3;
        matching_compilation.x87_exit_top = 3;
        cache
            .install(&matching_compilation)
            .expect("matching install");
        assert!(cache.bind_dynamic_successor(
            site_cell,
            matching_top.linear,
            matching_top.linear,
            matching_top.mode_key
        ));
        assert!(cache.link_cells[source_id.index()][0].linked());
    }

    #[cfg(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    #[test]
    fn dynamic_ret_pic_stays_unlinked_until_both_translation_epochs_are_current() {
        let mut cache = BlockCache::default();
        let source = key(0x1000);
        let target = key(0x1100);
        let source_id = install_dynamic_trivial(&mut cache, source);
        install_trivial(&mut cache, target, 1);
        let cell = cache.link_cells[source_id.index()][0].clone();
        let site_cell = cell.address();
        assert!(cache.bind_dynamic_successor(
            site_cell,
            target.linear,
            target.linear,
            target.mode_key
        ));
        assert!(cell.linked());

        cache.invalidate_translation();
        assert!(!cell.linked());
        assert_eq!(cell.target_eip.load(Ordering::Acquire), target.linear);
        assert!(!cache.bind_dynamic_successor(
            site_cell,
            target.linear,
            target.linear,
            target.mode_key
        ));

        cache
            .refresh_decode_residency(source, 1)
            .expect("source revalidation");
        assert!(!cache.bind_dynamic_successor(
            site_cell,
            target.linear,
            target.linear,
            target.mode_key
        ));
        cache
            .refresh_decode_residency(target, 1)
            .expect("target revalidation");
        assert!(cache.bind_dynamic_successor(
            site_cell,
            target.linear,
            target.linear,
            target.mode_key
        ));
        assert!(cell.linked());
    }

    #[cfg(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    #[test]
    fn translation_epoch_preserves_code_and_relinks_only_revalidated_blocks() {
        let mut cache = BlockCache::default();
        let source = key(0x1000);
        let target = key(0x1100);
        let rejected = key(0x1200);
        assert!(matches!(cache.probe(source), BlockProbe::Interpret));
        let mut source_compilation =
            trivial_compilation(BlockSpan::new(source, 1, 1).expect("source span"));
        source_compilation.successors[0] = Some(LinkTarget {
            linear: target.linear,
            mode_key: target.mode_key,
        });
        let source_id = cache.install(&source_compilation).expect("source install");
        install_trivial(&mut cache, target, 1);
        assert!(matches!(cache.probe(rejected), BlockProbe::Interpret));
        assert!(matches!(cache.probe(rejected), BlockProbe::Compile));
        cache.reject(rejected);

        let entry = cache.block(source_id).expect("source block").entry_ptr();
        let slots = cache.arena.as_ref().expect("arena").used_slots();
        let cells = cache.link_cells[source_id.index()].clone();
        assert!(cells[0].linked());

        cache.invalidate_translation();

        assert_eq!(cache.len(), 2);
        assert_eq!(cache.tracked_len(), 3);
        assert_eq!(cache.arena.as_ref().expect("arena").used_slots(), slots);
        assert_eq!(
            cache.block(source_id).expect("source block").entry_ptr(),
            entry
        );
        assert!(cache.range_hits_compiled_code(source.physical, 1));
        assert!(!cells[0].linked());
        assert!(cache.linear_blocks.is_empty());
        assert!(matches!(cache.probe(rejected), BlockProbe::Rejected));
        assert!(matches!(cache.probe(source), BlockProbe::Ready(id) if id == source_id));

        cache
            .refresh_decode_residency(source, 1)
            .expect("source revalidation");
        assert!(
            !cells[0].linked(),
            "an unvalidated target must stay unlinked"
        );
        let remapped_target =
            BlockKey::new(target.linear, target.physical + 0x1000, target.mode_key);
        assert!(matches!(
            cache.probe(remapped_target),
            BlockProbe::Interpret
        ));
        assert!(
            !cells[0].linked(),
            "a different physical key cannot satisfy the link"
        );

        assert!(matches!(cache.probe(target), BlockProbe::Ready(_)));
        cache
            .refresh_decode_residency(target, 1)
            .expect("same mapping revalidation");
        assert!(cells[0].linked());
        assert_eq!(cache.arena.as_ref().expect("arena").used_slots(), slots);
    }

    #[test]
    fn full_arena_compacts_only_when_it_can_reclaim_a_slot() {
        assert!(!BlockCache::arena_compaction_can_reclaim(0, 8));
        assert!(BlockCache::arena_compaction_can_reclaim(7, 8));
        assert!(!BlockCache::arena_compaction_can_reclaim(8, 8));
    }

    #[cfg(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    #[test]
    fn invalidated_metadata_slot_reuse_rejects_its_stale_generation() {
        let mut cache = BlockCache::default();
        let source = key(0x1400);
        let missing = key(0x1500);
        assert!(matches!(cache.probe(source), BlockProbe::Interpret));
        assert!(matches!(cache.probe(source), BlockProbe::Compile));
        let mut compilation =
            trivial_compilation(BlockSpan::new(source, 1, 1).expect("source span"));
        compilation.successors[0] = Some(LinkTarget {
            linear: missing.linear,
            mode_key: missing.mode_key,
        });
        let stale_id = cache.install(&compilation).expect("source install");
        let stale_block = cache.block(stale_id).expect("source block");
        assert!(
            cache
                .waiting
                .values()
                .flatten()
                .any(|source| source.block == stale_id)
        );

        assert_eq!(cache.invalidate_physical_range(source.physical, 1), 1);
        assert!(cache.block(stale_id).is_none());
        assert_eq!(cache.blocks[stale_id.index()].entry, 0);
        assert_eq!(cache.blocks[stale_id.index()].body_entry, 0);
        assert!(
            !cache
                .waiting
                .values()
                .flatten()
                .any(|source| source.block == stale_id)
        );

        let replacement_id = install_trivial(&mut cache, key(0x1600), 1);
        assert_eq!(replacement_id.index(), stale_id.index());
        assert_ne!(replacement_id, stale_id);
        assert_eq!(cache.blocks.len(), 1);
        assert!(cache.block(stale_block.id()).is_none());
        assert_eq!(
            cache.block(replacement_id).expect("replacement").id(),
            replacement_id
        );
    }

    #[cfg(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    #[test]
    fn linked_blocks_relocate_without_replacing_link_cells() {
        let mut cache = BlockCache::default();
        let source = key(0x1700);
        let target = key(0x1800);
        let dead = key(0x1900);
        assert!(matches!(cache.probe(source), BlockProbe::Interpret));
        assert!(matches!(cache.probe(source), BlockProbe::Compile));
        let mut source_compilation =
            trivial_compilation(BlockSpan::new(source, 1, 1).expect("source span"));
        source_compilation.successors[0] = Some(LinkTarget {
            linear: target.linear,
            mode_key: target.mode_key,
        });
        let source_cell_address = source_compilation.link_cells[0].address();
        source_compilation.code = vec![0x48, 0xb8];
        source_compilation
            .code
            .extend_from_slice(&(source_cell_address as u64).to_le_bytes());
        source_compilation.code.extend_from_slice(&[0xff, 0x20]);
        let source_id = cache.install(&source_compilation).expect("source install");
        let target_id = install_trivial(&mut cache, target, 1);
        let dead_id = install_trivial(&mut cache, dead, 1);
        let source_cell = cache.link_cells[source_id.index()][0].clone();
        let old_source_entry = cache.block(source_id).expect("source").entry;
        let old_target_body = cache.block(target_id).expect("target").body_ptr();
        let link_epochs = cache.block_link_epochs.clone();
        assert_eq!(source_cell.body.load(Ordering::Acquire), old_target_body);
        let old_entry: extern "C" fn() =
            unsafe { std::mem::transmute(cache.block(source_id).expect("source").entry_ptr()) };
        old_entry();
        assert_eq!(cache.invalidate_physical_range(dead.physical, 1), 1);

        assert!(cache.compact_arena());

        let relocated_source = cache.block(source_id).expect("relocated source");
        let relocated_target = cache.block(target_id).expect("relocated target");
        assert_ne!(relocated_source.entry, old_source_entry);
        assert_ne!(relocated_target.body_ptr(), old_target_body);
        assert_eq!(source_cell.address(), source_cell_address);
        assert_eq!(
            source_cell.body.load(Ordering::Acquire),
            relocated_target.body_ptr()
        );
        assert_eq!(cache.block_link_epochs, link_epochs);
        assert!(cache.range_hits_compiled_code(source.physical, 1));
        assert!(cache.range_hits_compiled_code(target.physical, 1));
        assert_eq!(cache.arena.as_ref().expect("arena").used_slots(), 2);
        let entry: extern "C" fn() = unsafe { std::mem::transmute(relocated_source.entry_ptr()) };
        entry();

        let reused_id = install_trivial(&mut cache, key(0x1a00), 1);
        assert_eq!(reused_id.index(), dead_id.index());
        assert_eq!(cache.arena.as_ref().expect("arena").used_slots(), 3);
        let stats = cache.take_stats();
        assert_eq!(stats.arena_compactions, 1);
        assert_eq!(stats.arena_compaction_live_blocks, 2);
        assert_eq!(stats.arena_compaction_bytes, 13);
        assert_eq!(stats.arena_compaction_failures, 0);
        assert_eq!(stats.cache_resets, 0);
    }

    #[cfg(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    #[test]
    fn unresolved_waiting_edge_survives_arena_compaction() {
        let mut cache = BlockCache::default();
        let source = key(0x1b00);
        let target = key(0x1c00);
        assert!(matches!(cache.probe(source), BlockProbe::Interpret));
        assert!(matches!(cache.probe(source), BlockProbe::Compile));
        let mut source_compilation =
            trivial_compilation(BlockSpan::new(source, 1, 1).expect("source span"));
        let target_key = LinkTarget {
            linear: target.linear,
            mode_key: target.mode_key,
        };
        source_compilation.successors[0] = Some(target_key);
        let source_id = cache.install(&source_compilation).expect("source install");
        let waiting = cache
            .waiting
            .get(&target_key)
            .cloned()
            .expect("waiting edge");

        assert!(cache.compact_arena());
        assert_eq!(cache.waiting.get(&target_key), Some(&waiting));
        let target_id = install_trivial(&mut cache, target, 1);
        assert_eq!(cache.outbound[source_id.index()][0], Some(target_id));
        assert_eq!(
            cache.link_cells[source_id.index()][0]
                .body
                .load(Ordering::Acquire),
            cache.block(target_id).expect("target").body_ptr()
        );
    }

    #[cfg(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    #[test]
    fn translation_invalid_blocks_stay_invisible_through_compaction() {
        let mut cache = BlockCache::default();
        let source = key(0x1d00);
        let target = key(0x1e00);
        assert!(matches!(cache.probe(source), BlockProbe::Interpret));
        assert!(matches!(cache.probe(source), BlockProbe::Compile));
        let mut source_compilation =
            trivial_compilation(BlockSpan::new(source, 1, 1).expect("source span"));
        source_compilation.successors[0] = Some(LinkTarget {
            linear: target.linear,
            mode_key: target.mode_key,
        });
        let source_id = cache.install(&source_compilation).expect("source install");
        let target_id = install_trivial(&mut cache, target, 1);
        let source_cell = cache.link_cells[source_id.index()][0].clone();
        assert!(source_cell.linked());

        cache.invalidate_translation();
        let link_epochs = cache.block_link_epochs.clone();
        assert!(cache.linear_blocks.is_empty());
        assert!(!source_cell.linked());
        assert!(cache.compact_arena());

        assert_eq!(cache.block_link_epochs, link_epochs);
        assert!(cache.linear_blocks.is_empty());
        assert!(cache.waiting.is_empty());
        assert!(!source_cell.linked());
        cache
            .refresh_decode_residency(source, 1)
            .expect("source revalidation");
        assert!(!source_cell.linked());
        cache
            .refresh_decode_residency(target, 1)
            .expect("target revalidation");
        assert!(source_cell.linked());
        assert_eq!(cache.outbound[source_id.index()][0], Some(target_id));
    }

    #[cfg(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    #[test]
    fn translation_flush_preserves_blocks_but_coarse_map_flushes_drop_them() {
        let mut cpu = CpuGsw::default();
        let block_key = key(0x1000);
        install_trivial(&mut cpu.jit_direct, block_key, 1);
        let entry = cpu.jit_direct.blocks[0].entry_ptr();

        cpu.flush_tlb_and_code_caches();

        assert_eq!(cpu.jit_direct.len(), 1);
        assert_eq!(cpu.jit_direct.blocks[0].entry_ptr(), entry);
        assert!(matches!(
            cpu.jit_direct.probe(block_key),
            BlockProbe::Ready(_)
        ));

        cpu.note_a20_changed();

        assert_eq!(cpu.jit_direct.len(), 0);
        assert!(cpu.jit_direct.arena.is_none());
        assert!(matches!(
            cpu.jit_direct.probe(block_key),
            BlockProbe::Interpret
        ));

        assert!(matches!(
            cpu.jit_direct.probe(block_key),
            BlockProbe::Compile
        ));
        let span = BlockSpan::new(block_key, 1, 1).expect("replacement span");
        cpu.jit_direct
            .install(&trivial_compilation(span))
            .expect("replacement install");
        cpu.note_direct_map_changed();
        assert_eq!(cpu.jit_direct.len(), 0);
        assert!(cpu.jit_direct.arena.is_none());
    }

    #[cfg(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    #[test]
    fn physical_invalidation_removes_overlap_and_preserves_adjacent_blocks() {
        let mut cache = BlockCache::default();
        let overlap = BlockKey::new(0x1000, 0x20_020, 7);
        let adjacent = BlockKey::new(0x1100, 0x20_040, 7);
        install_trivial(&mut cache, overlap, 16);
        install_trivial(&mut cache, adjacent, 16);

        assert_eq!(cache.invalidate_physical_range(0x20_02f, 1), 1);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.blocks.len(), 2, "stable block IDs must not compact");
        assert_eq!(cache.block_active, [false, true]);
        assert!(cache.arena.is_some(), "sealed pages stay allocated");
        assert!(matches!(cache.probe(overlap), BlockProbe::Interpret));
        assert!(matches!(cache.probe(adjacent), BlockProbe::Ready(_)));

        cache.clear();
        assert_eq!(cache.len(), 0);
        assert!(cache.blocks.is_empty());
        assert!(cache.block_active.is_empty());
        assert!(cache.physical_keys.is_empty());
        assert!(cache.arena.is_none());
    }

    #[cfg(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    #[test]
    fn physical_invalidation_refcounts_shared_watch_chunks() {
        let mut cache = BlockCache::default();
        let first = BlockKey::new(0x1000, 0x20_020, 7);
        let second = BlockKey::new(0x2000, 0x20_028, 7);
        install_trivial(&mut cache, first, 8);
        install_trivial(&mut cache, second, 8);
        assert!(cache.range_hits_compiled_code(0x20_020, 16));

        assert_eq!(cache.invalidate_physical_range(first.physical, 1), 1);
        assert!(
            cache.range_hits_compiled_code(first.physical, 1),
            "the neighboring block still owns the shared 16-byte watch"
        );

        assert_eq!(cache.invalidate_physical_range(second.physical, 1), 1);
        assert!(!cache.range_hits_compiled_code(first.physical, 16));
    }

    #[cfg(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    #[test]
    fn invalidated_code_chunk_can_be_reused_and_recompiled() {
        let mut cache = BlockCache::default();
        let old = BlockKey::new(0x1000, 0x21_020, 7);
        install_trivial(&mut cache, old, 8);
        assert!(cache.range_hits_compiled_code(old.physical, 1));

        assert_eq!(cache.invalidate_physical_range(old.physical, 1), 1);
        assert!(!cache.range_hits_compiled_code(old.physical, 1));

        install_trivial(&mut cache, old, 8);
        assert!(cache.range_hits_compiled_code(old.physical, 1));
        assert_eq!(cache.invalidate_physical_range(old.physical, 1), 1);
        assert!(!cache.range_hits_compiled_code(old.physical, 1));
    }

    #[cfg(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    #[test]
    fn physical_invalidation_removes_every_linear_alias_without_stale_ready_hits() {
        let mut cache = BlockCache::default();
        let first = BlockKey::new(0x1000, 0x30_080, 7);
        let alias = BlockKey::new(0x5000, 0x30_080, 9);
        install_trivial(&mut cache, first, 16);
        install_trivial(&mut cache, alias, 16);

        assert_eq!(cache.invalidate_physical_range(0x30_084, 2), 2);
        assert_eq!(cache.len(), 0);
        assert!(matches!(cache.probe(first), BlockProbe::Interpret));
        assert!(matches!(cache.probe(alias), BlockProbe::Interpret));
    }

    #[test]
    fn physical_invalidation_forgets_seen_and_rejected_entries_only_on_overlap() {
        let mut cache = BlockCache::default();
        let seen = BlockKey::new(0x1000, 0x40_010, 7);
        let rejected = BlockKey::new(0x2000, 0x40_010, 9);
        let adjacent = BlockKey::new(0x3000, 0x40_020, 7);
        assert!(matches!(cache.probe(seen), BlockProbe::Interpret));
        assert!(matches!(cache.probe(rejected), BlockProbe::Interpret));
        assert!(matches!(cache.probe(rejected), BlockProbe::Compile));
        cache.reject(rejected);
        assert!(matches!(cache.probe(adjacent), BlockProbe::Interpret));

        assert_eq!(cache.invalidate_physical_range(0x40_010, 1), 2);
        assert!(matches!(cache.probe(seen), BlockProbe::Interpret));
        assert!(matches!(cache.probe(rejected), BlockProbe::Interpret));
        assert!(matches!(cache.probe(adjacent), BlockProbe::Compile));
    }

    #[test]
    fn physical_invalidation_checks_both_pages_of_a_cross_page_write() {
        let mut cache = BlockCache::default();
        let low = BlockKey::new(0x1000, 0x4fff, 7);
        let high = BlockKey::new(0x2000, 0x5000, 7);
        assert!(matches!(cache.probe(low), BlockProbe::Interpret));
        assert!(matches!(cache.probe(high), BlockProbe::Interpret));

        assert_eq!(cache.invalidate_physical_range(0x4fff, 2), 2);
        assert!(matches!(cache.probe(low), BlockProbe::Interpret));
        assert!(matches!(cache.probe(high), BlockProbe::Interpret));
    }

    #[cfg(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    #[test]
    fn cpu_code_write_uses_selective_direct_invalidation() {
        let mut cpu = CpuGsw::default();
        let overlap = BlockKey::new(0x1000, 0x60_010, 7);
        let adjacent = BlockKey::new(0x2000, 0x60_030, 7);
        install_trivial(&mut cpu.jit_direct, overlap, 16);
        install_trivial(&mut cpu.jit_direct, adjacent, 16);
        cpu.decode_cache.mark_code_range(overlap.physical, 1);
        cpu.jit_direct.mark_code_range(overlap.physical, 1);
        cpu.decode_cache.invalidate_and_clear_code_marks();
        assert!(!cpu.decode_cache.range_hits_code(overlap.physical, 1));

        cpu.note_code_write(overlap.physical, 1);

        assert_eq!(cpu.jit_direct.len(), 1);
        assert!(matches!(
            cpu.jit_direct.probe(overlap),
            BlockProbe::Interpret
        ));
        assert!(matches!(
            cpu.jit_direct.probe(adjacent),
            BlockProbe::Ready(_)
        ));
    }

    #[cfg(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    #[test]
    fn ranged_device_write_preserves_unrelated_blocks_and_unlinks_overlap() {
        let mut cpu = CpuGsw::default();
        let source = BlockKey::new(0x1000, 0x60_000, 7);
        let overlap = BlockKey::new(0x2000, 0x61_000, 7);
        let unrelated = BlockKey::new(0x3000, 0x62_000, 7);

        let mut source_compilation =
            trivial_compilation(BlockSpan::new(source, 1, 1).expect("source span"));
        source_compilation.successors[0] = Some(LinkTarget {
            linear: overlap.linear,
            mode_key: overlap.mode_key,
        });
        assert!(matches!(
            cpu.jit_direct.probe(source),
            BlockProbe::Interpret
        ));
        assert!(matches!(cpu.jit_direct.probe(source), BlockProbe::Compile));
        let source_id = cpu
            .jit_direct
            .install(&source_compilation)
            .expect("source installs");
        install_trivial(&mut cpu.jit_direct, overlap, 16);
        install_trivial(&mut cpu.jit_direct, unrelated, 16);
        let source_cell = cpu.jit_direct.link_cells[source_id.index()][0].clone();
        assert!(source_cell.linked());

        cpu.note_device_memory_write_range(0x70_000, 512);
        assert_eq!(cpu.jit_direct.len(), 3);
        assert!(source_cell.linked());

        cpu.note_device_memory_write_range(overlap.physical + 4, 1);
        assert_eq!(cpu.jit_direct.len(), 2);
        assert!(!source_cell.linked());
        assert!(matches!(
            cpu.jit_direct.probe(overlap),
            BlockProbe::Interpret
        ));
        assert!(matches!(cpu.jit_direct.probe(source), BlockProbe::Ready(_)));
        assert!(matches!(
            cpu.jit_direct.probe(unrelated),
            BlockProbe::Ready(_)
        ));

        let stats = cpu.jit_direct.take_stats();
        assert_eq!(stats.cache_resets, 0);
        assert_eq!(stats.unlinks, 1);
        assert_eq!(cpu.perf.device_write_ranges, 2);
        assert_eq!(cpu.perf.device_write_bytes, 513);
        assert_eq!(cpu.perf.device_write_code_hits, 1);
        assert_eq!(cpu.perf.device_write_coarse_resets, 0);
    }
}
