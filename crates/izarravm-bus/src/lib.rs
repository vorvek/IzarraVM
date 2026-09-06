// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

#![forbid(unsafe_code)]

use std::collections::VecDeque;

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BusError {
    #[error("memory size must be greater than zero")]
    EmptyMemory,
    #[error("memory access {address:#x}..{end:#x} is outside {len:#x} bytes")]
    MemoryOutOfBounds {
        address: usize,
        end: usize,
        len: usize,
    },
    #[error("unmapped physical memory access at {address:#010x}")]
    UnmappedMemory { address: u32 },
    #[error("unsupported I/O port {port:#06x}")]
    UnsupportedPort { port: u16 },
    #[error("bus value width mismatch for {width:?}")]
    WidthMismatch { width: BusWidth },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Memory {
    data: PageAlignedBytes,
}

/// Guest RAM's backing bytes, windowed to host-page (4096) alignment inside a deliberately
/// over-allocated `Vec` — `align_offset` finds the boundary and every accessor sees only the
/// aligned window, so the whole scheme stays inside this crate's `forbid(unsafe_code)`.
///
/// The alignment is a PERFORMANCE contract, not a correctness one: the CPU's one-lookup store
/// table (`dev_docs/2026-08-07-one-lookup-store-design.md` D7) steals the low bits of each
/// per-page host bias for its poison/tag encoding, which is only possible when every
/// `DirectPage::ptr` handed out of this buffer is 4096-aligned; a misaligned backing silently
/// degrades every page to the slow store path.
///
/// The window never moves: the `Vec` is sized once and never grown, so its allocation — and
/// therefore `start` — is stable for the buffer's life, which is the same write-once contract
/// the CPU's fast-map bias tables already lean on. `Clone` re-derives the offset in the fresh
/// allocation rather than copying it.
///
/// Public because every buffer that backs a `DirectPage` needs it — guest RAM here, and both
/// VGA buffers (`vram` for the Mode X path, `mode13_linear` for chained 13h) in
/// izarravm-video, which would otherwise silently degrade doom's aperture stores.
pub struct PageAlignedBytes {
    raw: Vec<u8>,
    start: usize,
    len: usize,
}

impl PageAlignedBytes {
    const ALIGN: usize = 4096;

    pub fn zeroed(len: usize) -> Self {
        let raw = vec![0u8; len + Self::ALIGN];
        let start = raw.as_ptr().align_offset(Self::ALIGN);
        // `align_offset` is PERMITTED to return usize::MAX ("not possible to align"). No real
        // allocator does that for a byte buffer, but this type's whole contract is "degrade,
        // never fail" — a misaligned window is only a slow store path, while an out-of-range
        // one would panic on first deref — so fall back to offset 0 rather than lean on the
        // allocator's goodwill.
        let start = if start >= Self::ALIGN { 0 } else { start };
        Self { raw, start, len }
    }
}

impl Clone for PageAlignedBytes {
    fn clone(&self) -> Self {
        let mut copy = Self::zeroed(self.len);
        copy.copy_from_slice(self);
        copy
    }
}

impl Default for PageAlignedBytes {
    fn default() -> Self {
        Self::zeroed(0)
    }
}

impl From<Vec<u8>> for PageAlignedBytes {
    /// Re-home plain bytes into an aligned window — the copy is the cost of the alignment
    /// guarantee, so this is for construction-time use (test harnesses, one-shot loads), not
    /// steady-state paths.
    fn from(bytes: Vec<u8>) -> Self {
        let mut buf = Self::zeroed(bytes.len());
        buf.copy_from_slice(&bytes);
        buf
    }
}

impl PartialEq for PageAlignedBytes {
    fn eq(&self, other: &Self) -> bool {
        self[..] == other[..]
    }
}

impl Eq for PageAlignedBytes {}

impl std::fmt::Debug for PageAlignedBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PageAlignedBytes")
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

impl std::ops::Deref for PageAlignedBytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.raw[self.start..self.start + self.len]
    }
}

impl std::ops::DerefMut for PageAlignedBytes {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.raw[self.start..self.start + self.len]
    }
}

impl Memory {
    pub fn from_mib(memory_mib: u16) -> Result<Self, BusError> {
        let bytes = usize::from(memory_mib) * 1024 * 1024;
        Self::new(bytes)
    }

    pub fn new(size: usize) -> Result<Self, BusError> {
        if size == 0 {
            return Err(BusError::EmptyMemory);
        }

        Ok(Self {
            data: PageAlignedBytes::zeroed(size),
        })
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn read_u8(&self, address: usize) -> Result<u8, BusError> {
        self.data
            .get(address)
            .copied()
            .ok_or_else(|| self.out_of_bounds(address, 1))
    }

    pub fn read_u16(&self, address: usize) -> Result<u16, BusError> {
        Ok(u16::from_le_bytes([
            self.read_u8(address)?,
            self.read_u8(address + 1)?,
        ]))
    }

    pub fn read_u32(&self, address: usize) -> Result<u32, BusError> {
        Ok(u32::from_le_bytes([
            self.read_u8(address)?,
            self.read_u8(address + 1)?,
            self.read_u8(address + 2)?,
            self.read_u8(address + 3)?,
        ]))
    }

    pub fn write_u8(&mut self, address: usize, value: u8) -> Result<(), BusError> {
        let len = self.data.len();
        let slot = self
            .data
            .get_mut(address)
            .ok_or(BusError::MemoryOutOfBounds {
                address,
                end: address.saturating_add(1),
                len,
            })?;
        *slot = value;
        Ok(())
    }

    pub fn write_u16(&mut self, address: usize, value: u16) -> Result<(), BusError> {
        for (offset, byte) in value.to_le_bytes().into_iter().enumerate() {
            self.write_u8(address + offset, byte)?;
        }
        Ok(())
    }

    pub fn write_u32(&mut self, address: usize, value: u32) -> Result<(), BusError> {
        for (offset, byte) in value.to_le_bytes().into_iter().enumerate() {
            self.write_u8(address + offset, byte)?;
        }
        Ok(())
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.data
    }

    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.data.as_mut_ptr()
    }

