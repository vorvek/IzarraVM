use izarravm_audio::{OplChip, Resampler};
use izarravm_bus::{BusAccessKind, BusCycle, BusError, BusTrace, BusWidth, CpuBus, Memory};
use izarravm_core::{CpuPreset, HardwareProfile, VideoCard};
use izarravm_cpu::{Cpu386, CpuError, SegmentIndex, SegmentRegister};
pub use izarravm_video::MARGO_ID_VALUE;
use izarravm_video::{
    DAC_ENTRIES, Framebuffer, MARGO_MMIO_SIZE, MARGO_VRAM_SIZE, MODE13H_MEMORY_SIZE, Margo,
    TextFrame, VGA_MODE13H_BASE, VGA_TEXT_BASE, VGA_TEXT_MEMORY_SIZE, VgaTextMode, VideoMode,
};
use thiserror::Error;

mod pic;

pub const HIGH_ROM_BASE: u32 = 0xffff_0000;
pub const MARGO_LFB_BASE: u32 = 0xE000_0000;
pub const MARGO_MMIO_BASE: u32 = 0xE040_0000;
pub const LOW_BIOS_BASE: u32 = 0x000f_0000;
pub const BIOS_ROM_SIZE: usize = 64 * 1024;
pub const BOOT_IMAGE_SIZE: usize = 1440 * 1024;
pub const BOOT_SECTOR_ADDRESS: usize = 0x7c00;
pub const BOOT_STAGE2_ADDRESS: usize = 0x8000;
pub const BIOS_IRET_STUB_ADDRESS: usize = 0x0600;
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

/// The OPL3 renders at this native rate; the Resonique 2 DAC outputs at 44100.
const OPL_NATIVE_HZ: u32 = 49_716;
const DAC_HZ: u32 = 44_100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveDisplay {
    Text,
    Mode13h,
    MargoLfb,
}

#[derive(Debug)]
pub struct Machine {
    profile: MachineProfile,
    cpu: Cpu386,
    memory: Memory,
    video: VgaTextMode,
    margo: Margo,
    margo_active: bool,
    rom: Vec<u8>,
    serial: SerialPort,
    device_ports: DevicePorts,
    pic: pic::Pic8259Pair,
    opl: OplChip,
    resampler: Resampler,
    opl_micros: f64, // fractional microseconds owed to the OPL timers
    trace: BusTrace,
    elapsed_clocks: u64,
}

impl Machine {
    pub fn new(profile: MachineProfile, rom: impl AsRef<[u8]>) -> Result<Self, MachineError> {
        let rom = rom.as_ref();
        if rom.len() != BIOS_ROM_SIZE {
            return Err(MachineError::InvalidRomSize(rom.len()));
        }

        let mut machine = Self {
            memory: Memory::from_mib(profile.memory_mib)?,
            profile,
            cpu: Cpu386::default(),
            video: VgaTextMode::default(),
            margo: Margo::default(),
            margo_active: false,
            rom: rom.to_vec(),
            serial: SerialPort::default(),
            device_ports: DevicePorts::default(),
            pic: pic::Pic8259Pair::default(),
            opl: OplChip::default(),
            resampler: Resampler::new(OPL_NATIVE_HZ, DAC_HZ),
            opl_micros: 0.0,
            trace: BusTrace::default(),
            elapsed_clocks: 0,
        };
        install_boot_bios_stubs(&mut machine.memory)?;
        Ok(machine)
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
            margo: Margo::default(),
            margo_active: false,
            rom: vec![0; BIOS_ROM_SIZE],
            serial: SerialPort::default(),
            device_ports: DevicePorts::default(),
            pic: pic::Pic8259Pair::default(),
            opl: OplChip::default(),
            resampler: Resampler::new(OPL_NATIVE_HZ, DAC_HZ),
            opl_micros: 0.0,
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

    pub fn mode13h_framebuffer(&self) -> &Framebuffer {
        self.video.mode13h_framebuffer()
    }

    fn make_bus(&mut self) -> MachineBus<'_> {
        MachineBus {
            memory: &mut self.memory,
            video: &mut self.video,
            margo: &mut self.margo,
            rom: &self.rom,
            serial: &mut self.serial,
            device_ports: &mut self.device_ports,
            trace: &mut self.trace,
            wait_states: self.profile.wait_states,
        }
    }

