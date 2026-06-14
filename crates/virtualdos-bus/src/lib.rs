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
        assert_eq!(memory.read_u8(3).unwrap(), 0x7f);
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
    fn io_bus_tracks_claimed_ports() {
        let mut bus = IoBus::default();
        bus.claim(PortRange::new(0x220, 0x22f));

        assert!(bus.is_claimed(0x220));
        assert!(bus.is_claimed(0x22f));
        assert!(!bus.is_claimed(0x230));
    }
}