    fn out_of_bounds(&self, address: usize, width: usize) -> BusError {
        BusError::MemoryOutOfBounds {
            address,
            end: address.saturating_add(width),
            len: self.data.len(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusWidth {
    Byte,
    Word,
    Dword,
}

impl BusWidth {
    pub const fn bytes(self) -> u32 {
        match self {
            Self::Byte => 1,
            Self::Word => 2,
            Self::Dword => 4,
        }
    }

    /// True when `address` is not naturally aligned for this width -- the single spelling of a
    /// predicate that used to be written four ways (`FastMap::lookup_access`'s two `matches!`
    /// clauses, `CpuGsw::direct_access_page_local`'s `match`, and `MachineBus`'s free
    /// `should_split`).
    ///
    /// NOT the `#AC` question. That one is asked of the EFFECTIVE address at CPL 3 with CR0.AM and
    /// EFLAGS.AC set (`izarravm-cpu`'s `check_alignment`) and raises a fault; this one is asked of
    /// a linear or physical address and decides only whether the bus splits the access into bytes.
    ///
    /// `bytes()` is 1/2/4, so `address & (bytes - 1)` reproduces all three arms exactly, including
    /// `Byte => false`.
    ///
    /// The JIT emitter does NOT call this. It works in `MemoryWidth`, which carries two widths
    /// this enum does not have, and whose `alignment_bytes()` is deliberately below their size
    /// because the interpreter issues an m64 as two independently 4-aligned dword transactions.
    /// Its mirror of this predicate is `MemoryWidth::alignment_mask`. The two are kept apart on
    /// purpose: folding Qword and Tbyte into this one would be a category error.
    #[inline]
    pub const fn misaligned_at(self, address: u32) -> bool {
        address & (self.bytes() - 1) != 0
    }

    pub const fn byte_enable(self, address: u32) -> u8 {
        match self {
            Self::Byte => 1 << (address & 0x3),
            Self::Word => 0b0011 << (address & 0x2),
            Self::Dword => 0b1111,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusAccessKind {
    InstructionPrefetch,
    DataRead,
    DataWrite,
    PageWalkRead,
    PageWalkWrite,
    IoRead,
    IoWrite,
    InterruptAcknowledge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectMemoryRead {
    pub value: u32,
    pub direct: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectMemoryWrite {
    pub direct: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectPage {
    pub physical_page: u32,
    pub ptr: *mut u8,
    pub len: usize,
    pub writable: bool,
    /// Generation of the host mapping that produced `ptr`.
    pub mapping_epoch: u64,
}

/// Writes completed through a native VGA aperture fast path during one CPU block chain.
/// The bus applies dirty tracking and timing once at the chain boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NativeVgaWrites {
    pub dirty_pages: u16,
    pub byte_writes: u64,
    pub word_writes: u64,
    pub dword_writes: u64,
}

impl NativeVgaWrites {
    pub const fn is_empty(self) -> bool {
        self.byte_writes == 0 && self.word_writes == 0 && self.dword_writes == 0
    }
}

/// Compatibility name retained while the direct CPU backend still labels the
/// VGA aperture page kind as Mode 13h.
pub type NativeMode13Writes = NativeVgaWrites;

const fn compiled_width_index(width: BusWidth) -> usize {
    match width {
        BusWidth::Byte => 0,
        BusWidth::Word => 1,
        BusWidth::Dword => 2,
    }
}

/// Aggregate effects produced while the CPU remains in compiled execution.
///
/// RAM reads and writes share one count because the fixed direct-RAM timing is
/// identical for both directions. VGA writes retain their dirty-page mask so
/// the bus can publish display changes when the compiled window closes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompiledBusDelta {
    instruction_fetches: u64,
    ram_accesses: [u64; 3],
    vga_reads: [u64; 3],
    vga_writes: NativeVgaWrites,
}

impl CompiledBusDelta {
    pub fn add_instruction_fetches(&mut self, count: u64) {
        self.instruction_fetches = self.instruction_fetches.saturating_add(count);
    }

    pub fn add_ram_accesses(&mut self, width: BusWidth, count: u64) {
        let slot = &mut self.ram_accesses[compiled_width_index(width)];
        *slot = slot.saturating_add(count);
    }

    pub fn add_vga_reads(&mut self, width: BusWidth, count: u64) {
        let slot = &mut self.vga_reads[compiled_width_index(width)];
        *slot = slot.saturating_add(count);
    }

    pub fn add_vga_writes(&mut self, writes: NativeVgaWrites) {
        self.vga_writes.dirty_pages |= writes.dirty_pages;
        self.vga_writes.byte_writes = self
            .vga_writes
            .byte_writes
            .saturating_add(writes.byte_writes);
        self.vga_writes.word_writes = self
            .vga_writes
            .word_writes
            .saturating_add(writes.word_writes);
        self.vga_writes.dword_writes = self
            .vga_writes
            .dword_writes
            .saturating_add(writes.dword_writes);
    }

    pub const fn instruction_fetches(&self) -> u64 {
        self.instruction_fetches
    }

    pub const fn ram_accesses(&self, width: BusWidth) -> u64 {
        self.ram_accesses[compiled_width_index(width)]
    }

    pub const fn vga_reads(&self, width: BusWidth) -> u64 {
        self.vga_reads[compiled_width_index(width)]
    }

    pub const fn vga_writes(&self) -> NativeVgaWrites {
        self.vga_writes
    }
}

/// Stable bus state certified for one compiled-execution residency window.
///
/// The value is neither `Copy` nor `Clone`. Passing it to
/// `CpuBus::finish_compiled_window` prevents a second finish in safe Rust. A bus
/// returns a window only when aggregate accounting is exact and direct mappings
/// remain valid for its `mapping_epoch` until a side exit.
#[must_use = "a compiled bus window must be finished exactly once"]
#[derive(Debug, PartialEq, Eq)]
pub struct CompiledBusWindow {
    mapping_epoch: u64,
    tracing_mode: TracingMode,
    fetch_raw_clocks: u64,
    ram_raw_clocks: [u64; 3],
    vga_raw_clocks: [u64; 3],
    batch_raw_clocks: u64,
    bus_scale_remainder: u64,
    bus_scale_numerator: u32,
    bus_scale_denominator: u32,
}

impl CompiledBusWindow {
    /// Build a window after the bus has checked mapping and timing stability.
    /// Full and count tracing require individual observations, so only off mode
    /// supports aggregate effects.
    #[allow(clippy::too_many_arguments)]
    pub fn certify(
        mapping_epoch: u64,
        tracing_mode: TracingMode,
        fetch_raw_clocks: u64,
        ram_raw_clocks: [u64; 3],
        vga_raw_clocks: [u64; 3],
        batch_raw_clocks: u64,
        bus_scale_remainder: u64,
        bus_scale_numerator: u32,
        bus_scale_denominator: u32,
    ) -> Option<Self> {
        if tracing_mode != TracingMode::Off || bus_scale_denominator == 0 {
            return None;
        }
        Some(Self {
            mapping_epoch,
            tracing_mode,
            fetch_raw_clocks,
            ram_raw_clocks,
            vga_raw_clocks,
            batch_raw_clocks,
            bus_scale_remainder,
            bus_scale_numerator,
            bus_scale_denominator,
        })
    }

    pub const fn mapping_epoch(&self) -> u64 {
        self.mapping_epoch
    }

    pub const fn tracing_mode(&self) -> TracingMode {
        self.tracing_mode
    }

    pub const fn fetch_raw_clocks(&self) -> u64 {
        self.fetch_raw_clocks
    }

    pub const fn ram_raw_clocks(&self, width: BusWidth) -> u64 {
        self.ram_raw_clocks[compiled_width_index(width)]
    }

    pub const fn vga_raw_clocks(&self, width: BusWidth) -> u64 {
        self.vga_raw_clocks[compiled_width_index(width)]
    }

    /// Raw bus clocks accumulated by this CPU batch before compiled entry.
    pub const fn batch_raw_clocks(&self) -> u64 {
        self.batch_raw_clocks
    }

    pub const fn bus_scale_remainder(&self) -> u64 {
        self.bus_scale_remainder
    }

    pub const fn bus_scale_numerator(&self) -> u32 {
        self.bus_scale_numerator
    }

    pub const fn bus_scale_denominator(&self) -> u32 {
        self.bus_scale_denominator
    }

    /// Raw clocks represented by `delta` under this window's fixed costs.
    pub fn delta_raw_clocks(&self, delta: &CompiledBusDelta) -> u64 {
        let mut clocks = self
            .fetch_raw_clocks
            .saturating_mul(delta.instruction_fetches);
        for width in [BusWidth::Byte, BusWidth::Word, BusWidth::Dword] {
            clocks = clocks.saturating_add(
                self.ram_raw_clocks(width)
                    .saturating_mul(delta.ram_accesses(width)),
            );
            clocks = clocks.saturating_add(
                self.vga_raw_clocks(width)
                    .saturating_mul(delta.vga_reads(width)),
            );
        }
        let writes = delta.vga_writes;
        clocks = clocks.saturating_add(
            self.vga_raw_clocks(BusWidth::Byte)
                .saturating_mul(writes.byte_writes),
        );
        clocks = clocks.saturating_add(
            self.vga_raw_clocks(BusWidth::Word)
                .saturating_mul(writes.word_writes),
        );
        clocks.saturating_add(
            self.vga_raw_clocks(BusWidth::Dword)
                .saturating_mul(writes.dword_writes),
        )
    }

    /// Exact scaled bus total after an additional raw-clock charge.
    pub fn projected_scaled_bus_clocks(&self, additional_raw: u64) -> Option<u64> {
        self.batch_raw_clocks
            .checked_add(additional_raw)?
            .checked_mul(u64::from(self.bus_scale_numerator))?
            .checked_add(self.bus_scale_remainder)
            .map(|scaled| scaled / u64::from(self.bus_scale_denominator))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusState {
    T1,
    T2,
    Tw,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusCycle {
    pub kind: BusAccessKind,
    pub address: u32,
    pub width: BusWidth,
    pub byte_enable: u8,
    pub wait_states: u8,
    pub states: Vec<BusState>,
    pub clocks: u32,
}

impl BusCycle {
    pub fn new(kind: BusAccessKind, address: u32, width: BusWidth, wait_states: u8) -> Self {
        let mut states = vec![BusState::T1, BusState::T2];
        states.extend(std::iter::repeat_n(BusState::Tw, usize::from(wait_states)));
        Self {
            kind,
            address,
            width,
            byte_enable: width.byte_enable(address),
            wait_states,
            clocks: states.len() as u32,
            states,
        }
    }

    /// The clock cost of a cycle without allocating its per-state detail vector.
    /// Matches `BusCycle::new(...).clocks` exactly: T1 + T2 plus the wait states.
    #[inline]
    pub const fn clocks_for(_width: BusWidth, wait_states: u8) -> u32 {
        2 + wait_states as u32
    }
}

/// How much of each bus cycle a `BusTrace` retains. Timing accounting is
/// independent of this: `elapsed_clocks` always advances by every pushed
/// cycle's clock count, so `Off` preserves the clock pacing the GUI and the
/// device scheduler depend on while eliding the per-cycle allocation that the
/// hot fetch/decode path would otherwise pay on every byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TracingMode {
    /// Retain `BusCycle` detail (kind, address, states) up to the capacity.
    #[default]
    Full,
    /// Record no per-cycle detail, but keep a count of recorded cycles.
    Counts,
    /// Record nothing but the running clock total.
    Off,
}

/// Default cap on the number of retained bus cycles. A run of many hundred
/// million cycles would otherwise grow the trace toward gigabytes and run the
/// host out of memory. Holding the most recent few million keeps the trace
/// bounded to tens of megabytes while still covering any halting test ROM in
/// full (their total bus traffic stays well under this) and leaving recent
/// history intact for the long runs that drive the bound.
pub const DEFAULT_BUS_TRACE_CAPACITY: usize = 4_000_000;

/// A bounded record of recent bus cycles plus the running clock total.
///
/// `push` keeps the most recent `capacity` cycles and drops the oldest once the
/// cap is reached. `elapsed_clocks` always reflects every pushed cycle, evicted
/// or not, so timing accounting stays exact no matter how long a run goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusTrace {
    cycles: VecDeque<BusCycle>,
    capacity: usize,
    elapsed_clocks: u64,
    access_count: u64,
    mode: TracingMode,
}

impl Default for BusTrace {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_BUS_TRACE_CAPACITY)
    }
}

impl BusTrace {
    /// A trace that retains at most `capacity` recent cycles. A capacity of zero
    /// keeps no cycle history but still totals `elapsed_clocks`.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            cycles: VecDeque::new(),
            capacity,
            elapsed_clocks: 0,
            access_count: 0,
            mode: TracingMode::Full,
        }
    }

    pub fn push(&mut self, cycle: BusCycle) {
        self.elapsed_clocks += u64::from(cycle.clocks);
        if self.mode != TracingMode::Off {
            self.access_count += 1;
        }
        if self.mode != TracingMode::Full {
            return;
        }
        if self.capacity == 0 {
            return;
        }
        if self.cycles.len() == self.capacity {
            self.cycles.pop_front();
        }
        self.cycles.push_back(cycle);
    }

    /// Record a cycle by its timing parameters, allocating the per-cycle detail
    /// only when the trace is in `Full` mode. In `Counts`/`Off` this bumps the
    /// clock total (and the access count) without touching the heap, which is the
    /// fast path the interpreter fetch loop takes.
    #[inline]
    pub fn record(&mut self, kind: BusAccessKind, address: u32, width: BusWidth, wait_states: u8) {
        // Off-mode (the interpreter's hot path) only bumps the clock total, so keep
        // that a tiny inlinable body and push the count/detail bookkeeping into a
        // cold helper. Folding this into the per-access caller avoids a cross-crate
        // call on every fetched byte and memory access.
        let clocks = BusCycle::clocks_for(width, wait_states);
        self.elapsed_clocks += u64::from(clocks);
        if self.mode != TracingMode::Off {
            self.record_traced(kind, address, width, wait_states);
        }
    }

    /// Record a contiguous run of `count` byte-wide instruction-prefetch cycles
    /// (addresses `address..address + count`, all at `wait_states`) in one shot.
    /// The clock total advances by `count` cycles' worth unconditionally; the
    /// access count bumps by `count` when tracing is on; the per-cycle detail is
    /// pushed only in `Full` mode, honoring the capacity eviction. This is the
    /// bulk equivalent of `count` `record(InstructionPrefetch, .., Byte, ..)`
    /// calls, and it is bit-identical to that loop in all three accounting fields.
    #[inline]
    pub fn record_instruction_fetch_run(&mut self, address: u32, count: u32, wait_states: u8) {
        let clocks = BusCycle::clocks_for(BusWidth::Byte, wait_states);
        self.elapsed_clocks += u64::from(clocks) * u64::from(count);
        if self.mode != TracingMode::Off {
            self.record_traced_run(address, count, wait_states);
        }
    }

    /// Record a contiguous run of equal-width memory cycles. In the normal off mode this folds
    /// the whole run into one clock update. Counts and full tracing retain the same access count
    /// and addresses as individual `record` calls.
    #[inline]
    pub fn record_memory_run(
        &mut self,
        kind: BusAccessKind,
        address: u32,
        count: u32,
        width: BusWidth,
        wait_states: u8,
    ) {
        let clocks = BusCycle::clocks_for(width, wait_states);
        self.elapsed_clocks += u64::from(clocks) * u64::from(count);
        if self.mode != TracingMode::Off {
            self.record_traced_memory_run(kind, address, count, width, wait_states);
        }
    }

    #[cold]
    fn record_traced_memory_run(
        &mut self,
        kind: BusAccessKind,
        address: u32,
        count: u32,
        width: BusWidth,
        wait_states: u8,
    ) {
        self.access_count += u64::from(count);
        if self.mode == TracingMode::Full && self.capacity > 0 {
            for i in 0..count {
                if self.cycles.len() == self.capacity {
                    self.cycles.pop_front();
                }
                self.cycles.push_back(BusCycle::new(
                    kind,
                    address.wrapping_add(i.wrapping_mul(width.bytes())),
                    width,
                    wait_states,
                ));
            }
        }
    }

    #[cold]
    fn record_traced_run(&mut self, address: u32, count: u32, wait_states: u8) {
        self.access_count += u64::from(count);
        if self.mode == TracingMode::Full && self.capacity > 0 {
            for i in 0..count {
                if self.cycles.len() == self.capacity {
                    self.cycles.pop_front();
                }
                self.cycles.push_back(BusCycle::new(
                    BusAccessKind::InstructionPrefetch,
                    address.wrapping_add(i),
                    BusWidth::Byte,
                    wait_states,
                ));
            }
        }
    }

    #[cold]
    fn record_traced(
        &mut self,
        kind: BusAccessKind,
        address: u32,
        width: BusWidth,
        wait_states: u8,
    ) {
        self.access_count += 1;
        if self.mode == TracingMode::Full && self.capacity > 0 {
            if self.cycles.len() == self.capacity {
                self.cycles.pop_front();
            }
            self.cycles
                .push_back(BusCycle::new(kind, address, width, wait_states));
        }
    }

    /// The number of cycles recorded since the trace was last cleared, regardless
    /// of mode. `elapsed_clocks` is the clock total; this is the access total.
    pub fn access_count(&self) -> u64 {
        self.access_count
    }

    pub fn tracing_mode(&self) -> TracingMode {
        self.mode
    }

    pub fn set_tracing_mode(&mut self, mode: TracingMode) {
        self.mode = mode;
    }

    /// The retained cycles, oldest first. Bounded to the configured capacity, so
    /// after a long run this holds the most recent window rather than all history.
    /// `VecDeque` indexes (`cycles()[0]`), reports `len()`, and yields `iter()`,
    /// so existing callers read it the same as the old slice.
    pub fn cycles(&self) -> &VecDeque<BusCycle> {
        &self.cycles
    }

    /// The most recent cycle, or `None` when no cycle has been pushed.
    pub fn last(&self) -> Option<&BusCycle> {
        self.cycles.back()
    }

    pub fn elapsed_clocks(&self) -> u64 {
        self.elapsed_clocks
    }

    /// Add aggregate bus clocks without recording individual accesses.
    pub fn add_elapsed_clocks(&mut self, clocks: u64) {
        self.elapsed_clocks += clocks;
    }

    pub fn clear(&mut self) {
        self.cycles.clear();
        self.elapsed_clocks = 0;
        self.access_count = 0;
    }
}

/// The long-division scaler `izarravm-machine`'s call-out-site poll-skip seam
/// (`MachineBus::callout_poll_skip`, via its `now_at_run` closure) uses to combine a raw clock
/// count with a persona's (num, den, carry) core-clock timing dial. Lives here, not in
/// `izarravm-cpu`, because the machine-side seam has no other legal way to reach it (see
/// `MachineBus::callout_poll_skip`'s doc).
///
/// **Not shared with `izarravm-cpu`'s own callers** (GP2 poll-skip revision review N1):
/// `CpuGsw::preview_scale_clocks` and `poll_skip_core_projection` each carry their OWN copy of
/// this same long division rather than calling this function -- `izarravm-cpu` cannot depend on
/// `izarravm-bus` for it without an actual crate wire-up that does not exist today. The three
/// copies are identical arithmetic as of this writing (verified by direct comparison, GP2
/// poll-skip revision review §5), but nothing enforces that identity mechanically: a mutant that
/// changes one caller's arithmetic and not the others would NOT be caught by anything today. Do
/// not claim otherwise in this doc without actually wiring the interpreter-side callers through
/// this function first.
pub fn scale_core_clocks(raw: u64, num: u32, den: u32, rem: u64) -> Option<u64> {
    raw.checked_mul(u64::from(num))?
        .checked_add(rem)?
        .checked_div(u64::from(den))
}

/// One certified device-poll span, as `izarravm-cpu`'s call-out helper sees it, in plain
/// data the `izarravm-bus` POD can carry across the crate boundary that
/// `CpuBus::callout_poll_skip` cannot cross with the real `PollLoop` (its fields are
/// private to `izarravm-cpu`; see that method's doc). `fetches` holds
/// `PollLoop::fetch(0..fetch_count)`, `status_mask` / `spins_when_bit_set` together
/// reproduce `PollLoop::fresh_iteration_spins`'s sense for a single-bit mask (the only
/// kind a certified shape ever carries), and the clock-domain fields are exactly the
/// terms `scale_core_clocks` needs to reconstruct `now_at(n)` on the machine side
/// without re-deriving the persona's timing dial.
#[derive(Clone, Copy, Debug)]
pub struct CalloutPollSkipRequest {
    /// `PollLoop::fetch(0..fetch_count)`, in order.
    pub fetches: [(u32, u32, u8); 6],
    pub fetch_count: u8,
    /// The one bit the shape's status test reads: `0x01` or `0x08`, never any other
    /// value (`exact_poll_test_branch` refuses every other mask).
    pub status_mask: u8,
    /// `PollLoop::fresh_iteration_spins(status_mask)` -- i.e. "does the shape spin
    /// while the tested bit is SET". Precomputed on the `izarravm-cpu` side, which
    /// still holds the shape's real fields; the machine side only ever needs this one
    /// derived bool plus the live status byte's bit to answer "is this iteration still
    /// spinning".
    pub spins_when_bit_set: bool,
    /// The shape's own certified per-iteration raw core-clock charge (17 / 21 / 28).
    pub raw_core_clocks: u64,
    /// `CpuGsw::core_clocks_so_far` as the running block set it at entry -- constant
    /// for the whole block, and the base `now_at(n)` is computed from.
    pub core_clocks_at_block_entry: u64,
    /// This slot's own `prefix_raw_clocks + scale_weighted_fp_clocks(..).clocks`,
    /// i.e. every raw core clock the block has retired before this call-out, RAW
    /// (unscaled) and with any chained-hop float charge already folded in by the
    /// caller.
    pub prefix_raw: u64,
    /// The persona's `level_timing()` dial and the carried remainder
    /// (`CpuGsw::timing_rem`), read-only: the seam previews the scale, it never
    /// consumes the carry (the block's own single end-of-block batch call does that,
    /// once, exactly as it does today).
    pub core_num: u32,
    pub core_den: u32,
    pub timing_rem: u64,
    /// The RUN's remaining guest-clock budget (`BlockCache::block_batch_cap`) -- NOT the
    /// batch-absolute cap the interpreter's `try_poll_skip` bounds against (BLOCKER 2's fix; see
    /// `BlockCache::block_batch_cap`'s doc for the derivation). Zero means "no cap published" and
    /// refuses every skip (obligation 3).
    pub cap: u64,
    /// The batch-scoped scaled bus clock total AT THIS RUN'S ENTRY (`BlockCache::
    /// block_bus_at_entry`). The seam's own bus-clock reads
    /// (`MachineBus::poll_project_scaled_bus_clocks`) are BATCH-absolute; subtracting this turns
    /// them into the RUN-scoped growth `cap` actually bounds. Without it the cap test compares a
    /// run-scoped budget against a batch-absolute spend, silently subtracting the batch's prior
    /// bus clocks from the budget a second time (BLOCKER 2).
    pub bus_scaled_at_run_entry: u64,
    /// `IZARRAVM_DIRECT_POLL_MIN_ITERATIONS`, default 2 -- matches `try_poll_skip`'s
    /// own `low = 2` binary-search floor.
    pub min_iterations: u64,
    /// `IZARRAVM_DIRECT_POLL_MAX_RAW`, default `u32::MAX - IN_PORT_CORE_CLOCKS` -- the
    /// 32-bit return-lane bound (obligation 4): `raw_core_clocks * iterations` must fit
    /// under this or the low-32-bit clock lane the emitted call-out returns through
    /// would collide with the step-break / resync status bits above it.
    pub max_skipped_raw: u64,
}

/// Why `CpuBus::callout_poll_skip` declined, so the caller can bump the NAMED counter
/// its own screen owns (design §8) rather than one shared "it declined" lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalloutPollDecline {
    /// `Vega::poll_skip_status1_port_active()` refused (tracing armed, or the port's
    /// lazy path is off).
    PortInactive,
    /// `poll_bus_certificate` could not certify the shape's fetch/io bus cost.
    BusCertificate,
    /// The live status byte says this iteration was already about to exit the loop.
    NotSpinning,
    /// The CRTC oracle (`dots_until_status1_bit_change_from`) has no edge to project
    /// to (an unprogrammed CRTC, `frame_dots == 0`).
    NoEdge,
    /// The published cap, the 32-bit lane bound, or `min_iterations` left no
    /// admissible span.
    Cap,
}

/// What a committed call-out-site poll span cost and where it left the guest's clock,
/// in the units the caller's own return lane and counters need. `iterations` is always
/// `>= CalloutPollSkipRequest::min_iterations` on success; `callout_poll_skip` returns
/// `Err` rather than an `Ok` with zero iterations.
#[derive(Clone, Copy, Debug)]
pub struct CalloutPollSkipOutcome {
    pub iterations: u64,
    /// `raw_core_clocks * iterations`, RAW: the caller adds this to the same raw clock
    /// lane the call-out already returns through (`STACK_RAW_CLOCKS`), so the block's
    /// own single end-of-block batch call scales it exactly once, together with the
    /// block's other raw clocks. The seam must NEVER scale this itself.
    pub skipped_raw_core_clocks: u64,
    /// The bus clocks the commit added to `trace.elapsed_clocks()` -- reported for the
    /// I2 identity and the `jit_direct_poll_skip_raw_bus_clocks` counter; already
    /// applied by the seam, not something the caller must apply again.
    pub committed_raw_bus_clocks: u64,
    /// `now_at(iterations)`: the recomputed post-skip instant, byte-for-byte what the
    /// caller must publish (`CpuBus::publish_core_clocks`) before its own fall-through
    /// `read_io` call, so the returned byte is read at the beam the skip actually left
    /// (obligation 2b).
    pub now_after: u64,
}

/// The reflected-call memo's answer-time gate request (slice1 plan section 7.2 /
/// section 6's edge rule R4.2). Plain `Copy` data across the crate edge, exactly like
/// `CalloutPollSkipRequest`: `izarravm-bus` can name neither the CPU's memo nor the
/// machine's timeline, so the CPU supplies the two clock lumps it wants to charge plus
/// the one term only it knows (the core clocks this RUN has retired so far, which the
/// machine's own `prior_runs_core_clocks` deliberately excludes), and the machine
/// answers whether the lump fits inside the batch cap it computed at batch entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReflectedCallGateRequest {
    /// The SCALED core clocks the answer would charge (the CPU has already run its own
    /// remainder-carry projection, so this is exact, not an estimate).
    pub scaled_core_clocks: u64,
    /// The RAW bus clocks the answer would charge; the machine scales them with the
    /// batch's own `(num, den)` snapshot, rounding UP, because a gate that rounds down
    /// can admit a lump that crosses the cap by one clock.
    pub raw_bus_clocks: u64,
    /// Core clocks retired by the CURRENT run before the `INT` that is asking. The
    /// machine's `prior_runs_core_clocks` covers every PRIOR run of this batch and
    /// nothing of this one, so without this term `spent` is under-counted by however
    /// far into the run the `INT` sits, and the gate would admit a lump that runs past
    /// the cap.
    pub run_core_clocks_so_far: u64,
}

/// Why `CpuBus::reflected_call_gate` declined, one named counter lane per screen
/// (the `CalloutPollDecline` shape).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReflectedCallDecline {
    /// The lump does not fit the remaining batch allowance, and the batch cap came
    /// from the mode-class fallback grain rather than from a device edge.
    Cap,
    /// The lump does not fit, and this batch's cap IS a device edge: the lump would
    /// have crossed a PIT/DSP/WSS/RTC/ATA/serial/LPT/FDC/keyboard/MPU edge.
    DeviceEdge,
    /// A Class R range the answer intends to SKIP is visible to something other than
    /// the CPU (a device window, the framebuffer aperture, an armed DMA region), so
    /// its intermediate values are observable and the write may not be skipped.
    DmaVisible,
    /// The bus has no reflected-call machinery at all (the trait default, and every
    /// test double): a memo may never answer against it.
    NotArmed,
}

pub trait CpuBus {
    fn read_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<u32, BusError>;

    fn write_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        value: u32,
        kind: BusAccessKind,
    ) -> Result<(), BusError>;

    fn read_memory_direct(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<DirectMemoryRead, BusError> {
        self.read_memory(address, width, kind)
            .map(|value| DirectMemoryRead {
                value,
                direct: false,
            })
    }

    fn write_memory_direct(
        &mut self,
        address: u32,
        width: BusWidth,
        value: u32,
        kind: BusAccessKind,
    ) -> Result<DirectMemoryWrite, BusError> {
        self.write_memory(address, width, value, kind)
            .map(|()| DirectMemoryWrite { direct: false })
    }

    fn read_memory_bytes_direct(
        &mut self,
        _address: u32,
        _out: &mut [u8],
        _access_width: BusWidth,
        _kind: BusAccessKind,
    ) -> Result<usize, BusError> {
        Ok(0)
    }

    fn write_memory_bytes_direct(
        &mut self,
        _address: u32,
        _data: &[u8],
        _access_width: BusWidth,
        _kind: BusAccessKind,
    ) -> Result<usize, BusError> {
        Ok(0)
    }

    /// Return the page-local byte count available to the matching bulk data operation.
    /// A full result promises that the corresponding direct read or write can complete without
    /// falling through to a device handler. Other access kinds must return zero for device pages.
    fn direct_memory_bytes(
        &self,
        _address: u32,
        _bytes: usize,
        _access_width: BusWidth,
        _kind: BusAccessKind,
    ) -> usize {
        0
    }

    /// PURITY PRIMITIVE FOR CALL-OUT PREFLIGHT ONLY. NOT A DATA PATH.
    ///
    /// Reads `width` bytes of plain guest RAM and charges NOTHING: no wait states, no
    /// `trace.record`, no counter, no device. It exists for one caller shape -- a native call-out
    /// helper that must decide whether it may proceed BEFORE it is allowed to commit any effect,
    /// and that then re-reads the same bytes through the charged path so the charge order matches
    /// the interpreter's exactly (`jit/direct.rs`'s port call-out helpers, phase P).
    ///
    /// ANY OTHER CALLER IS A BUG. A read that the guest can observe the timing of must go through
    /// `read_memory_direct` or `read_memory`; using this instead silently deletes the access from
    /// `elapsed_clocks` and from the bus trace, which are the currency every performance
    /// comparison in this project is measured in. The default declines, so a bus without a
    /// provably side-effect-free RAM arm keeps the helper's refusal as the only answer.
    ///
    /// `&self`, which is the contract rather than a convenience: an implementation that needs
    /// `&mut self` is by construction doing something this must not do.
    ///
    /// A `Some` result additionally promises that `read_memory_direct(address, width, _)` will
    /// take its ALIGNED direct-RAM arm and return the same value -- so the caller may pair the
    /// probe with a charged re-read and know which charge it will pay.
    fn peek_direct_ram(&self, _address: u32, _width: BusWidth) -> Option<u32> {
        None
    }

    fn direct_page(
        &mut self,
        _address: u32,
        _kind: BusAccessKind,
    ) -> Result<Option<DirectPage>, BusError> {
        Ok(None)
    }

    /// Open a native-residency window. The default declines and keeps the
    /// interpreter path for buses without stable direct mappings and timing.
    fn begin_compiled_window(&mut self) -> Option<CompiledBusWindow> {
        None
    }

    /// Apply the aggregate effects from one completed native-residency window.
    /// Implementations must not fail after returning a window from `begin`.
    fn finish_compiled_window(&mut self, _window: CompiledBusWindow, _delta: CompiledBusDelta) {}

    fn in_batch_scaled_bus_clocks(&self) -> u64 {
        0
    }

    /// `in_batch_scaled_bus_clocks() >= target`, for implementations that can answer the
    /// comparison more cheaply than they can produce the value.
    ///
    /// The straight-line run loop asks this once per retired instruction — 553.6M times in a
    /// Quake/586 6.2G run — to test a cap that fires 22,903 times. `MachineBus`'s scaled figure
    /// is `(raw * num + rem) / den`, so answering it by value put a 64-bit DIVIDE in that loop;
    /// a RIP profile attributed 2.99% of wall to the accessor once an inline barrier made it
    /// visible separately. Comparing instead of dividing is exact, not approximate: for integers
    /// `A >= 0`, `target >= 0`, `den > 0`, `floor(A / den) >= target` iff `A >= target * den`.
    fn in_batch_scaled_bus_clocks_at_least(&self, target: u64) -> bool {
        self.in_batch_scaled_bus_clocks() >= target
    }

    /// RAW (unscaled) bus clocks this batch has accumulated so far — the `raw` that
    /// `in_batch_scaled_bus_clocks` divides down. Must be monotone non-decreasing within a batch.
    /// Only meaningful together with a non-zero `in_batch_scaled_bus_clocks_screen_scale`.
    ///
    /// **Trap, named for whoever reaches for this across a trip that spans more than one
    /// machine batch**: it resets at every batch re-entry (`izarravm-machine`'s `run.rs`
    /// re-snapshots the subtrahend at the top of each batch, including the plain IF-edge
    /// break — not just a cap/deadline/fault boundary), so subtracting two samples taken on
    /// either side of ANY batch re-entry can go negative or under-count. Use
    /// `cumulative_raw_bus_clocks` below for a span that must survive a batch boundary.
    fn in_batch_raw_bus_clocks(&self) -> u64 {
        0
    }

    /// RAW (unscaled) bus clocks since the bus was constructed — monotone non-decreasing for
    /// the whole run, NEVER reset at a batch boundary (unlike `in_batch_raw_bus_clocks` above).
    /// A caller that needs a raw bus-clock delta across a span that might cross one or more
    /// machine batches (a reflected-call trip, which crosses 6-8 IF-edge batch re-entries per
    /// the 0b review) must sample this, not the in-batch figure, at both ends and subtract.
    /// `0` on a bus that does not track it (the same "unsupported = 0" convention as
    /// `in_batch_raw_bus_clocks`).
    fn cumulative_raw_bus_clocks(&self) -> u64 {
        0
    }

    /// A per-batch constant `F` with `S(raw2) - S(raw1) <= (raw2 - raw1) * F` for every
    /// `raw2 >= raw1` inside one batch, where `S` is `in_batch_scaled_bus_clocks`. `0` means the
    /// bus offers no such bound and the run loop must always ask the exact question.
    ///
    /// Purpose: let the straight-line run loop screen the per-retired-instruction cap test with
    /// one cheap 64-bit compare. The exact test asks whether
    /// `total + (S(raw) - S(raw_entry)) >= cap`; substituting the bound above gives the
    /// necessary condition `total + (raw - raw_entry) * F >= cap`, so when that fails the exact
    /// test is certainly false and can be skipped. See `run_budgeted_inner` for the full
    /// derivation and the overflow handling.
    ///
    /// For a bus whose scaled figure is `floor((raw * num + rem) / den)` with `den > 0` and a
    /// per-batch snapshot of `(num, den, rem)`, `F = ceil(num / den)` is such a bound:
    /// `floor(a / den) - floor(b / den) <= ceil((a - b) / den)` for `a >= b`, the `rem` term
    /// cancels in `a - b`, and `ceil(x * num / den) <= x * ceil(num / den)` for `x >= 0`.
    fn in_batch_scaled_bus_clocks_screen_scale(&self) -> u64 {
        0
    }

    fn charge_direct_memory(
        &mut self,
        _address: u32,
        _width: BusWidth,
        _kind: BusAccessKind,
    ) -> Result<(), BusError> {
        Ok(())
    }

    /// Charge a direct-memory access already known to be plain RAM (not the Mode13h VGA
    /// aperture), skipping whatever aperture range compare `charge_direct_memory` would otherwise
    /// redo. Callers that have NOT independently established the access is outside the VGA
    /// aperture must call `charge_direct_memory` instead.
    ///
    /// The default DELEGATES to `charge_direct_memory` rather than no-op'ing: today only
    /// `MachineBus` and `TestBus` override this trait, and both also override
    /// `charge_direct_memory`, so nothing currently reaches this default. But nothing links the
    /// pair, and a future bus that overrides one and forgets the other would otherwise charge
    /// ZERO clocks on every FastMap hit -- a silent timing divergence with no test, no assert, no
    /// compile error (this campaign has shipped exactly that "missing arm invisible to every
    /// other assertion" bug class more than once). Delegating makes the failure mode "correct but
    /// pays the redundant aperture compare this method exists to skip" instead of "silently
    /// wrong"; behavior-neutral for both current implementors, since each already overrides this
    /// method directly and never reaches the default.
    fn charge_direct_ram_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<(), BusError> {
        self.charge_direct_memory(address, width, kind)
    }

    /// Charge a page-local MISALIGNED direct-RAM access exactly as the byte-splitting bus path
    /// charges it today: `width.bytes()` BYTE cycles at `address .. address + bytes`.
    ///
    /// That doubling (a misaligned word costs `2 x (2 + ws)` against an aligned word's
    /// `1 x (2 + ws)`, because `BusCycle::clocks_for` ignores width entirely) is a SIDE EFFECT of
    /// the splitting mechanism, not a modelled penalty -- nothing in tree says a misaligned access
    /// costs two bus cycles. Reproducing it here is a requirement of the caller that stopped
    /// splitting, not an endorsement of it: the point of admitting misaligned accesses to the
    /// interpreter's fast path is to remove a slow-path excursion, bit-identically.
    ///
    /// Callers must have established RAM backing and page-locality, and the Mode13h aperture must
    /// NOT come here -- its split has different DATA semantics, not merely different timing (a
    /// byte write into the Distira LFB is swallowed where a wide write would have stored).
    ///
    /// The default DELEGATES, per byte, for the reason `charge_direct_ram_memory` above spells
    /// out: a bus that overrides one of this family and forgets another must fail "correct but
    /// slow", never "silently zero clocks".
    fn charge_direct_ram_split(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<(), BusError> {
        for i in 0..width.bytes() {
            self.charge_direct_ram_memory(address.wrapping_add(i), BusWidth::Byte, kind)?;
        }
        Ok(())
    }

    /// Charge exactly what `read_memory_direct` would charge for its ALIGNED direct-RAM arm, and
    /// read nothing.
    ///
    /// THE PAIR TO `peek_direct_ram`: that primitive reads without charging so a call-out helper
    /// can decide whether it may proceed; this one charges without reading so the helper can pay
    /// the interpreter's exact price for a value it already holds from a paired `peek_direct_ram`
    /// call. A `Some` from `peek_direct_ram` promises the aligned direct-RAM arm, and this is
    /// that arm's charge with the load removed. Any other caller is a bug, for
    /// `peek_direct_ram`'s reason.
    ///
    /// NOT `charge_direct_ram_memory`, on purpose, even though that method's `MachineBus`
    /// override is charge-identical to this one's. Two reasons: `charge_direct_ram_memory`'s doc
    /// scopes it to the interpreter's FastMap serve path, a different caller with a different
    /// precondition (`PageKind::Ram` classified at population time, not `peek_direct_ram`'s
    /// aligned-arm promise); and on `TestBus` the two are NOT interchangeable -- `TestBus`'s
    /// override of `charge_direct_ram_memory` charges nothing at all unless its
    /// `direct_page_clocks` test knob is armed, which would silently zero this method's charge on
    /// every existing fixture that never had a reason to set that knob. The default below reads
    /// and discards through the general charged path instead, so a bus without an override is
    /// bit-identical to the code this method exists to replace.
    fn charge_direct_ram_read(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<(), BusError> {
        self.read_memory_direct(address, width, kind).map(|_| ())
    }

    /// Return an upper bound on the raw clocks added by one cached direct-memory charge. `Some`
    /// remains valid until the bus reports a step break. A JIT uses it only after its CPU-side
    /// direct-page cache hit; `None` keeps the ordinary instruction path.
    ///
    /// **This bounds ONE bus cycle, not one guest access, and the two stopped being the same
    /// thing.** `BusCycle::clocks_for` ignores `width` entirely -- it is `2 + wait_states` -- so
    /// every implementation of this method returns the same number for Byte, Word and Dword. A
    /// MISALIGNED access is split into `width.bytes()` byte cycles by every charging path that
    /// serves one, so its true cost is `width.bytes()` times what this returns: 2x for a Word, 4x
    /// for a Dword.
    ///
    /// That was invisible while the direct JIT side-exited on every misaligned access. It no
    /// longer does at its two lean one-lookup sites, and the interpreter's own direct-RAM path
    /// serves misaligned accesses as a byte run. **A caller certifying a budget, a step break or a
    /// batch cap against this value owes the `width.bytes()` multiplier itself** -- this method
    /// cannot apply it, because it does not know the access's alignment.
    ///
    /// There is no production caller in the tree as this is written; the two that exist are tests
    /// and the canonical-state dump. The warning is here rather than at a call site for exactly
    /// that reason: the next caller is the one that would get it wrong, and nothing would fault.
    fn jit_direct_memory_max_clocks(&self, _width: BusWidth, _kind: BusAccessKind) -> Option<u64> {
        None
    }

    /// Return the raw bus clocks that `charge_instruction_fetch_run(start, count)` will add.
    /// `Some` also guarantees that call cannot fail and that the cost remains valid until the bus
    /// reports a step break. A JIT uses this to preflight non-faulting fixed-cost native groups;
    /// `None` keeps the per-instruction path.
    fn jit_cached_fetch_run_clocks(&self, _start: u32, _count: u32) -> Option<u64> {
        None
    }

    /// Project the exact in-batch scaled bus-clock total after `additional_raw` clocks. `Some`
    /// guarantees a fixed integer-rational scaler for the batch, so the scaled delta for the same
    /// raw increment can drift by at most one clock as its starting remainder changes. A JIT may
    /// batch native instructions only when this returns `Some`; `None` keeps the ordinary path.
    fn jit_projected_batch_scaled_bus_clocks(&self, _additional_raw: u64) -> Option<u64> {
        None
    }

    /// Return the raw cacheable-RAM cost for one warm instruction fetch.
    fn jit_fetch_cost_clocks(&self) -> u64 {
        0
    }

    /// A value that CHANGES whenever any of the JIT cost dials above could return something
    /// different: `jit_fetch_cost_clocks`, `jit_data_cost_clocks`, `jit_mode13_data_cost_clocks`
    /// and the scale applied by `jit_scale_bus_cost_upper`. The Direct backend memoises
    /// worst-case per-hop costs derived from those dials and keys the memo on this value, so an
    /// implementation whose dials can move MUST override this. The default is correct only for a
    /// bus whose dials never change.
    fn jit_cost_dial_epoch(&self) -> u64 {
        0
    }

    /// Whether every direct-code block admitted by this bus can charge instruction fetches as
    /// one uniform per-instruction total. The default keeps the address-observing fallback.
    fn native_fetches_are_uniform(&self) -> bool {
        false
    }

    /// Whether native blocks may fold fetch and data observations into aggregate clock charges.
    /// Buses that retain per-access counts or addresses must return false so the interpreter keeps
    /// their trace contract exact. The conservative default requires an explicit opt-in.
    fn native_aggregate_accounting_allowed(&self) -> bool {
        false
    }

    /// S1d slice 2 of the shadow-cache-probe feature (`shadow-cache-probe`): whether
    /// THIS entry into a compiled block should be diverted to the interpreter instead
    /// of running native, so the interpreter's existing per-address shadow-probe seam
    /// sees every data access of the sampled iteration (the JIT's compiled body never
    /// carries per-access addresses; see `izarravm-machine`'s `shadow_cache` module
    /// doc). Called once per compiled-block ENTRY attempt, never per instruction. The
    /// default never diverts, so a bus without this feature (or with the probe
    /// unarmed) runs every block native, unchanged -- one predictable branch on the
    /// call site's side, zero cost here.
    #[cfg(feature = "shadow-cache-probe")]
    fn shadow_probe_should_sample_entry(&mut self) -> bool {
        false
    }

    /// Observe and charge repeated warm instruction fetches from one page-local direct block.
    /// `linear_start` is the guest-visible code address, while `physical_start` selects memory
    /// timing. Returning `true` tells the CPU that all observations and charges were applied.
    /// The default declines so generic buses retain the exact per-instruction fallback.
    fn charge_native_cached_fetches(
        &mut self,
        _linear_start: u32,
        _physical_start: u32,
        _fetch_lens: &[u8],
        _iterations: u64,
    ) -> bool {
        false
    }

    /// The clock cost this bus charges for ONE byte-wide direct data access (the JIT cost-fold's
    /// per-byte-access data constant). Buses without bus timing return 0.
    ///
    /// CALLER OBLIGATION: this is the flat Approximate-class L1 constant. The Accurate class and
    /// device-window accesses charge per-address, so the cost-fold must only fold Approximate-class
    /// blocks whose data hits the direct-page cache (which the native probe already requires) - the
    /// constant is wrong otherwise.
    fn jit_data_byte_cost_clocks(&self) -> u64 {
        0
    }

    /// Direct-RAM cost for one access of `width`. Backends with width-dependent bus cycles
    /// override this; the default preserves the existing flat byte-cost contract.
    fn jit_data_cost_clocks(&self, _width: BusWidth) -> u64 {
        self.jit_data_byte_cost_clocks()
    }

    /// Canonical Mode 13h cost for one native access of `width`. The direct backend uses the
    /// larger of this and the RAM cost for pre-entry deadline admission.
    fn jit_mode13_data_cost_clocks(&self, width: BusWidth) -> u64 {
        self.jit_data_cost_clocks(width)
    }

    /// Raw bus clocks one port access of `width` records. The Direct backend's interpreter
    /// call-out slots run real `read_io` calls from inside a native block, so the block's
    /// pre-entry budget bound has to price them. Defaulted to 0 for buses that record no port
    /// cost; `jit_cost_dial_epoch` must cover whatever an override reads.
    fn jit_io_cost_clocks(&self, _width: BusWidth) -> u64 {
        0
    }

    /// Convert a raw native bus-cost bound into the clock domain used by
    /// `in_batch_scaled_bus_clocks`. The default is identity for buses whose
    /// batch accounting is already raw or which do not scale bus time.
    fn jit_scale_bus_cost_upper(&self, raw_clocks: u64) -> u64 {
        raw_clocks
    }

    /// Conservative guest-clock cost for one byte of string data. Budgeted REP uses this before a
    /// chunk; buses with tiered or scaled timing override it so a cold cache or device window cannot
    /// cross an event.
    ///
    /// MUST NOT CHANGE WITHIN ONE INSTRUCTION: `run_string`'s `RepLimitPlan` prices the whole REP
    /// loop from a single call made before the loop starts, on the strength of this. An override
    /// that varies its answer per access silently moves the yield boundary the loop-invariant hoist
    /// computed, and does so only in release builds, where the hoist's `debug_assert_eq!` staleness
    /// check is compiled out.
    /// CPU-clock upper bound for one string I/O element's complete port transaction.
    /// Must remain conservative throughout the instruction, including device reprogramming.
    fn rep_io_cost_upper(&self, _port: u16, width: BusWidth) -> u64 {
        self.jit_scale_bus_cost_upper(self.jit_io_cost_clocks(width))
    }

    fn rep_data_byte_cost_upper(&self) -> u64 {
        self.jit_data_cost_clocks(BusWidth::Byte)
            .max(self.jit_mode13_data_cost_clocks(BusWidth::Byte))
    }

    /// Return a scaled-clock upper bound for one successful cold page translation, including
    /// PDE and PTE reads plus their possible accessed/dirty writes. `None` makes a budgeted paged
    /// REP yield before initial progress; a resumed instruction may still advance one iteration.
    ///
    /// MUST NOT CHANGE WITHIN ONE INSTRUCTION, for the same reason as `rep_data_byte_cost_upper`.
    fn rep_page_walk_cost_upper(&self) -> Option<u64> {
        None
    }

    /// Commit native VGA writes at a block-chain boundary. Implementations update
    /// the dirty-page set and charge their existing video-memory timing here.
    fn charge_native_vga_writes(&mut self, _writes: NativeVgaWrites) {}

    /// Compatibility seam for the current direct CPU backend.
    fn charge_native_mode13_writes(&mut self, writes: NativeMode13Writes) {
        self.charge_native_vga_writes(writes);
    }

    /// Charge `clocks` bus clocks in one shot (the JIT cost-fold's bulk flush). The folded block
    /// accumulates fetch + data cost from the two constants above and flushes it here at a flush
    /// point, keeping the device-scheduler-visible clock total correct without a per-access record.
    /// Buses without bus timing do nothing.
    fn charge_bus_clocks_bulk(&mut self, _clocks: u64) {}

    /// Copy physical instruction bytes into `out` without charging bus clocks.
    /// The CPU charges each consumed fetch byte separately so prefetch snapshots
    /// do not advance guest-visible time for bytes that never execute.
    fn prefetch_memory(&mut self, address: u32, out: &mut [u8]) -> Result<usize, BusError>;

    /// Charge one byte of instruction-fetch bus time at `address`.
    fn charge_instruction_fetch(&mut self, address: u32) -> Result<(), BusError>;

    /// Observe the LINEAR address of a code byte the CPU is about to consume
    /// (called per fetched byte, alongside the physical charge). Purely an
    /// observation seam: no bus time, no required side effects. The machine
    /// keys its BIOS software-interrupt stub recognition on this, because the
    /// stub table is an architectural (linear) address and a paging guest (an
    /// EMM386-class monitor shadowing the BIOS F-page) may back it with a
    /// different physical page, so the physical fetch address cannot identify
    /// a stub landing. Default: no-op.
    #[inline]
    fn note_code_fetch_linear(&mut self, _linear: u32) {}

    /// Observe a guest INVD/WBINVD or a machine/persona reset that flushes the
    /// real cache. Purely an observation seam: no bus time, no required
    /// guest-visible side effect. The machine uses this to flush its shadow
    /// L1 tag probe (`izarravm_machine::shadow_cache`) so the array does not
    /// carry state across an event the real 486 clears its own cache on.
    /// Default: no-op.
    #[inline]
    fn note_cache_flush(&mut self) {}

    /// Charge a warm instruction-fetch run in one address domain.
    fn charge_instruction_fetch_run(&mut self, start: u32, count: u32) -> Result<(), BusError> {
        for i in 0..count {
            self.charge_instruction_fetch(start.wrapping_add(i))?;
        }
        Ok(())
    }

    /// Charge the instruction-fetch clocks for a run of `count` physically contiguous bytes.
    /// Linear observation remains a separate `note_code_fetch_linear` call. Equivalent to
    /// `count` calls to `charge_instruction_fetch(physical_start + i)`, but an implementation
    /// backed by region-uniform memory may charge it in one operation.
    fn charge_physical_instruction_fetch_run(
        &mut self,
        physical_start: u32,
        count: u32,
    ) -> Result<(), BusError> {
        self.charge_instruction_fetch_run(physical_start, count)
    }

    /// `core_clocks_so_far`: CPU core clocks charged by prior instructions in the
    /// current straight-line run, NOT including the in-flight instruction issuing
    /// this read (the batch-break boundary today always falls after an IN's own
    /// charge, so this matches "now" as any batch break has always meant). Lets a
    /// lazy port read compute time-derived device state without ending the batch.
    ///
    /// `cpu_is_ring0_pm`: true when the CPU issuing this access is executing
    /// ring-0 protected-mode code that is not a V86 task (`Cpu::is_ring0_protected`)
    /// AT THE INSTANT of this call. Passed as a live per-call argument, not cached
    /// on the bus, because ring state can change mid-batch: a V86 sensitive-
    /// instruction #GP delivers into the ring-0 monitor, or the monitor's IRETD
    /// returns into V86, entirely inside a single `run_straight_line` run (neither
    /// transition sets `io_touched` or otherwise ends the run), so a value sampled
    /// once at bus construction would go stale before a later port access in the
    /// same batch observes it. Every call site already has `&self` in scope
    /// (mirroring `core_clocks_so_far`), so this is a live read, not new state.
    fn read_io(
        &mut self,
        port: u16,
        width: BusWidth,
        core_clocks_so_far: u64,
        cpu_is_ring0_pm: bool,
    ) -> Result<u32, BusError>;

    /// `core_clocks_so_far` and `cpu_is_ring0_pm` have the same live-per-call
    /// contract as `read_io` so output events can use their exact guest-time
    /// offset inside a batch.
    fn write_io(
        &mut self,
        port: u16,
        width: BusWidth,
        value: u32,
        core_clocks_so_far: u64,
        cpu_is_ring0_pm: bool,
    ) -> Result<(), BusError>;

    /// Default string-port cost forwards the generic byte transaction estimate.
    /// MachineBus specializes it for the complete physical port transaction.
    fn read_io_string_element(
        &mut self,
        port: u16,
        width: BusWidth,
        core_clocks_so_far: u64,
        cpu_is_ring0_pm: bool,
    ) -> Result<u32, BusError> {
        self.read_io(port, width, core_clocks_so_far, cpu_is_ring0_pm)
    }

    fn write_io_string_element(
        &mut self,
        port: u16,
        width: BusWidth,
        value: u32,
        core_clocks_so_far: u64,
        cpu_is_ring0_pm: bool,
    ) -> Result<(), BusError> {
        self.write_io(port, width, value, core_clocks_so_far, cpu_is_ring0_pm)
    }

    fn interrupt_acknowledge(&mut self, vector: u8, ax: u16) -> Result<(), BusError>;

    /// True while a device is asserting INTR through the PIC with a request that
    /// outranks anything in service. Non-mutating: the CPU calls it on every cycle
    /// and every halted cycle, so it must never consume the request. Defaulted to
    /// `false` so buses without an interrupt controller see no injected interrupts.
    fn interrupt_pending(&self) -> bool {
        false
    }

    /// The interrupt-acknowledge handshake. Commits the highest-priority request
    /// (sets ISR, clears IRR) and returns its vector byte, or `None` if the line
    /// dropped before acknowledge. Defaulted to `None`.
    fn acknowledge_interrupt(&mut self) -> Option<u8> {
        None
    }

    /// True when the machine must service something before the next instruction runs: a port
    /// access touched time-dependent device state, or an HLE software interrupt is pending. The
    /// straight-line run executor checks this after each instruction and ends the run so the
    /// machine services it at exactly the old per-instruction boundary. Defaulted false for buses
    /// without devices.
    fn requires_step_break(&self) -> bool {
        false
    }

    /// Publish the caller's in-run scaled core-clock instant so the bus's lazy,
    /// time-dependent predictions (the VGA beam, the OPL status peek) are taken at
    /// THIS instant rather than at whatever the previous `read_io` left behind.
    /// `read_io` publishes its own `core_clocks_so_far` argument as its first
    /// statement, so calling this immediately before a `read_io` at the same instant
    /// is redundant but harmless; a call-out that PROJECTS forward before it reads
    /// must call this explicitly first, because nothing else will.
    ///
    /// Defaulted to a no-op: a bus with no lazy time-dependent device state has
    /// nothing to publish to.
    fn publish_core_clocks(&mut self, _now: u64) {}

    /// The reflected-call memo's answer-time clamp (slice1 plan section 6, rule R4.2).
    /// Answer `Ok(())` only when the whole lump -- scaled core plus scaled bus --
    /// fits strictly inside `cap - spent`, where `cap` is the batch cap the machine's
    /// own loop computed from `compose_batch_cap(next_device_edge_ticks(), remaining)`.
    /// That is what makes "no device edge is ever inside an answered lump" a property
    /// of the SAME set the batch loop bounds itself by, rather than a second,
    /// drift-prone scan.
    ///
    /// **Defaulted to refuse.** A test double that inherits this can never fake a hit,
    /// on the `callout_poll_skip` precedent: an answer path whose gate silently passes
    /// on every bus in the CPU crate's own test suite would make every clamp fixture
    /// vacuous.
    fn reflected_call_gate(
        &self,
        _req: &ReflectedCallGateRequest,
    ) -> Result<(), ReflectedCallDecline> {
        Err(ReflectedCallDecline::NotArmed)
    }

    /// Commit the BUS half of an answered reflected call: add `raw` RAW bus clocks to
    /// the bus's own running total, exactly the way `MachineBus::poll_commit_bus`
    /// commits the poll skip's bus half, and return what was actually added.
    ///
    /// Defaulted to a no-op returning 0, so a bus with no bus-clock model charges
    /// nothing rather than silently pretending it did.
    fn reflected_call_commit_bus(&mut self, _raw: u64) -> u64 {
        0
    }

    /// Answer-time observer test (slice1 plan section 5 step 7, design review A.4):
    /// is any byte of the inclusive physical range `[lo, hi]` visible to something
    /// other than the CPU -- a device window, the framebuffer aperture, or an armed
    /// DMA region? A memo SKIPS its Class R writes, so an intermediate value inside
    /// such a range would be observed in the real world and not in the answered one.
    ///
    /// **Defaulted to `true` (visible, therefore refuse):** the safe answer for a bus
    /// that cannot tell.
    fn reflected_call_dma_visible(&self, _lo: u32, _hi: u32) -> bool {
        true
    }

    /// Is an HLE software interrupt already POSTED and waiting to be serviced at the next
    /// batch boundary? The reflected-call memo refuses a trip that has one pending at
    /// entry, and refuses to LEARN from a trip that acquired one on its way through: the
    /// posted vector is guest-visible work the memo carries no record of, so an answered
    /// trip would either skip it or deliver it at the wrong instant.
    ///
    /// Non-mutating, and DEFAULTED TO TRUE (posted, therefore refuse) for the same reason
    /// the gate defaults to refuse: a bus that cannot tell must not be able to admit.
    fn reflected_call_soft_int_posted(&self) -> bool {
        true
    }

    /// Tell the bus an answered reflected call just landed, so the machine's batch
    /// loop can END THE BATCH at this instant (slice1 plan section 6's batch-end
    /// mitigation). A real trip contains 6-8 IF-enable batch boundaries and an
    /// answered one contains none, so without this term an IRQ raised during the lump
    /// waits for the batch cap instead of the trip's first `IRET`. One boundary per
    /// answer against 6-8 per real trip is strictly fewer, and the pending-interrupt
    /// re-check then happens at the first instruction boundary after the lump.
    ///
    /// Defaulted to a no-op.
    fn note_reflected_call_answered(&mut self) {}

    /// Certify, binary-search and commit ONE call-out-site device-poll span, entirely
    /// inside `izarravm-machine` (the only crate with the device/timeline state the
    /// decision needs). Plain data in (`CalloutPollSkipRequest`), plain data out
    /// (`Result<CalloutPollSkipOutcome, CalloutPollDecline>`); the DECISION stays
    /// beside the interpreter's own `try_poll_skip` so the two executors share one
    /// predicate and cannot drift apart.
    ///
    /// **Why this is a method on `CpuBus` at all, and why it cannot take the real
    /// `PollLoop`.** The certified shape (`izarravm_cpu::PollLoop`) and the executor
    /// that knows how to certify, project and commit against it
    /// (`izarravm_machine::run::try_poll_skip`) live on opposite sides of a crate edge
    /// that only runs one way: `izarravm-cpu` depends on `izarravm-bus`, and
    /// `izarravm-bus` depends on neither `izarravm-cpu` nor `izarravm-machine`. A
    /// call-out helper generic over `B: CpuBus` therefore cannot name `PollLoop`, call
    /// any `izarravm-machine`-side accessor, or receive one back — the only channel
    /// available is a `Copy` POD declared in THIS crate, which is what
    /// `CalloutPollSkipRequest` / `CalloutPollSkipOutcome` are.
    ///
    /// Defaulted to always decline: a bus with no device-poll machinery (every test
    /// double in `izarravm-cpu` — `TestBus`, `A20Bus`, `FlatBus`, and any bus a future
    /// backend adds) inherits this and pays nothing beyond the four screens the caller
    /// runs before ever reaching here. **`TestBus` must not override this.** Doing so
    /// would let `jit_direct_poll_skip_spans` rise from a stub that always hits,
    /// reinstating the vacuity every fixture in the GP2 poll-skip mutant table exists
    /// to rule out — see that design's BLOCKER D.
    fn callout_poll_skip(
        &mut self,
        _req: &CalloutPollSkipRequest,
    ) -> Result<CalloutPollSkipOutcome, CalloutPollDecline> {
        Err(CalloutPollDecline::PortInactive)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortRange {
    pub start: u16,
    pub end: u16,
}

impl PortRange {
    pub const fn new(start: u16, end: u16) -> Self {
        Self { start, end }
    }

    pub const fn contains(self, port: u16) -> bool {
        self.start <= port && port <= self.end
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IoBus {
    claimed_ranges: Vec<PortRange>,
}

impl IoBus {
    pub fn claim(&mut self, range: PortRange) {
        self.claimed_ranges.push(range);
    }

    pub fn is_claimed(&self, port: u16) -> bool {
        self.claimed_ranges.iter().any(|range| range.contains(port))
    }
}

#[cfg(test)]
#[path = "bus_test.rs"]
mod tests;
