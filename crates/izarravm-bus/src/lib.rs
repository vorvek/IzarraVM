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
    data: Vec<u8>,
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
            data: vec![0; size],
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

    /// Add `clocks` guest bus clocks to the running total in one shot, without a per-cycle record.
    /// The JIT cost-fold uses this to flush a block's accumulated fetch/data bus cost (which it
    /// computed from compile-time constants) at a flush point, instead of one `record` per access.
    /// Only `elapsed_clocks` (what the device scheduler and batch-end step read) is affected; the
    /// retained per-cycle detail is intentionally coarsened away for the folded run.
    pub fn add_elapsed_clocks(&mut self, clocks: u64) {
        self.elapsed_clocks += clocks;
    }

    pub fn clear(&mut self) {
        self.cycles.clear();
        self.elapsed_clocks = 0;
        self.access_count = 0;
    }
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

    fn direct_memory_bytes(&self, _address: u32, _bytes: usize, _access_width: BusWidth) -> usize {
        0
    }

    fn direct_page(
        &mut self,
        _address: u32,
        _kind: BusAccessKind,
    ) -> Result<Option<DirectPage>, BusError> {
        Ok(None)
    }

    /// Scaled bus clocks this batch has accumulated so far, in GUEST clocks.
    /// The straight-line run loop adds the growth of this figure to its core
    /// total when checking the run cap, so a bus-heavy run (a framebuffer
    /// blit) cannot exhaust a guest-clock budget expressed in core clocks and
    /// overshoot the next timer edge - the batch cap's PIT terms are guest
    /// clocks, and a real PIT interrupts at every edge. Buses without batch
    /// bus accounting return 0 and use the core-only check.
    fn in_batch_scaled_bus_clocks(&self) -> u64 {
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

    /// The clock cost this bus charges for ONE instruction-fetch access of cacheable RAM (the JIT
    /// cost-fold's per-instruction fetch constant, read once per region entry and folded across the
    /// block instead of charged per slot). Buses without bus timing return 0. See `charge_bus_clocks_bulk`.
    ///
    /// CALLER OBLIGATION: this is the CACHEABLE-RAM fast-path constant. ROM / device-window / A20-edge
    /// fetches charge differently (per byte, or address-classified), so the cost-fold must only fold
    /// blocks whose code is conventional cacheable RAM - the constant is wrong otherwise.
    fn jit_fetch_cost_clocks(&self) -> u64 {
        0
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

    /// Charge the instruction-fetch clocks for a run of `count` bytes starting at
    /// `start`. Equivalent to `count` calls to `charge_instruction_fetch(start + i)`,
    /// but an impl backed by region-uniform memory may charge it in one op. Default:
    /// the per-byte loop.
    fn charge_instruction_fetch_run(&mut self, start: u32, count: u32) -> Result<(), BusError> {
        for i in 0..count {
            self.charge_instruction_fetch(start.wrapping_add(i))?;
        }
        Ok(())
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
