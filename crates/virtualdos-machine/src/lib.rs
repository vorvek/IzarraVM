use thiserror::Error;
use virtualdos_bus::{BusAccessKind, BusCycle, BusError, BusTrace, BusWidth, CpuBus, Memory};
use virtualdos_core::{CpuPreset, HardwareProfile, VideoCard};
use virtualdos_cpu::{Cpu386, CpuError, SegmentIndex, SegmentRegister};
use virtualdos_video::{TextFrame, VGA_TEXT_BASE, VGA_TEXT_MEMORY_SIZE, VgaTextMode};

pub const HIGH_ROM_BASE: u32 = 0xffff_0000;
pub const LOW_BIOS_BASE: u32 = 0x000f_0000;
pub const BIOS_ROM_SIZE: usize = 64 * 1024;
pub const BOOT_IMAGE_SIZE: usize = 1440 * 1024;
pub const BOOT_SECTOR_ADDRESS: usize = 0x7c00;
pub const BOOT_STAGE2_ADDRESS: usize = 0x8000;
pub const BIOS_INT13_STUB_ADDRESS: usize = 0x0600;
pub const RESULT_BLOCK_ADDRESS: usize = 0x9000;

#[derive(Debug, Error)]
pub enum MachineError {
    #[error(transparent)]
    Bus(#[from] BusError),
    #[error(transparent)]
    Cpu(#[from] CpuError),
    #[error("test BIOS ROM must be exactly 64 KiB, got {0} bytes")]
    InvalidRomSize(usize),
    #[error("boot image must be exactly 1.44 MiB, got {0} bytes")]
    InvalidBootImageSize(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitStateProfile {
    pub ram: u8,
    pub rom: u8,
    pub video: u8,
    pub io: u8,
}

impl Default for WaitStateProfile {
    fn default() -> Self {
        Self {
            ram: 0,
            rom: 1,
            video: 1,
            io: 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineProfile {
    pub cpu: CpuPreset,
    pub clock_hz: u64,
    pub memory_mib: u16,
    pub video: VideoCard,
    pub wait_states: WaitStateProfile,
    pub address_pipelining: bool,
    pub cache_enabled: bool,
}

impl MachineProfile {
    pub fn i386dx25(memory_mib: u16, video: VideoCard) -> Self {
        Self {
            cpu: CpuPreset::I386Dx25,
            clock_hz: 25_000_000,
            memory_mib,
            video,
            wait_states: WaitStateProfile::default(),
            address_pipelining: false,
            cache_enabled: false,
        }
    }

    pub fn from_hardware_profile(profile: &HardwareProfile) -> Self {
        Self {
            cpu: profile.cpu,
            clock_hz: profile.clock_hz,
            memory_mib: profile.memory_mib,
            video: profile.video,
            wait_states: WaitStateProfile::default(),
            address_pipelining: false,
            cache_enabled: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    Halted,
    CycleLimit { requested: u64 },
    CpuError(String),
}

#[derive(Debug)]
pub struct Machine {
    profile: MachineProfile,
    cpu: Cpu386,
    memory: Memory,
    video: VgaTextMode,
    rom: Vec<u8>,
    serial: SerialPort,
    device_ports: DevicePorts,
    trace: BusTrace,
    elapsed_clocks: u64,
}

impl Machine {
    pub fn new(profile: MachineProfile, rom: impl AsRef<[u8]>) -> Result<Self, MachineError> {
        let rom = rom.as_ref();
        if rom.len() != BIOS_ROM_SIZE {
            return Err(MachineError::InvalidRomSize(rom.len()));
        }

        Ok(Self {
            memory: Memory::from_mib(profile.memory_mib)?,
            profile,
            cpu: Cpu386::default(),
            video: VgaTextMode::default(),
            rom: rom.to_vec(),
            serial: SerialPort::default(),
            device_ports: DevicePorts::default(),
            trace: BusTrace::default(),
            elapsed_clocks: 0,
        })
    }

    pub fn new_boot_image(
        profile: MachineProfile,
        image: impl AsRef<[u8]>,
    ) -> Result<Self, MachineError> {
        let image = image.as_ref();
        if image.len() != BOOT_IMAGE_SIZE {
            return Err(MachineError::InvalidBootImageSize(image.len()));
        }

        let mut machine = Self {
            memory: Memory::from_mib(profile.memory_mib)?,
            profile,
            cpu: boot_sector_cpu(),
            video: VgaTextMode::default(),
            rom: vec![0; BIOS_ROM_SIZE],
            serial: SerialPort::default(),
            device_ports: DevicePorts::default(),
            trace: BusTrace::default(),
            elapsed_clocks: 0,
        };

        for (offset, byte) in image[0..512].iter().copied().enumerate() {
            machine
                .memory
                .write_u8(BOOT_SECTOR_ADDRESS + offset, byte)?;
        }

        let stage2_len = 16 * 512;
        for (offset, byte) in image[512..512 + stage2_len].iter().copied().enumerate() {
            machine
                .memory
                .write_u8(BOOT_STAGE2_ADDRESS + offset, byte)?;
        }

        install_boot_bios_stubs(&mut machine.memory)?;
        Ok(machine)
    }

    pub fn profile(&self) -> &MachineProfile {
        &self.profile
    }

    pub fn cpu(&self) -> &Cpu386 {
        &self.cpu
    }

    pub fn memory(&self) -> &Memory {
        &self.memory
    }

    pub fn serial_output(&self) -> &[u8] {
        self.serial.output()
    }

    pub fn serial_text(&self) -> String {
        String::from_utf8_lossy(self.serial_output()).into_owned()
    }

    pub fn result_block_bytes(&self, len: usize) -> Vec<u8> {
        let end = RESULT_BLOCK_ADDRESS
            .saturating_add(len)
            .min(self.memory.len());
        self.memory.as_slice()[RESULT_BLOCK_ADDRESS..end].to_vec()
    }

    pub fn screen_text(&self) -> TextFrame {
        self.video.frame()
    }

    pub fn bus_trace(&self) -> &BusTrace {
        &self.trace
    }

    pub fn elapsed_clocks(&self) -> u64 {
        self.elapsed_clocks
    }

    pub fn run_cycles(&mut self, cycles: u64) -> Result<StopReason, MachineError> {
        let deadline = self.elapsed_clocks.saturating_add(cycles);
        self.run_until_clock(deadline, cycles)
    }

    pub fn run_until_halt_or_cycles(
        &mut self,
        max_cycles: u64,
    ) -> Result<StopReason, MachineError> {
        let deadline = self.elapsed_clocks.saturating_add(max_cycles);
        self.run_until_clock(deadline, max_cycles)
    }

    fn run_until_clock(
        &mut self,
        deadline: u64,
        requested: u64,
    ) -> Result<StopReason, MachineError> {
        while self.elapsed_clocks < deadline {
            let trace_before = self.trace.elapsed_clocks();
            let outcome = {
                let Machine {
                    profile,
                    cpu,
                    memory,
                    video,
                    rom,
                    serial,
                    device_ports,
                    trace,
                    ..
                } = self;
                let mut bus = MachineBus {
                    memory,
                    video,
                    rom,
                    serial,
                    device_ports,
                    trace,
                    wait_states: profile.wait_states,
                };
                cpu.cycle(&mut bus)
            };

            match outcome {
                Ok(outcome) => {
                    let bus_clocks = self.trace.elapsed_clocks() - trace_before;
                    self.elapsed_clocks += u64::from(outcome.core_clocks) + bus_clocks;
                    if outcome.halted {
                        return Ok(StopReason::Halted);
                    }
                }
                Err(error) => return Ok(StopReason::CpuError(error.to_string())),
            }
        }

        Ok(StopReason::CycleLimit { requested })
    }
}

struct MachineBus<'a> {
    memory: &'a mut Memory,
    video: &'a mut VgaTextMode,
    rom: &'a [u8],
    serial: &'a mut SerialPort,
    device_ports: &'a mut DevicePorts,
    trace: &'a mut BusTrace,
    wait_states: WaitStateProfile,
}

impl CpuBus for MachineBus<'_> {
    fn read_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<u32, BusError> {
        if should_split(address, width) {
            let mut value = 0u32;
            for offset in 0..width.bytes() {
                let byte = self.read_memory(address + offset, BusWidth::Byte, kind)?;
                value |= byte << (offset * 8);
            }
            return Ok(value);
        }

        self.trace.push(BusCycle::new(
            kind,
            address,
            width,
            self.memory_wait_states(address),
        ));

        let bytes = width.bytes() as usize;
        let data = self.read_memory_bytes(address, bytes)?;
        Ok(match width {
            BusWidth::Byte => u32::from(data[0]),
            BusWidth::Word => u32::from(u16::from_le_bytes([data[0], data[1]])),
            BusWidth::Dword => u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
        })
    }

    fn write_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        value: u32,
        kind: BusAccessKind,
    ) -> Result<(), BusError> {
        if should_split(address, width) {
            for offset in 0..width.bytes() {
                self.write_memory(
                    address + offset,
                    BusWidth::Byte,
                    (value >> (offset * 8)) & 0xff,
                    kind,
                )?;
            }
            return Ok(());
        }

        self.trace.push(BusCycle::new(
            kind,
            address,
            width,
            self.memory_wait_states(address),
        ));

        match width {
            BusWidth::Byte => self.write_memory_byte(address, value as u8),
            BusWidth::Word => {
                for (offset, byte) in (value as u16).to_le_bytes().into_iter().enumerate() {
                    self.write_memory_byte(address + offset as u32, byte)?;
                }
                Ok(())
            }
            BusWidth::Dword => {
                for (offset, byte) in value.to_le_bytes().into_iter().enumerate() {
                    self.write_memory_byte(address + offset as u32, byte)?;
                }
                Ok(())
            }
        }
    }

    fn read_io(&mut self, port: u16, width: BusWidth) -> Result<u32, BusError> {
        self.trace.push(BusCycle::new(
            BusAccessKind::IoRead,
            u32::from(port),
            width,
            self.wait_states.io,
        ));

        if width != BusWidth::Byte {
            return Err(BusError::WidthMismatch { width });
        }

        if let Some(value) = self.serial.read_port(port) {
            return Ok(u32::from(value));
        }
        if let Some(value) = self.video.read_port(port) {
            return Ok(u32::from(value));
        }
        self.device_ports
            .read_port(port)
            .map(u32::from)
            .ok_or(BusError::UnsupportedPort { port })
    }

    fn write_io(&mut self, port: u16, width: BusWidth, value: u32) -> Result<(), BusError> {
        self.trace.push(BusCycle::new(
            BusAccessKind::IoWrite,
            u32::from(port),
            width,
            self.wait_states.io,
        ));

        if width != BusWidth::Byte {
            return Err(BusError::WidthMismatch { width });
        }

        if self.serial.write_port(port, value as u8)
            || self.video.write_port(port, value as u8)
            || self.device_ports.write_port(port, value as u8)
        {
            Ok(())
        } else {
            Err(BusError::UnsupportedPort { port })
        }
    }

    fn interrupt_acknowledge(&mut self, vector: u8) -> Result<(), BusError> {
        self.trace.push(BusCycle::new(
            BusAccessKind::InterruptAcknowledge,
            u32::from(vector),
            BusWidth::Byte,
            self.wait_states.io,
        ));
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DevicePorts {
    ports: std::collections::BTreeMap<u16, u8>,
}

impl Default for DevicePorts {
    fn default() -> Self {
        let mut ports = std::collections::BTreeMap::new();
        for port in known_passive_ports() {
            ports.insert(port, 0xff);
        }
        ports.insert(0x0092, 0x00);
        Self { ports }
    }
}

impl DevicePorts {
    fn read_port(&self, port: u16) -> Option<u8> {
        self.ports.get(&port).copied()
    }

    fn write_port(&mut self, port: u16, value: u8) -> bool {
        let Some(slot) = self.ports.get_mut(&port) else {
            return false;
        };
        *slot = value;
        true
    }
}

fn known_passive_ports() -> impl Iterator<Item = u16> {
    let ranges = [
        0x0000..=0x000f, // DMA controller 1
        0x0020..=0x0021, // master PIC
        0x0040..=0x0043, // PIT
        0x0060..=0x0064, // keyboard controller / A20 path
        0x0080..=0x008f, // DMA page registers
        0x0092..=0x0092, // system control port A / fast A20
        0x00a0..=0x00a1, // slave PIC
        0x00c0..=0x00df, // DMA controller 2
        0x0220..=0x022f, // Sound Blaster base
        0x0388..=0x038b, // OPL2/OPL3
        0x03b0..=0x03df, // MDA/CGA/EGA/VGA registers
    ];
    ranges.into_iter().flatten()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SerialPort {
    registers: [u8; 8],
    output: Vec<u8>,
}

impl Default for SerialPort {
    fn default() -> Self {
        let mut registers = [0; 8];
        registers[5] = 0x60;
        Self {
            registers,
            output: Vec::new(),
        }
    }
}

impl SerialPort {
    fn output(&self) -> &[u8] {
        &self.output
    }

    fn read_port(&self, port: u16) -> Option<u8> {
        let offset = serial_offset(port)?;
        if offset == 5 {
            Some(0x60)
        } else {
            Some(self.registers[offset])
        }
    }

    fn write_port(&mut self, port: u16, value: u8) -> bool {
        let Some(offset) = serial_offset(port) else {
            return false;
        };

        self.registers[offset] = value;
        if offset == 0 && self.registers[3] & 0x80 == 0 {
            self.output.push(value);
        }
        true
    }
}

fn serial_offset(port: u16) -> Option<usize> {
    if (0x03f8..=0x03ff).contains(&port) {
        Some(usize::from(port - 0x03f8))
    } else {
        None
    }
}

fn boot_sector_cpu() -> Cpu386 {
    let mut cpu = Cpu386::default();
    for segment in [
        SegmentIndex::Cs,
        SegmentIndex::Ds,
        SegmentIndex::Es,
        SegmentIndex::Ss,
        SegmentIndex::Fs,
        SegmentIndex::Gs,
    ] {
        cpu.registers.set_segment(segment, SegmentRegister::real(0));
    }
    cpu.registers.eip = BOOT_SECTOR_ADDRESS as u32;
    cpu.registers.eflags = 0x0000_0002;
    cpu.registers.set_edx(0x80);
    cpu
}

fn install_boot_bios_stubs(memory: &mut Memory) -> Result<(), BusError> {
    let vector = 0x13 * 4;
    memory.write_u16(vector, BIOS_INT13_STUB_ADDRESS as u16)?;
    memory.write_u16(vector + 2, 0)?;
    memory.write_u8(BIOS_INT13_STUB_ADDRESS, 0xcf)
}

impl MachineBus<'_> {
    fn read_memory_bytes(&self, address: u32, width: usize) -> Result<Vec<u8>, BusError> {
        if let Some(offset) = rom_offset(address, width) {
            return Ok(self.rom[offset..offset + width].to_vec());
        }

        if let Some(offset) = video_offset(address, width) {
            return (0..width)
                .map(|index| {
                    self.video
                        .read_u8(offset + index)
                        .map_err(|_| BusError::UnmappedMemory { address })
                })
                .collect();
        }

        let end = address as usize + width;
        if end <= self.memory.len() {
            return (0..width)
                .map(|index| self.memory.read_u8(address as usize + index))
                .collect();
        }

        Err(BusError::UnmappedMemory { address })
    }

    fn write_memory_byte(&mut self, address: u32, value: u8) -> Result<(), BusError> {
        if rom_offset(address, 1).is_some() {
            return Ok(());
        }

        if let Some(offset) = video_offset(address, 1) {
            return self
                .video
                .write_u8(offset, value)
                .map_err(|_| BusError::UnmappedMemory { address });
        }

        if (address as usize) < self.memory.len() {
            return self.memory.write_u8(address as usize, value);
        }

        Err(BusError::UnmappedMemory { address })
    }

    fn memory_wait_states(&self, address: u32) -> u8 {
        if rom_offset(address, 1).is_some() {
            self.wait_states.rom
        } else if video_offset(address, 1).is_some() {
            self.wait_states.video
        } else {
            self.wait_states.ram
        }
    }
}

fn should_split(address: u32, width: BusWidth) -> bool {
    match width {
        BusWidth::Byte => false,
        BusWidth::Word => address & 0x1 != 0,
        BusWidth::Dword => address & 0x3 != 0,
    }
}

fn rom_offset(address: u32, width: usize) -> Option<usize> {
    let offset = if (HIGH_ROM_BASE..=u32::MAX).contains(&address) {
        address.wrapping_sub(HIGH_ROM_BASE)
    } else if (LOW_BIOS_BASE..LOW_BIOS_BASE + BIOS_ROM_SIZE as u32).contains(&address) {
        address - LOW_BIOS_BASE
    } else {
        return None;
    } as usize;

    (offset + width <= BIOS_ROM_SIZE).then_some(offset)
}

fn video_offset(address: u32, width: usize) -> Option<usize> {
    let end = VGA_TEXT_BASE + VGA_TEXT_MEMORY_SIZE as u32;
    if (VGA_TEXT_BASE..end).contains(&address) && address + width as u32 <= end {
        Some((address - VGA_TEXT_BASE) as usize)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use virtualdos_firmware::I386DX25_TEST_ROM;

    fn test_machine() -> Machine {
        Machine::new(
            MachineProfile::i386dx25(16, VideoCard::Et4000W32p),
            I386DX25_TEST_ROM,
        )
        .unwrap()
    }

    #[test]
    fn rejects_non_64k_roms() {
        let err = Machine::new(
            MachineProfile::i386dx25(16, VideoCard::Et4000W32p),
            [0u8; 8],
        )
        .unwrap_err();

        assert!(matches!(err, MachineError::InvalidRomSize(8)));
    }

    #[test]
    fn first_instruction_fetch_uses_386_reset_vector() {
        let mut machine = test_machine();
        let reason = machine.run_cycles(32).unwrap();

        assert_ne!(reason, StopReason::Halted);
        assert_eq!(
            machine.bus_trace().cycles()[0].kind,
            BusAccessKind::InstructionPrefetch
        );
        assert_eq!(machine.bus_trace().cycles()[0].address, 0xffff_fff0);
    }

    #[test]
    fn unaligned_dword_splits_into_byte_bus_cycles() {
        let mut machine = test_machine();
        {
            let mut bus = MachineBus {
                memory: &mut machine.memory,
                video: &mut machine.video,
                rom: &machine.rom,
                serial: &mut machine.serial,
                device_ports: &mut machine.device_ports,
                trace: &mut machine.trace,
                wait_states: machine.profile.wait_states,
            };
            bus.write_memory(
                0x101,
                BusWidth::Dword,
                0x1234_5678,
                BusAccessKind::DataWrite,
            )
            .unwrap();
        }

        let writes = machine
            .bus_trace()
            .cycles()
            .iter()
            .filter(|cycle| cycle.kind == BusAccessKind::DataWrite)
            .count();
        assert_eq!(writes, 4);
    }

    #[test]
    fn test_rom_reaches_deterministic_text_screen() {
        let mut machine = test_machine();
        let reason = machine.run_until_halt_or_cycles(5_000_000).unwrap();
        let frame = machine.screen_text();

        assert_eq!(reason, StopReason::Halted);
        assert_eq!(frame.line_string(0), "RESET VECTOR + BIOS INT10 PASS");
        assert_eq!(frame.line_string(1), "B8000 DIRECT TEXT PASS");
        assert_eq!(frame.line_string(2), "PROTECTED MODE FLAT SEGMENTS PASS");
        assert_eq!(frame.line_string(3), "PAGING + B8000 ALIAS PASS");
        assert_eq!(frame.line_string(4), "RING0 PAGE FAULT HANDLER PASS");
        assert!(
            machine
                .bus_trace()
                .cycles()
                .iter()
                .any(|cycle| cycle.kind == BusAccessKind::PageWalkRead)
        );
        assert!(machine.cpu().is_protected_mode());
        assert!(machine.cpu().is_paging_enabled());
    }

    #[test]
    fn boot_image_starts_at_bios_loaded_boot_sector() {
        let mut machine = Machine::new_boot_image(
            MachineProfile::i386dx25(16, VideoCard::Et4000W32p),
            virtualdos_firmware::X86_BOOT_TEST_IMAGE,
        )
        .unwrap();

        let reason = machine.run_cycles(16).unwrap();

        assert_ne!(reason, StopReason::Halted);
        assert_eq!(
            machine.bus_trace().cycles()[0].address,
            BOOT_SECTOR_ADDRESS as u32
        );
    }

    #[test]
    fn boot_image_emits_serial_records_and_result_block() {
        let mut machine = Machine::new_boot_image(
            MachineProfile::i386dx25(16, VideoCard::Et4000W32p),
            virtualdos_firmware::X86_BOOT_TEST_IMAGE,
        )
        .unwrap();

        let reason = machine.run_until_halt_or_cycles(2_000_000).unwrap();
        let serial = machine.serial_text();
        let results = virtualdos_firmware::parse_result_block(machine.memory().as_slice()).unwrap();

        assert_eq!(reason, StopReason::Halted);
        assert!(serial.contains("PASS boot.stage2"));
        assert!(serial.contains("FAIL sound.opl3"));
        assert_eq!(
            usize::from(results.declared_record_count),
            results.records.len()
        );
        assert!(results.records.iter().any(|record| {
            record.status == virtualdos_firmware::SuiteRecordStatus::Pass
                && record.name == "video.vga_text"
        }));
        assert!(results.records.iter().any(|record| {
            record.status == virtualdos_firmware::SuiteRecordStatus::Fail
                && record.name == "sound.sb_16bit_dma"
        }));
    }

    #[test]
    fn passive_target_ports_allow_capability_probes_to_fail_cleanly() {
        let mut machine = test_machine();
        let value = {
            let mut bus = MachineBus {
                memory: &mut machine.memory,
                video: &mut machine.video,
                rom: &machine.rom,
                serial: &mut machine.serial,
                device_ports: &mut machine.device_ports,
                trace: &mut machine.trace,
                wait_states: machine.profile.wait_states,
            };
            bus.read_io(0x0388, BusWidth::Byte).unwrap()
        };

        assert_eq!(value, 0xff);
        assert!(
            machine
                .bus_trace()
                .cycles()
                .iter()
                .any(|cycle| cycle.kind == BusAccessKind::IoRead && cycle.address == 0x0388)
        );
    }
}
