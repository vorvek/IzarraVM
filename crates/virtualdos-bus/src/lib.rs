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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BusTrace {
    cycles: Vec<BusCycle>,
    elapsed_clocks: u64,
}

impl BusTrace {
    pub fn push(&mut self, cycle: BusCycle) {
        self.elapsed_clocks += u64::from(cycle.clocks);
        self.cycles.push(cycle);
    }

    pub fn cycles(&self) -> &[BusCycle] {
        &self.cycles
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

    fn interrupt_acknowledge(&mut self, vector: u8) -> Result<(), BusError>;
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
    fn io_bus_tracks_claimed_ports() {
        let mut bus = IoBus::default();
        bus.claim(PortRange::new(0x220, 0x22f));

        assert!(bus.is_claimed(0x220));
        assert!(bus.is_claimed(0x22f));
        assert!(!bus.is_claimed(0x230));
    }
}