    pub fn read_physical_u8(&mut self, address: u32) -> u8 {
        let bus = self.make_bus();
        bus.read_memory_bytes(address, 1).map(|b| b[0]).unwrap_or(0)
    }

    pub fn write_physical_u8(&mut self, address: u32, value: u8) {
        let mut bus = self.make_bus();
        let _ = bus.write_memory_byte(address, value);
    }

    pub fn is_graphics_mode(&self) -> bool {
        self.video.active_mode() == VideoMode::Mode13h
    }

    pub fn margo(&self) -> &Margo {
        &self.margo
    }

    pub fn margo_mut(&mut self) -> &mut Margo {
        &mut self.margo
    }

    pub fn set_margo_mode_640x480x8(&mut self) {
        self.margo.set_mode_640x480x8();
        self.margo_active = true;
    }

    pub fn active_display(&self) -> ActiveDisplay {
        if self.margo_active {
            ActiveDisplay::MargoLfb
        } else if self.video.active_mode() == VideoMode::Mode13h {
            ActiveDisplay::Mode13h
        } else {
            ActiveDisplay::Text
        }
    }

    pub fn palette_argb(&self) -> [u32; DAC_ENTRIES] {
        self.video.palette_argb()
    }

    pub fn bus_trace(&self) -> &BusTrace {
        &self.trace
    }

    pub fn elapsed_clocks(&self) -> u64 {
        self.elapsed_clocks
    }

    /// Advance time-based devices (currently the OPL timers) by `clocks` of CPU
    /// time, converting to microseconds and carrying the fractional remainder.
    fn advance_devices(&mut self, clocks: u64) {
        self.opl_micros += clocks as f64 * 1_000_000.0 / self.profile.clock_hz as f64;
        let whole = self.opl_micros.floor();
        self.opl.advance_micros(whole as u64);
        self.opl_micros -= whole;
    }

    /// Render `native_samples` of OPL3 output at 49716 Hz and return the
    /// resampled 44100 Hz stereo PCM (saturated to 16-bit) ready for the DAC.
    /// The caller paces this by elapsed emulated time to keep audio in step.
    pub fn render_audio(&mut self, native_samples: usize) -> Vec<(i16, i16)> {
        let native: Vec<(i32, i32)> = (0..native_samples)
            .map(|_| self.opl.render_sample())
            .collect();
        self.resampler
            .process(&native)
            .into_iter()
            .map(|(l, r)| (clamp_i16(l), clamp_i16(r)))
            .collect()
    }

