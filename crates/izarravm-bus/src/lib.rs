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
        }
    }

    pub fn push(&mut self, cycle: BusCycle) {
        self.elapsed_clocks += u64::from(cycle.clocks);
        if self.capacity == 0 {
            return;
        }
        if self.cycles.len() == self.capacity {
            self.cycles.pop_front();
        }
        self.cycles.push_back(cycle);
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

    pub fn clear(&mut self) {
        self.cycles.clear();
        self.elapsed_clocks = 0;
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

    fn read_io(&mut self, port: u16, width: BusWidth) -> Result<u32, BusError>;

    fn write_io(&mut self, port: u16, width: BusWidth, value: u32) -> Result<(), BusError>;

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
mod tests {
    use super::*;

    #[test]
    fn memory_reads_and_writes() {
        let mut memory = Memory::new(16).unwrap();
        memory.write_u8(3, 0x7f).unwrap();
        memory.write_u16(4, 0x1234).unwrap();
        memory.write_u32(8, 0x89abcdef).unwrap();

        assert_eq!(memory.read_u8(3).unwrap(), 0x7f);
        assert_eq!(memory.read_u16(4).unwrap(), 0x1234);
        assert_eq!(memory.read_u32(8).unwrap(), 0x89abcdef);
    }

    #[test]
    fn memory_rejects_out_of_bounds_access() {
        let mut memory = Memory::new(16).unwrap();
        assert!(matches!(
            memory.write_u8(16, 0),
            Err(BusError::MemoryOutOfBounds { .. })
        ));
    }

    #[test]
    fn bus_cycle_tracks_state_count_and_byte_enables() {
        let cycle = BusCycle::new(BusAccessKind::DataRead, 0x1002, BusWidth::Word, 2);

        assert_eq!(cycle.byte_enable, 0b1100);
        assert_eq!(
            cycle.states,
            vec![BusState::T1, BusState::T2, BusState::Tw, BusState::Tw]
        );
        assert_eq!(cycle.clocks, 4);
    }

    #[test]
    fn bus_trace_caps_retained_cycles_but_keeps_total_clocks() {
        let mut trace = BusTrace::with_capacity(3);
        for index in 0..10u32 {
            trace.push(BusCycle::new(
                BusAccessKind::DataRead,
                index,
                BusWidth::Byte,
                0,
            ));
        }

        // Only the three most recent cycles survive, oldest first.
        assert_eq!(trace.cycles().len(), 3);
        assert_eq!(trace.cycles()[0].address, 7);
        assert_eq!(trace.cycles()[2].address, 9);
        assert_eq!(trace.last().unwrap().address, 9);
        // Every pushed cycle still counts toward the clock total (BusState::T1+T2
        // with no wait states is two clocks each, ten cycles is twenty clocks).
        assert_eq!(trace.elapsed_clocks(), 20);
    }

    #[test]
    fn bus_trace_zero_capacity_keeps_no_history_but_totals_clocks() {
        let mut trace = BusTrace::with_capacity(0);
        trace.push(BusCycle::new(BusAccessKind::DataRead, 0, BusWidth::Byte, 0));

        assert_eq!(trace.cycles().len(), 0);
        assert_eq!(trace.last(), None);
        assert_eq!(trace.elapsed_clocks(), 2);
    }

    #[test]
    fn io_bus_tracks_claimed_ports() {
        let mut bus = IoBus::default();
        bus.claim(PortRange::new(0x220, 0x22f));

        assert!(bus.is_claimed(0x220));
        assert!(bus.is_claimed(0x22f));
        assert!(!bus.is_claimed(0x230));
    }
}