    /// Raise a hardware interrupt request line into the PIC. The PIT and other
    /// devices call this; slice 2b wires the PIT's IRQ0 tick through here.
    pub fn request_irq(&mut self, line: u8) {
        self.pic.request(line);
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
                    margo,
                    rom,
                    serial,
                    device_ports,
                    pic,
                    opl,
                    trace,
                    ..
                } = self;
                let mut bus = MachineBus {
                    memory,
                    video,
                    margo,
                    rom,
                    serial,
                    device_ports,
                    pic,
                    opl,
                    trace,
                    wait_states: profile.wait_states,
                };
                cpu.cycle(&mut bus)
            };

            match outcome {
                Ok(outcome) => {
                    let bus_clocks = self.trace.elapsed_clocks() - trace_before;
                    let step = u64::from(outcome.core_clocks) + bus_clocks;
                    self.elapsed_clocks += step;
                    // Advance the OPL timers so AdLib detection's delay loops see
                    // the overflow flag (the synthesis clock is driven separately
                    // by `render_audio`).
                    self.advance_devices(step);
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
    margo: &'a mut Margo,
    rom: &'a [u8],
    serial: &'a mut SerialPort,
    device_ports: &'a mut DevicePorts,
    pic: &'a mut pic::Pic8259Pair,
    opl: &'a mut OplChip,
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
        if let Some(opl_port) = opl_port(port) {
            // The chip drives only the status byte on reads; data ports read open-bus.
            return Ok(u32::from(self.opl.read_port(opl_port).unwrap_or(0xff)));
        }
        if let Some(value) = self.pic.read_port(port) {
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

        if let Some(opl_port) = opl_port(port) {
            self.opl.write_port(opl_port, value as u8);
            return Ok(());
        }
        if self.serial.write_port(port, value as u8)
            || self.video.write_port(port, value as u8)
            || self.pic.write_port(port, value as u8)
            || self.device_ports.write_port(port, value as u8)
        {
            Ok(())
        } else {
            Err(BusError::UnsupportedPort { port })
        }
    }

    fn interrupt_pending(&self) -> bool {
        self.pic.interrupt_pending()
    }

    fn acknowledge_interrupt(&mut self) -> Option<u8> {
        self.pic.acknowledge()
    }

    fn interrupt_acknowledge(&mut self, vector: u8, ax: u16) -> Result<(), BusError> {
        self.trace.push(BusCycle::new(
            BusAccessKind::InterruptAcknowledge,
            u32::from(vector),
            BusWidth::Byte,
            self.wait_states.io,
        ));
        if vector == 0x10 && ax == 0x0013 {
            self.video.set_mode13h();
        }
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
        0x0040..=0x0043, // PIT
        0x0060..=0x0064, // keyboard controller / A20 path
        0x0080..=0x008f, // DMA page registers
        0x0092..=0x0092, // system control port A / fast A20
        0x00c0..=0x00df, // DMA controller 2
        0x0220..=0x022f, // Sound Blaster base
        0x0388..=0x038b, // OPL2/OPL3 (intercepted by the chip, kept as a fallback)
        0x03b0..=0x03df, // MDA/CGA/EGA/VGA registers
    ];
    ranges.into_iter().flatten()
}

/// Map a CPU I/O port to the OPL register port (0x388-0x38B) it addresses, or
/// `None` if it is not an OPL port. The native AdLib ports are mirrored onto the
/// Sound Blaster Pro/16 OPL aliases at base 0x220: 0x220-0x223 are the two OPL3
/// banks, and 0x228-0x229 the OPL2-compatible single bank.
fn opl_port(port: u16) -> Option<u16> {
    match port {
        0x0388..=0x038b => Some(port),
        0x0220 => Some(0x0388),
        0x0221 => Some(0x0389),
        0x0222 => Some(0x038a),
        0x0223 => Some(0x038b),
        0x0228 => Some(0x0388),
        0x0229 => Some(0x0389),
        _ => None,
    }
}

/// Saturate an OPL mix value to the 16-bit DAC range.
fn clamp_i16(value: i32) -> i16 {
    value.clamp(i16::MIN as i32, i16::MAX as i32) as i16
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
    for vector in [0x10, 0x13] {
        let address = vector * 4;
        memory.write_u16(address, BIOS_IRET_STUB_ADDRESS as u16)?;
        memory.write_u16(address + 2, 0)?;
    }
    memory.write_u8(BIOS_IRET_STUB_ADDRESS, 0xcf)
}

impl MachineBus<'_> {
    fn read_memory_bytes(&self, address: u32, width: usize) -> Result<Vec<u8>, BusError> {
        if let Some(offset) = rom_offset(address, width) {
            return Ok(self.rom[offset..offset + width].to_vec());
        }

        if let Some(offset) = video_text_offset(address, width) {
            return (0..width)
                .map(|index| {
                    self.video
                        .read_u8(offset + index)
                        .map_err(|_| BusError::UnmappedMemory { address })
                })
                .collect();
        }

        if let Some(offset) = video_mode13h_offset(address, width) {
            return (0..width)
                .map(|index| {
                    self.video
                        .read_mode13h_u8(offset + index)
                        .map_err(|_| BusError::UnmappedMemory { address })
                })
                .collect();
        }

        if let Some(offset) = margo_lfb_offset(address, width) {
            return Ok((0..width)
                .map(|index| self.margo.read_vram_u8(offset + index))
                .collect());
        }

        if let Some(offset) = margo_mmio_offset(address, width) {
            return Ok((0..width)
                .map(|index| self.margo.read_mmio_u8(offset + index))
                .collect());
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

        if let Some(offset) = video_text_offset(address, 1) {
            return self
                .video
                .write_u8(offset, value)
                .map_err(|_| BusError::UnmappedMemory { address });
        }

        if let Some(offset) = video_mode13h_offset(address, 1) {
            return self
                .video
                .write_mode13h_u8(offset, value)
                .map_err(|_| BusError::UnmappedMemory { address });
        }

        if let Some(offset) = margo_lfb_offset(address, 1) {
            self.margo.write_vram_u8(offset, value);
            return Ok(());
        }

        if let Some(offset) = margo_mmio_offset(address, 1) {
            self.margo.write_mmio_u8(offset, value);
            return Ok(());
        }

        if (address as usize) < self.memory.len() {
            return self.memory.write_u8(address as usize, value);
        }

        Err(BusError::UnmappedMemory { address })
    }

    fn memory_wait_states(&self, address: u32) -> u8 {
        if rom_offset(address, 1).is_some() {
            self.wait_states.rom
        } else if video_text_offset(address, 1).is_some()
            || video_mode13h_offset(address, 1).is_some()
            || margo_lfb_offset(address, 1).is_some()
            || margo_mmio_offset(address, 1).is_some()
        {
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

fn video_text_offset(address: u32, width: usize) -> Option<usize> {
    let end = VGA_TEXT_BASE + VGA_TEXT_MEMORY_SIZE as u32;
    if (VGA_TEXT_BASE..end).contains(&address) && address + width as u32 <= end {
        Some((address - VGA_TEXT_BASE) as usize)
    } else {
        None
    }
}

fn video_mode13h_offset(address: u32, width: usize) -> Option<usize> {
    let end = VGA_MODE13H_BASE + MODE13H_MEMORY_SIZE as u32;
    if (VGA_MODE13H_BASE..end).contains(&address) && address + width as u32 <= end {
        Some((address - VGA_MODE13H_BASE) as usize)
    } else {
        None
    }
}

fn margo_lfb_offset(address: u32, width: usize) -> Option<usize> {
    let end = MARGO_LFB_BASE + MARGO_VRAM_SIZE as u32;
    if (MARGO_LFB_BASE..end).contains(&address) && address + width as u32 <= end {
        Some((address - MARGO_LFB_BASE) as usize)
    } else {
        None
    }
}

fn margo_mmio_offset(address: u32, width: usize) -> Option<usize> {
    let end = MARGO_MMIO_BASE + MARGO_MMIO_SIZE as u32;
    if (MARGO_MMIO_BASE..end).contains(&address) && address + width as u32 <= end {
        Some((address - MARGO_MMIO_BASE) as usize)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use izarravm_firmware::I386DX25_TEST_ROM;

    fn test_machine() -> Machine {
        Machine::new(
            MachineProfile::i386dx25(16, VideoCard::Et4000Ax),
            I386DX25_TEST_ROM,
        )
        .unwrap()
    }

    fn rom_with_code(code: &[u8]) -> Vec<u8> {
        let mut rom = vec![0; BIOS_ROM_SIZE];
        rom[..code.len()].copy_from_slice(code);
        rom[0xfff0..0xfff5].copy_from_slice(&[0xea, 0x00, 0x00, 0x00, 0xf0]);
        rom
    }

    #[test]
    fn rejects_non_64k_roms() {
        let err =
            Machine::new(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), [0u8; 8]).unwrap_err();

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
                margo: &mut machine.margo,
                rom: &machine.rom,
                serial: &mut machine.serial,
                device_ports: &mut machine.device_ports,
                pic: &mut machine.pic,
                opl: &mut machine.opl,
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
    fn int10_mode13h_maps_a000_to_framebuffer() {
        let rom = rom_with_code(&[
            0xb8, 0x13, 0x00, // mov ax, 0013h
            0xcd, 0x10, // int 10h
            0xb8, 0x00, 0xa0, // mov ax, a000h
            0x8e, 0xc0, // mov es, ax
            0xbf, 0x7b, 0x00, // mov di, 007bh
            0xb0, 0x2a, // mov al, 2ah
            0xaa, // stosb
            0xf4, // hlt
        ]);
        let mut machine =
            Machine::new(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), rom).unwrap();

        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();

        assert_eq!(reason, StopReason::Halted);
        assert_eq!(machine.mode13h_framebuffer().indexed_pixels[0x7b], 0x2a);
        assert!(machine.is_graphics_mode());
        assert!(machine.bus_trace().cycles().iter().any(|cycle| {
            cycle.kind == BusAccessKind::InterruptAcknowledge && cycle.address == 0x10
        }));
    }

    #[test]
    fn boot_image_starts_at_bios_loaded_boot_sector() {
        let mut machine = Machine::new_boot_image(
            MachineProfile::i386dx25(16, VideoCard::Et4000Ax),
            izarravm_firmware::X86_BOOT_TEST_IMAGE,
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
            MachineProfile::i386dx25(16, VideoCard::Et4000Ax),
            izarravm_firmware::X86_BOOT_TEST_IMAGE,
        )
        .unwrap();

        let reason = machine.run_until_halt_or_cycles(2_000_000).unwrap();
        let serial = machine.serial_text();
        let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();

        assert_eq!(reason, StopReason::Halted);
        assert!(serial.contains("PASS boot.stage2"));
        assert!(serial.contains("PASS video.vga_mode13h"));
        assert!(serial.contains("FAIL sound.opl3"));
        assert_eq!(
            usize::from(results.declared_record_count),
            results.records.len()
        );
        assert!(results.records.iter().any(|record| {
            record.status == izarravm_firmware::SuiteRecordStatus::Pass
                && record.name == "video.vga_text"
        }));
        assert!(results.records.iter().any(|record| {
            record.status == izarravm_firmware::SuiteRecordStatus::Pass
                && record.name == "video.vga_mode13h"
        }));
        assert_eq!(machine.mode13h_framebuffer().indexed_pixels[0], 0x2a);
        assert_eq!(machine.mode13h_framebuffer().indexed_pixels[319], 0x13);
        assert_eq!(machine.mode13h_framebuffer().indexed_pixels[63680], 0x7f);
        assert!(results.records.iter().any(|record| {
            record.status == izarravm_firmware::SuiteRecordStatus::Fail
                && record.name == "sound.sb_16bit_dma"
        }));
    }

    #[test]
    fn margo_apertures_route_through_the_bus() {
        let mut machine = test_machine();

        // LFB: write a byte at the aperture base + 5, read it back.
        let lfb = MARGO_LFB_BASE + 5;
        machine.write_physical_u8(lfb, 0x9c);
        assert_eq!(machine.read_physical_u8(lfb), 0x9c);

        // MMIO: the ID register reads the Margo magic.
        let id = u32::from(machine.read_physical_u8(MARGO_MMIO_BASE))
            | (u32::from(machine.read_physical_u8(MARGO_MMIO_BASE + 1)) << 8)
            | (u32::from(machine.read_physical_u8(MARGO_MMIO_BASE + 2)) << 16)
            | (u32::from(machine.read_physical_u8(MARGO_MMIO_BASE + 3)) << 24);
        assert_eq!(id, MARGO_ID_VALUE);
    }

    #[test]
    fn host_mode_set_selects_margo_lfb() {
        let mut machine = test_machine();
        assert_eq!(machine.active_display(), ActiveDisplay::Text);

        machine.set_margo_mode_640x480x8();

        assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);
        assert_eq!(machine.margo().display().width, 640);
        assert_eq!(machine.margo().display().height, 480);
    }

    #[test]
    fn passive_target_ports_allow_capability_probes_to_fail_cleanly() {
        // 0x224 is the SB DSP reset port: still an unimplemented passive port
        // (0x388 is now the OPL chip).
        let mut machine = test_machine();
        let value = with_bus(&mut machine, |bus| {
            bus.read_io(0x0224, BusWidth::Byte).unwrap()
        });

        assert_eq!(value, 0xff);
        assert!(
            machine
                .bus_trace()
                .cycles()
                .iter()
                .any(|cycle| cycle.kind == BusAccessKind::IoRead && cycle.address == 0x0224)
        );
    }

    // Run one closure against a freshly-borrowed bus over the whole machine.
    fn with_bus<R>(machine: &mut Machine, f: impl FnOnce(&mut MachineBus) -> R) -> R {
        let mut bus = MachineBus {
            memory: &mut machine.memory,
            video: &mut machine.video,
            rom: &machine.rom,
            serial: &mut machine.serial,
            device_ports: &mut machine.device_ports,
            pic: &mut machine.pic,
            opl: &mut machine.opl,
            trace: &mut machine.trace,
            wait_states: machine.profile.wait_states,
        };
        f(&mut bus)
    }

    // Program channel 0 as a keyed sine tone through the given OPL address/data
    // port pair (so the same routine can drive the native and aliased ports).
    fn program_tone(bus: &mut MachineBus, addr: u16, data: u16) {
        let mut write = |reg: u8, value: u8| {
            bus.write_io(addr, BusWidth::Byte, u32::from(reg)).unwrap();
            bus.write_io(data, BusWidth::Byte, u32::from(value))
                .unwrap();
        };
        write(0x20, 0x01); // modulator: multiple x1
        write(0x40, 0x3f); // modulator muted
        write(0x60, 0xf0); // modulator instant attack
        write(0x80, 0x00);
        write(0x23, 0x21); // carrier: sustained, multiple x1
        write(0x43, 0x00); // carrier loud
        write(0x63, 0xf0); // carrier instant attack
        write(0x83, 0x00);
        write(0xc0, 0x01); // additive
        write(0xa0, 0x00); // f-number low
        write(0xb0, 0x20 | (4 << 2) | 0x02); // key-on, block 4, fnum 0x200
    }

    #[test]
    fn pic_command_and_data_ports_route_to_the_master() {
        let mut machine = test_machine();
        let mask = with_bus(&mut machine, |bus| {
            // ICW1..ICW4 init, then OCW1 sets the mask to a recognizable value.
            for (port, value) in [
                (0x20u16, 0x11u32),
                (0x21, 0x08),
                (0x21, 0x04),
                (0x21, 0x01),
                (0x21, 0xab),
            ] {
                bus.write_io(port, BusWidth::Byte, value).unwrap();
            }
            // The data port reads back the mask, not the passive 0xff stub.
            bus.read_io(0x21, BusWidth::Byte).unwrap()
        });
        assert_eq!(mask, 0xab);
    }

    #[test]
    fn machine_bus_acknowledges_a_pic_interrupt() {
        let mut machine = test_machine();
        with_bus(&mut machine, |bus| {
            for (port, value) in [(0x20u16, 0x11u32), (0x21, 0x08), (0x21, 0x04), (0x21, 0x01)] {
                bus.write_io(port, BusWidth::Byte, value).unwrap();
            }
        });
        machine.request_irq(0);

        let (pending, vector) = with_bus(&mut machine, |bus| {
            (bus.interrupt_pending(), bus.acknowledge_interrupt())
        });
        assert!(pending);
        assert_eq!(vector, Some(0x08));
    }

    #[test]
    fn opl_sounds_through_the_adlib_ports() {
        let mut machine = test_machine();
        with_bus(&mut machine, |bus| program_tone(bus, 0x0388, 0x0389));
        let pcm = machine.render_audio(2000);
        assert!(
            pcm.iter().any(|&(l, _)| l != 0),
            "the OPL should produce audio via the AdLib ports"
        );
    }

    #[test]
    fn opl_sounds_through_the_sound_blaster_aliases() {
        // 0x220/0x221 mirror the OPL3 primary-bank address/data ports.
        let mut machine = test_machine();
        with_bus(&mut machine, |bus| program_tone(bus, 0x0220, 0x0221));
        let pcm = machine.render_audio(2000);
        assert!(
            pcm.iter().any(|&(l, _)| l != 0),
            "the OPL should produce audio via the SB base aliases"
        );
    }

    #[test]
    fn render_audio_outputs_at_the_dac_rate() {
        let mut machine = test_machine();
        let pcm = machine.render_audio(OPL_NATIVE_HZ as usize); // one second of OPL time
        assert!(
            (pcm.len() as i32 - DAC_HZ as i32).abs() < 50,
            "expected ~{DAC_HZ} frames, got {}",
            pcm.len()
        );
    }

    #[test]
    fn opl_timers_advance_with_machine_clocks() {
        // AdLib detection: arm timer 1 to overflow in one 80us step, let machine
        // time pass, and confirm the status port reports the overflow + IRQ.
        let mut machine = test_machine();
        with_bus(&mut machine, |bus| {
            let mut write = |reg: u8, value: u8| {
                bus.write_io(0x0388, BusWidth::Byte, u32::from(reg))
                    .unwrap();
                bus.write_io(0x0389, BusWidth::Byte, u32::from(value))
                    .unwrap();
            };
            write(0x04, 0x60); // mask both timers
            write(0x04, 0x80); // reset the overflow flags
            write(0x02, 0xff); // timer 1 preset: overflow in one step
            write(0x04, 0x21); // start timer 1 (unmasked), mask timer 2
        });

        // 100 us of CPU time (clock_hz/10000 clocks) covers the 80 us timer step.
        machine.advance_devices(machine.profile().clock_hz / 10_000);

        let status = with_bus(&mut machine, |bus| {
            bus.read_io(0x0388, BusWidth::Byte).unwrap()
        });
        assert_eq!(
            status & 0xe0,
            0xc0,
            "timer 1 overflow raises IRQ + timer-1 flag"
        );
    }
}
