use izarravm_audio::{OplChip, Resampler};
use izarravm_bus::{BusAccessKind, BusCycle, BusError, BusTrace, BusWidth, CpuBus, Memory};
use izarravm_core::{CpuPreset, HardwareProfile, VideoCard};
use izarravm_cpu::{Cpu386, CpuError, SegmentIndex, SegmentRegister};
pub use izarravm_video::MARGO_ID_VALUE;
use izarravm_video::{
    DAC_ENTRIES, Framebuffer, MARGO_MMIO_SIZE, MARGO_VBE_MODES, MARGO_VRAM_SIZE,
    MODE13H_MEMORY_SIZE, Margo, TextFrame, VGA_MODE13H_BASE, VGA_TEXT_BASE, VGA_TEXT_MEMORY_SIZE,
    VgaTextMode, VideoMode, bytes_per_pixel, pixel_format, vbe_mode,
};
use thiserror::Error;

mod pic;
mod pit;

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
/// Fixed load segment for a .COM: PSP at linear 0x1000, clear of the IVT, the
/// BIOS data area, the BIOS stub at 0x0600, and the boot result block at 0x9000.
const DOS_LOAD_SEGMENT: u16 = 0x0100;

#[derive(Debug, Error)]
pub enum MachineError {
    #[error(transparent)]
    Bus(#[from] BusError),
    #[error(transparent)]
    Cpu(#[from] CpuError),
    #[error(transparent)]
    Dos(#[from] izarravm_dos::DosError),
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
    DosExit { code: u8 },
}

/// The OPL3 renders at this native rate; the Resonique 2 DAC outputs at 44100.
const OPL_NATIVE_HZ: u32 = 49_716;
const DAC_HZ: u32 = 44_100;
/// Standard PC PIT input clock frequency.
const PIT_INPUT_HZ: u32 = 1_193_182;

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
    pending_soft_int: Option<u8>, // software-INT vector awaiting deferred dispatch
    dos: izarravm_dos::DosKernel, // DOS kernel state: open files, drive, stdin/stdout
    rom: Vec<u8>,
    serial: SerialPort,
    device_ports: DevicePorts,
    pic: pic::Pic8259Pair,
    pit: pit::Pit,
    pit_clocks: f64, // fractional PIT input clocks owed to the counters
    opl: OplChip,
    resampler: Resampler,
    opl_micros: f64, // fractional microseconds owed to the OPL timers
    margo_ns: f64,   // fractional nanoseconds owed to the Margo busy countdown
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
            pending_soft_int: None,
            dos: izarravm_dos::DosKernel::default(),
            rom: rom.to_vec(),
            serial: SerialPort::default(),
            device_ports: DevicePorts::default(),
            pic: pic::Pic8259Pair::default(),
            pit: pit::Pit::default(),
            pit_clocks: 0.0,
            opl: OplChip::default(),
            resampler: Resampler::new(OPL_NATIVE_HZ, DAC_HZ),
            opl_micros: 0.0,
            margo_ns: 0.0,
            trace: BusTrace::default(),
            elapsed_clocks: 0,
        };
        // The Margo LFB aperture is decoded before RAM, so system memory must
        // stay below it. Validated config caps memory far under this bound.
        debug_assert!(
            machine.memory.len() as u64 <= u64::from(MARGO_LFB_BASE),
            "system RAM overlaps the Margo LFB aperture at 0xE0000000"
        );
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
            pending_soft_int: None,
            dos: izarravm_dos::DosKernel::default(),
            rom: vec![0; BIOS_ROM_SIZE],
            serial: SerialPort::default(),
            device_ports: DevicePorts::default(),
            pic: pic::Pic8259Pair::default(),
            pit: pit::Pit::default(),
            pit_clocks: 0.0,
            opl: OplChip::default(),
            resampler: Resampler::new(OPL_NATIVE_HZ, DAC_HZ),
            opl_micros: 0.0,
            margo_ns: 0.0,
            trace: BusTrace::default(),
            elapsed_clocks: 0,
        };
        // The Margo LFB aperture is decoded before RAM, so system memory must
        // stay below it. Validated config caps memory far under this bound.
        debug_assert!(
            machine.memory.len() as u64 <= u64::from(MARGO_LFB_BASE),
            "system RAM overlaps the Margo LFB aperture at 0xE0000000"
        );

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

    /// Build a machine with a .COM program loaded and ready to run. The program is
    /// placed at DOS_LOAD_SEGMENT with its PSP, and the CPU is set to its entry
    /// (CS=DS=ES=SS=segment, IP=0x100, SP=0xFFFE). Run with run_until_halt_or_cycles
    /// and read dos_output plus the DosExit stop reason.
    ///
    /// Entry eflags has IF clear, unlike real DOS which hands a .COM control with
    /// interrupts enabled. This slice installs no BIOS interrupt handlers (IVT[8]
    /// and friends are zero), so a program that wants hardware IRQs must set them up
    /// and STI itself; the BIOS IVT and an interrupts-enabled handoff come with a
    /// later slice.
    pub fn new_dos_com(profile: MachineProfile, image: &[u8]) -> Result<Self, MachineError> {
        let mut machine = Self {
            memory: Memory::from_mib(profile.memory_mib)?,
            profile,
            cpu: Cpu386::default(),
            video: VgaTextMode::default(),
            margo: Margo::default(),
            margo_active: false,
            pending_soft_int: None,
            dos: izarravm_dos::DosKernel::default(),
            rom: vec![0; BIOS_ROM_SIZE],
            serial: SerialPort::default(),
            device_ports: DevicePorts::default(),
            pic: pic::Pic8259Pair::default(),
            pit: pit::Pit::default(),
            pit_clocks: 0.0,
            opl: OplChip::default(),
            resampler: Resampler::new(OPL_NATIVE_HZ, DAC_HZ),
            opl_micros: 0.0,
            margo_ns: 0.0,
            trace: BusTrace::default(),
            elapsed_clocks: 0,
        };
        debug_assert!(
            machine.memory.len() as u64 <= u64::from(MARGO_LFB_BASE),
            "system RAM overlaps the Margo LFB aperture at 0xE0000000"
        );
        install_boot_bios_stubs(&mut machine.memory)?;

        let entry = izarravm_dos::load_com(image, &mut machine.memory, DOS_LOAD_SEGMENT)?;
        let segment = SegmentRegister::real(entry.segment);
        for index in [
            SegmentIndex::Cs,
            SegmentIndex::Ds,
            SegmentIndex::Es,
            SegmentIndex::Ss,
        ] {
            machine.cpu.registers.set_segment(index, segment);
        }
        machine.cpu.registers.eip = u32::from(entry.ip);
        machine.cpu.registers.set_esp(u32::from(entry.sp));
        machine.cpu.registers.eflags = 0x0000_0002;
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
            pic: &mut self.pic,
            pit: &mut self.pit,
            opl: &mut self.opl,
            trace: &mut self.trace,
            pending_soft_int: &mut self.pending_soft_int,
            wait_states: self.profile.wait_states,
        }
    }

    pub fn read_physical_u8(&mut self, address: u32) -> u8 {
        let bus = self.make_bus();
        bus.read_memory_bytes(address, 1).map(|b| b[0]).unwrap_or(0)
    }

    /// Last byte written to a passive I/O port (such as 0x80, the POST diagnostic
    /// port), or None if the port address is not in the passive port map. A
    /// decoded but never written port reads its default, not None.
    pub fn io_port(&self, port: u16) -> Option<u8> {
        self.device_ports.read_port(port)
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

    /// Service the host side of an `INT 10h` after the instruction retires.
    /// The CPU registers are intact here: a software interrupt only pushes
    /// flags/CS/IP.
    fn handle_int10(&mut self) {
        let ax = self.cpu.registers.eax() as u16;
        if ax == 0x0013 {
            self.video.set_mode13h();
            self.margo_active = false;
            return;
        }
        if (ax >> 8) == 0x4f {
            self.handle_vbe(ax as u8);
        }
    }

    /// Service a DOS software interrupt (INT 20h or INT 21h) host-side after the
    /// instruction retires. The CPU registers are intact here (a software interrupt
    /// only pushes flags/CS/IP), so the kernel reads and writes them through a
    /// marshalled DosRegs. IVT[0x20]/[0x21] is an IRET stub, so the next cycle
    /// returns to the caller with the results in place. DOS services are emulated
    /// host-side (HLE); the hardware INT path is otherwise real. Returns Some(code)
    /// when the program should terminate.
    fn handle_dos_int(&mut self, vector: u8) -> Result<Option<u8>, izarravm_dos::DosError> {
        let mut regs = izarravm_dos::DosRegs {
            ax: self.cpu.registers.eax() as u16,
            bx: self.cpu.registers.ebx() as u16,
            cx: self.cpu.registers.ecx() as u16,
            dx: self.cpu.registers.edx() as u16,
            si: self.cpu.registers.esi() as u16,
            di: self.cpu.registers.edi() as u16,
            ds: self.cpu.registers.segment(SegmentIndex::Ds).selector,
            es: self.cpu.registers.segment(SegmentIndex::Es).selector,
            cf: self.cpu.registers.eflags & 0x1 != 0,
            zf: self.cpu.registers.eflags & 0x40 != 0,
        };
        let action = self.dos.dispatch(vector, &mut regs, &mut self.memory)?;
        // Marshal the result back. Every general-purpose register is written
        // unconditionally so a later slice that returns a value in any of them (for
        // example AH=3Fh returns the byte count in CX) needs no change here. Only the
        // low 16 bits are touched, preserving each e-register's high half. DS and ES
        // are inputs to INT 21h; the rare functions that return a segment (AH=2Fh in
        // ES) add their own write-back when implemented.
        //
        // The INT pushed FLAGS/CS/IP; after it the real-mode frame is [SS:SP]=IP,
        // [SS:SP+2]=CS, [SS:SP+4]=FLAGS. handle_dos_int does not move the guest
        // stack, so SS:SP+4 is the FLAGS image the IRET stub will pop. Returned
        // flags must go there: writing live eflags would be discarded by that IRET.
        let flags_addr = self.cpu.registers.segment(SegmentIndex::Ss).base
            + u32::from((self.cpu.registers.esp() as u16).wrapping_add(4));
        let r = &mut self.cpu.registers;
        r.set_eax((r.eax() & 0xffff_0000) | u32::from(regs.ax));
        r.set_ebx((r.ebx() & 0xffff_0000) | u32::from(regs.bx));
        r.set_ecx((r.ecx() & 0xffff_0000) | u32::from(regs.cx));
        r.set_edx((r.edx() & 0xffff_0000) | u32::from(regs.dx));
        r.set_esi((r.esi() & 0xffff_0000) | u32::from(regs.si));
        r.set_edi((r.edi() & 0xffff_0000) | u32::from(regs.di));
        // CF is bit 0, ZF is bit 6; FLAG_CF/FLAG_ZF are private to the cpu crate.
        let mut flags = self.memory.read_u16(flags_addr as usize)?;
        flags = if regs.cf {
            flags | 0x0001
        } else {
            flags & !0x0001
        };
        flags = if regs.zf {
            flags | 0x0040
        } else {
            flags & !0x0040
        };
        self.memory.write_u16(flags_addr as usize, flags)?;
        Ok(match action {
            izarravm_dos::DosAction::Continue => None,
            izarravm_dos::DosAction::Exit(code) => Some(code),
        })
    }

    /// The bytes the DOS kernel has written to standard output (INT 21h AH=09h and
    /// the character-output calls). Captured host-side for headless runs; not yet
    /// rendered to the VGA text mode.
    pub fn dos_output(&self) -> &[u8] {
        self.dos.stdout()
    }

    /// Replace the DOS standard-input buffer, consumed front to back by the
    /// character-input calls. An exhausted buffer yields ^Z (EOF) for the blocking
    /// reads (AH=01h/08h); AH=06h reports an empty buffer through ZF.
    pub fn set_dos_stdin(&mut self, bytes: &[u8]) {
        self.dos.set_stdin(bytes);
    }

    /// Mount a host directory as the guest C: drive for INT 21h file calls.
    pub fn mount_c_drive(&mut self, drive: izarravm_dos::HostDrive) {
        self.dos.mount_c(drive);
    }

    /// VBE (`INT 10h`, `AH=4Fh`). `function` is `AL`. Unimplemented functions
    /// leave `AX` unchanged, so `AL != 0x4F` signals "not supported" to the guest.
    fn handle_vbe(&mut self, function: u8) {
        match function {
            0x00 => self.vbe_controller_info(),
            0x01 => self.vbe_mode_info(),
            0x02 => self.vbe_set_mode(),
            0x03 => self.vbe_current_mode(),
            _ => {}
        }
    }

    fn vbe_controller_info(&mut self) {
        let es = self.cpu.registers.segment(SegmentIndex::Es).selector;
        let di = self.cpu.registers.edi() as u16;
        let mut block = [0u8; 256];
        block[0x00..0x04].copy_from_slice(b"VESA");
        block[0x04..0x06].copy_from_slice(&0x0200u16.to_le_bytes()); // VbeVersion
        block[0x12..0x14].copy_from_slice(&64u16.to_le_bytes()); // TotalMemory: 64 * 64 KB = 4 MB

        // The mode list lives inside the block at offset 0x14. VideoModePtr is a
        // real-mode far pointer the guest decodes as seg:off, so it carries the
        // ES selector, not the linear base. vbe_block_ptr() uses the base for the
        // write-side physical address; in real mode the two agree (base = selector << 4).
        let list_offset = di.wrapping_add(0x14);
        let video_mode_ptr = (u32::from(es) << 16) | u32::from(list_offset);
        block[0x0e..0x12].copy_from_slice(&video_mode_ptr.to_le_bytes());

        let mut pos = 0x14;
        for mode in MARGO_VBE_MODES {
            block[pos..pos + 2].copy_from_slice(&mode.number.to_le_bytes());
            pos += 2;
        }
        block[pos..pos + 2].copy_from_slice(&0xffffu16.to_le_bytes());

        let addr = self.vbe_block_ptr();
        self.write_guest_block(addr, &block);
        self.set_vbe_status(0x004f);
    }

    /// Set the `AX` low word to a VBE status (`0x004F` ok, `0x014F` failed),
    /// preserving the high word.
    fn set_vbe_status(&mut self, status: u16) {
        let eax = (self.cpu.registers.eax() & 0xffff_0000) | u32::from(status);
        self.cpu.registers.set_eax(eax);
    }

    fn vbe_set_mode(&mut self) {
        let mode = self.cpu.registers.ebx() as u16 & 0x01ff;
        if self.margo.set_mode(mode) {
            self.margo_active = true;
            self.set_vbe_status(0x004f);
        } else {
            self.set_vbe_status(0x014f);
        }
    }

    fn vbe_current_mode(&mut self) {
        let mode = if self.margo_active {
            self.margo.display().mode
        } else {
            0x0003 // VBE mode 0003h: standard 80x25 text fallback
        };
        let ebx = (self.cpu.registers.ebx() & 0xffff_0000) | u32::from(mode);
        self.cpu.registers.set_ebx(ebx);
        self.set_vbe_status(0x004f);
    }

    /// Real-mode `ES:DI` of the caller's info block, as a physical address.
    fn vbe_block_ptr(&self) -> u32 {
        let es = self.cpu.registers.segment(SegmentIndex::Es).base;
        let di = self.cpu.registers.edi() as u16;
        es + u32::from(di)
    }

    fn write_guest_block(&mut self, addr: u32, bytes: &[u8]) {
        for (index, &byte) in bytes.iter().enumerate() {
            self.write_physical_u8(addr + index as u32, byte);
        }
    }

    fn vbe_mode_info(&mut self) {
        let mode = self.cpu.registers.ecx() as u16 & 0x01ff;
        let Some(info) = vbe_mode(mode) else {
            self.set_vbe_status(0x014f);
            return;
        };
        let pitch = (info.width * bytes_per_pixel(info.bpp)) as u16;
        let mut block = [0u8; 256];
        block[0x00..0x02].copy_from_slice(&0x009bu16.to_le_bytes()); // ModeAttributes: supported, color, graphics, linear-fb
        block[0x10..0x12].copy_from_slice(&pitch.to_le_bytes()); // BytesPerScanLine
        block[0x12..0x14].copy_from_slice(&(info.width as u16).to_le_bytes()); // XResolution
        block[0x14..0x16].copy_from_slice(&(info.height as u16).to_le_bytes()); // YResolution
        block[0x18] = 1; // NumberOfPlanes
        block[0x19] = info.bpp as u8; // BitsPerPixel
        block[0x1b] = 4; // MemoryModel: packed pixel
        if let Some(fmt) = pixel_format(info.bpp) {
            block[0x1f] = fmt.r.size as u8; // RedMaskSize
            block[0x20] = fmt.r.pos as u8; // RedFieldPosition
            block[0x21] = fmt.g.size as u8; // GreenMaskSize
            block[0x22] = fmt.g.pos as u8; // GreenFieldPosition
            block[0x23] = fmt.b.size as u8; // BlueMaskSize
            block[0x24] = fmt.b.pos as u8; // BlueFieldPosition
            block[0x25] = fmt.x.size as u8; // RsvdMaskSize
            block[0x26] = fmt.x.pos as u8; // RsvdFieldPosition
        }
        block[0x28..0x2c].copy_from_slice(&MARGO_LFB_BASE.to_le_bytes()); // PhysBasePtr
        let addr = self.vbe_block_ptr();
        self.write_guest_block(addr, &block);
        self.set_vbe_status(0x004f);
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

    /// Advance time-based devices by `clocks` of CPU time, carrying fractional
    /// remainders forward for the OPL timers (microseconds), the PIT counters,
    /// and the Margo blit engine (nanoseconds).
    fn advance_devices(&mut self, clocks: u64) {
        self.opl_micros += clocks as f64 * 1_000_000.0 / self.profile.clock_hz as f64;
        let whole = self.opl_micros.floor();
        self.opl.advance_micros(whole as u64);
        self.opl_micros -= whole;

        self.pit_clocks += clocks as f64 * f64::from(PIT_INPUT_HZ) / self.profile.clock_hz as f64;
        let whole = self.pit_clocks.floor();
        self.pit_clocks -= whole;
        let edges = self.pit.tick(whole as u64);
        for _ in 0..edges {
            self.pic.request(0); // channel 0 OUT rising edge is IRQ0
        }

        self.margo_ns += clocks as f64 * 1_000_000_000.0 / self.profile.clock_hz as f64;
        let whole_ns = self.margo_ns.floor();
        self.margo.advance_busy(whole_ns as u64);
        self.margo_ns -= whole_ns;
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

    /// Drive a PIT counter's GATE line. The PC ties GATE0/GATE1 high; the sound
    /// slice wires GATE2 from port 0x61. Exposed now so the GATE-triggered modes
    /// have a caller outside tests.
    pub fn set_timer_gate(&mut self, channel: usize, level: bool) {
        self.pit.set_gate(channel, level);
    }

    /// Input CLK pulses until channel 0 produces its next OUT rising edge, or None
    /// if the counter cannot fire from its current state. Used by the HLT
    /// fast-forward path added in Task 2b-2.
    pub fn clocks_until_timer0_irq(&self) -> Option<u64> {
        self.pit.clocks_until_channel0_irq()
    }

    /// CPU clocks to advance while halted so the next channel-0 IRQ0 lands, or None
    /// if nothing can wake the CPU (so HLT is a genuine halt). Clamped to the
    /// deadline and to at least one clock, so the run loop always makes progress.
    fn next_timer_wake(&self, deadline: u64) -> Option<u64> {
        if !self.cpu.interrupts_enabled() || !self.pic.irq0_unmasked() {
            return None;
        }
        let pit_delta = self.clocks_until_timer0_irq()?;
        let cpu_delta = (u128::from(pit_delta) * u128::from(self.profile.clock_hz))
            .div_ceil(u128::from(PIT_INPUT_HZ)) as u64;
        let remaining = deadline.saturating_sub(self.elapsed_clocks);
        if remaining == 0 {
            return None;
        }
        Some(cpu_delta.max(1).min(remaining))
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
            self.pending_soft_int = None;
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
                    pit,
                    opl,
                    trace,
                    pending_soft_int,
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
                    pit,
                    opl,
                    trace,
                    pending_soft_int,
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
                    if let Some(vector) = self.pending_soft_int {
                        match vector {
                            0x10 => self.handle_int10(),
                            0x20 | 0x21 => match self.handle_dos_int(vector) {
                                Ok(Some(code)) => return Ok(StopReason::DosExit { code }),
                                Ok(None) => {}
                                Err(error) => {
                                    return Ok(StopReason::CpuError(format!(
                                        "DOS INT {vector:#04x}: {error}"
                                    )));
                                }
                            },
                            _ => {}
                        }
                    }
                    if outcome.halted {
                        match self.next_timer_wake(deadline) {
                            Some(wake_step) => {
                                self.elapsed_clocks += wake_step;
                                self.advance_devices(wake_step);
                            }
                            None => return Ok(StopReason::Halted),
                        }
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
    pit: &'a mut pit::Pit,
    opl: &'a mut OplChip,
    trace: &'a mut BusTrace,
    pending_soft_int: &'a mut Option<u8>,
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
        if let Some(value) = self.pit.read_port(port) {
            return Ok(u32::from(value));
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
            || self.pit.write_port(port, value as u8)
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

    fn interrupt_acknowledge(&mut self, vector: u8, _ax: u16) -> Result<(), BusError> {
        self.trace.push(BusCycle::new(
            BusAccessKind::InterruptAcknowledge,
            u32::from(vector),
            BusWidth::Byte,
            self.wait_states.io,
        ));
        // Record the software-INT vector for the deferred dispatch in the run loop,
        // which has the CPU registers and memory to service it. 0x10 is the BIOS
        // video service; 0x20/0x21 are the DOS kernel. Vector 0x10 reaches here
        // only from a software INT today (the CPU never faults with vector 0x10);
        // revisit if an x87 #MF is added.
        if matches!(vector, 0x10 | 0x20 | 0x21) {
            *self.pending_soft_int = Some(vector);
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
    for vector in [0x10, 0x13, 0x20, 0x21] {
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
    fn io_port_reports_last_post_write() {
        // mov al,0x42; out 0x80,al; hlt
        let mut machine = Machine::new(
            MachineProfile::i386dx25(16, VideoCard::Et4000Ax),
            rom_with_code(&[0xb0, 0x42, 0xe6, 0x80, 0xf4]),
        )
        .unwrap();
        machine.run_until_halt_or_cycles(10_000).unwrap();
        assert_eq!(machine.io_port(0x80), Some(0x42));
        assert_eq!(machine.io_port(0x0100), None); // outside the passive port map
    }

    fn read_u16(machine: &mut Machine, addr: u32) -> u16 {
        u16::from(machine.read_physical_u8(addr))
            | (u16::from(machine.read_physical_u8(addr + 1)) << 8)
    }

    fn read_u32(machine: &mut Machine, addr: u32) -> u32 {
        u32::from(read_u16(machine, addr)) | (u32::from(read_u16(machine, addr + 2)) << 16)
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
            let mut bus = machine.make_bus();
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

        // The budget covers the timer test's idle (ten ticks of about 11932 PIT
        // clocks, near 2.5M CPU clocks) plus the setup, matching the headless runner.
        let reason = machine.run_until_halt_or_cycles(5_000_000).unwrap();
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
    fn vga_mode_set_clears_a_latched_margo_display() {
        let rom = rom_with_code(&[
            0xb8, 0x13, 0x00, // mov ax, 0013h
            0xcd, 0x10, // int 10h
            0xf4, // hlt
        ]);
        let mut machine =
            Machine::new(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), rom).unwrap();

        // Host path latches Margo as the active display.
        machine.set_margo_mode_640x480x8();
        assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);

        // A guest VGA mode-set must hand the display back to VGA.
        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        assert_eq!(machine.active_display(), ActiveDisplay::Mode13h);
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
    fn vbe_set_mode_selects_a_margo_mode() {
        let rom = rom_with_code(&[
            0xb8, 0x02, 0x4f, // mov ax, 4F02h
            0xbb, 0x01, 0x41, // mov bx, 0101h | 4000h (LFB)
            0xcd, 0x10, // int 10h
            0xf4, // hlt
        ]);
        let mut machine =
            Machine::new(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), rom).unwrap();

        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);
        assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);
        assert_eq!(machine.margo().display().width, 640);
        assert_eq!(machine.margo().display().height, 480);
    }

    #[test]
    fn vbe_set_mode_then_vga_mode_follows_the_display() {
        let rom = rom_with_code(&[
            0xb8, 0x02, 0x4f, // mov ax, 4F02h
            0xbb, 0x01, 0x41, // mov bx, 0101h | 4000h
            0xcd, 0x10, // int 10h
            0xb8, 0x13, 0x00, // mov ax, 0013h
            0xcd, 0x10, // int 10h
            0xf4, // hlt
        ]);
        let mut machine =
            Machine::new(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), rom).unwrap();

        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        // The VGA mode-set hands the display back to VGA, but the 4F02 call must
        // still have set the Margo mode (width stays set; only margo_active clears).
        assert_eq!(machine.margo().display().width, 640);
        assert_eq!(machine.active_display(), ActiveDisplay::Mode13h);
    }

    #[test]
    fn vbe_set_mode_accepts_hi_color_modes() {
        let rom = rom_with_code(&[
            0xb8, 0x02, 0x4f, // mov ax, 4F02h
            0xbb, 0x11, 0x41, // mov bx, 0111h | 4000h (640x480x16, linear frame buffer)
            0xcd, 0x10, // int 10h
            0xf4, // hlt
        ]);
        let mut machine =
            Machine::new(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), rom).unwrap();

        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);
        assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);
        assert_eq!(machine.margo().display().bpp, 16);
    }

    #[test]
    fn vbe_current_mode_returns_the_set_mode() {
        let rom = rom_with_code(&[
            0xb8, 0x02, 0x4f, // mov ax, 4F02h
            0xbb, 0x01, 0x41, // mov bx, 0101h | 4000h
            0xcd, 0x10, // int 10h
            0xb8, 0x03, 0x4f, // mov ax, 4F03h
            0xcd, 0x10, // int 10h
            0xf4, // hlt
        ]);
        let mut machine =
            Machine::new(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), rom).unwrap();

        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);
        assert_eq!(machine.cpu().registers.ebx() as u16, 0x0101);
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
            margo: &mut machine.margo,
            rom: &machine.rom,
            serial: &mut machine.serial,
            device_ports: &mut machine.device_ports,
            pic: &mut machine.pic,
            pit: &mut machine.pit,
            opl: &mut machine.opl,
            trace: &mut machine.trace,
            pending_soft_int: &mut machine.pending_soft_int,
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

    fn boot_image_with(code: &[u8]) -> Vec<u8> {
        let mut image = vec![0; BOOT_IMAGE_SIZE];
        image[..code.len()].copy_from_slice(code);
        image[510] = 0x55;
        image[511] = 0xaa;
        image
    }

    #[test]
    fn hlt_wakes_on_pit_timer_tick() {
        // Boot code: init the PIC, unmask IRQ0, program channel 0 (mode 3, count 1000),
        // install IVT[8] -> a handler that bumps [0x0500] and EOIs, sti, hlt, then
        // cli, hlt. The run loop must fast-forward to the IRQ0 edge and wake the CPU.
        // The count is large enough that the handler finishes long before the next
        // tick, so the cli after hlt runs and the program reaches a genuine halt.
        let code: &[u8] = &[
            0xb0, 0x11, 0xe6, 0x20, 0xb0, 0x08, 0xe6, 0x21, 0xb0, 0x04, 0xe6, 0x21, 0xb0, 0x01,
            0xe6, 0x21, 0xb0, 0xfe, 0xe6, 0x21, 0xb0, 0x36, 0xe6, 0x43, 0xb0, 0xe8, 0xe6, 0x40,
            0xb0, 0x03, 0xe6, 0x40, 0xc7, 0x06, 0x20, 0x00, 0x30, 0x7c, 0xc7, 0x06, 0x22, 0x00,
            0x00, 0x00, 0xfb, 0xf4, 0xfa, 0xf4, 0xff, 0x06, 0x00, 0x05, 0xb0, 0x20, 0xe6, 0x20,
            0xcf,
        ];
        let mut machine = Machine::new_boot_image(
            MachineProfile::i386dx25(16, VideoCard::Et4000Ax),
            boot_image_with(code),
        )
        .unwrap();

        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();

        assert_eq!(reason, StopReason::Halted);
        let tick = u16::from_le_bytes([
            machine.memory().as_slice()[0x0500],
            machine.memory().as_slice()[0x0501],
        ]);
        assert_eq!(tick, 1, "the IRQ0 handler should have run once");
        // One tick is about 1000 PIT clocks, near 21000 CPU clocks at 25 MHz, so a
        // real fast-forward clears this slack floor while a no-op halt would not.
        assert!(
            machine.elapsed_clocks() > 10_000,
            "the fast-forward should have advanced emulated time across the tick interval"
        );
    }

    #[test]
    fn boot_suite_reports_timer_irq0_pass() {
        let mut machine = Machine::new_boot_image(
            MachineProfile::i386dx25(16, VideoCard::Et4000Ax),
            izarravm_firmware::X86_BOOT_TEST_IMAGE,
        )
        .unwrap();
        let reason = machine.run_until_halt_or_cycles(5_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);

        let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();
        assert!(
            results.records.iter().any(|record| {
                record.status == izarravm_firmware::SuiteRecordStatus::Pass
                    && record.name == "timer.irq0"
            }),
            "boot suite should report PASS timer.irq0"
        );
        // The timer idle genuinely advanced emulated time (ten ticks of ~11932
        // input clocks each), not spun instantly.
        assert!(machine.elapsed_clocks() > 1_500_000);
    }

    #[test]
    fn cli_hlt_is_a_genuine_halt() {
        // With interrupts off, HLT must still halt immediately, not spin.
        let mut machine = Machine::new(
            MachineProfile::i386dx25(16, VideoCard::Et4000Ax),
            rom_with_code(&[0xfa, 0xf4]), // cli; hlt
        )
        .unwrap();
        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
    }

    #[test]
    fn pit_channel0_raises_irq0_while_running() {
        // cli; jmp $ keeps the CPU spinning with interrupts off, so advance_devices
        // ticks the PIT but the raised IRQ0 stays pending (never acknowledged).
        let mut machine = Machine::new(
            MachineProfile::i386dx25(16, VideoCard::Et4000Ax),
            rom_with_code(&[0xfa, 0xeb, 0xfe]),
        )
        .unwrap();
        with_bus(&mut machine, |bus| {
            bus.write_io(0x43, BusWidth::Byte, 0x36).unwrap(); // counter 0, mode 3
            bus.write_io(0x40, BusWidth::Byte, 0x04).unwrap(); // count low
            bus.write_io(0x40, BusWidth::Byte, 0x00).unwrap(); // count high -> 4
        });
        machine.run_cycles(4000).unwrap();
        let pending = with_bus(&mut machine, |bus| bus.interrupt_pending());
        assert!(
            pending,
            "channel 0 should have raised IRQ0 over 4000 cycles"
        );
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

    #[test]
    fn vbe_mode_info_fills_the_block() {
        // ES = 0x4000 -> physical 0x40000, DI = 0.
        let rom = rom_with_code(&[
            0xb8, 0x00, 0x40, // mov ax, 4000h
            0x8e, 0xc0, // mov es, ax
            0xbf, 0x00, 0x00, // mov di, 0
            0xb8, 0x01, 0x4f, // mov ax, 4F01h
            0xb9, 0x01, 0x01, // mov cx, 0101h
            0xcd, 0x10, // int 10h
            0xf4, // hlt
        ]);
        let mut machine =
            Machine::new(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), rom).unwrap();

        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);

        let base = 0x40000;
        assert_eq!(read_u16(&mut machine, base + 0x10), 640); // BytesPerScanLine
        assert_eq!(read_u16(&mut machine, base + 0x12), 640); // XResolution
        assert_eq!(read_u16(&mut machine, base + 0x14), 480); // YResolution
        assert_eq!(machine.read_physical_u8(base + 0x19), 8); // BitsPerPixel
        assert_eq!(read_u32(&mut machine, base + 0x28), MARGO_LFB_BASE); // PhysBasePtr
    }

    #[test]
    fn vbe_controller_info_fills_the_block() {
        let rom = rom_with_code(&[
            0xb8, 0x00, 0x40, // mov ax, 4000h
            0x8e, 0xc0, // mov es, ax
            0xbf, 0x00, 0x00, // mov di, 0
            0xb8, 0x00, 0x4f, // mov ax, 4F00h
            0xcd, 0x10, // int 10h
            0xf4, // hlt
        ]);
        let mut machine =
            Machine::new(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), rom).unwrap();

        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);

        let base = 0x40000;
        assert_eq!(machine.read_physical_u8(base), b'V');
        assert_eq!(machine.read_physical_u8(base + 1), b'E');
        assert_eq!(machine.read_physical_u8(base + 2), b'S');
        assert_eq!(machine.read_physical_u8(base + 3), b'A');
        assert_eq!(read_u16(&mut machine, base + 0x04), 0x0200); // VbeVersion
        assert_eq!(read_u16(&mut machine, base + 0x12), 64); // TotalMemory (64 KB units)
        // OemStringPtr and Capabilities are intentionally left zero.
        assert_eq!(read_u32(&mut machine, base + 0x06), 0); // OemStringPtr
        assert_eq!(read_u32(&mut machine, base + 0x0a), 0); // Capabilities

        // VideoModePtr (seg:off) must point at the mode list, which lists every
        // entry in MARGO_VBE_MODES (8bpp then hi-color then true-color) and ends
        // with the 0xffff terminator.
        let ptr = read_u32(&mut machine, base + 0x0e);
        let list = (((ptr >> 16) & 0xffff) << 4) + (ptr & 0xffff);
        let expected = [
            0x0100, 0x0101, 0x0103, 0x0105, 0x0110, 0x0111, 0x0113, 0x0114, 0x0116, 0x0117, 0x014a,
            0x014c, 0x014e, 0xffff,
        ];
        for (i, &mode) in expected.iter().enumerate() {
            assert_eq!(read_u16(&mut machine, list + (i * 2) as u32), mode);
        }
    }

    #[test]
    fn vbe_mode_info_rejects_unknown_modes() {
        let rom = rom_with_code(&[
            0xb8, 0x00, 0x40, // mov ax, 4000h
            0x8e, 0xc0, // mov es, ax
            0xbf, 0x00, 0x00, // mov di, 0
            0xb8, 0x01, 0x4f, // mov ax, 4F01h
            0xb9, 0x12, 0x01, // mov cx, 0112h (640x480x24, packed 24-bit not provided)
            0xcd, 0x10, // int 10h
            0xf4, // hlt
        ]);
        let mut machine =
            Machine::new(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), rom).unwrap();

        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        assert_eq!(machine.cpu().registers.eax() as u16, 0x014f);
    }

    // Write/read a 32-bit Margo register through the MMIO aperture.
    fn write_mmio_reg(machine: &mut Machine, offset: u32, value: u32) {
        for (i, b) in value.to_le_bytes().into_iter().enumerate() {
            machine.write_physical_u8(MARGO_MMIO_BASE + offset + i as u32, b);
        }
    }

    fn read_mmio_reg(machine: &mut Machine, offset: u32) -> u32 {
        let mut value = 0u32;
        for i in 0..4 {
            value |= u32::from(machine.read_physical_u8(MARGO_MMIO_BASE + offset + i)) << (8 * i);
        }
        value
    }

    #[test]
    fn copy_through_the_mmio_aperture_moves_vram_and_times_busy() {
        let mut machine = test_machine();
        // Seed a 2x2 source rectangle at (0, 0), pitch 640, depth 1, through the LFB.
        machine.write_physical_u8(MARGO_LFB_BASE, 0xa1); // (0,0)
        machine.write_physical_u8(MARGO_LFB_BASE + 1, 0xa2); // (1,0)
        machine.write_physical_u8(MARGO_LFB_BASE + 640, 0xa3); // (0,1)
        machine.write_physical_u8(MARGO_LFB_BASE + 641, 0xa4); // (1,1)

        // Copy it to (10, 10) on the same surface (no overlap).
        write_mmio_reg(&mut machine, 0x100, 0); // DST_BASE
        write_mmio_reg(&mut machine, 0x104, 640); // DST_PITCH
        write_mmio_reg(&mut machine, 0x108, 0); // SRC_BASE
        write_mmio_reg(&mut machine, 0x10c, 640); // SRC_PITCH
        write_mmio_reg(&mut machine, 0x110, 1); // DEPTH
        write_mmio_reg(&mut machine, 0x114, (10 << 16) | 10); // DST_XY: y=10, x=10
        write_mmio_reg(&mut machine, 0x118, 0); // SRC_XY: (0,0)
        write_mmio_reg(&mut machine, 0x11c, (2 << 16) | 2); // DIM: h=2, w=2
        write_mmio_reg(&mut machine, 0x128, 0xcc); // ROP: SRCCOPY
        write_mmio_reg(&mut machine, 0x130, 0); // FLAGS: none
        write_mmio_reg(&mut machine, 0x150, 0x02); // COMMAND: COPY

        // Destination corners hold the source bytes (read back through the LFB).
        assert_eq!(
            machine.read_physical_u8(MARGO_LFB_BASE + 10 * 640 + 10),
            0xa1
        );
        assert_eq!(
            machine.read_physical_u8(MARGO_LFB_BASE + 11 * 640 + 11),
            0xa4
        );
        // BUSY is set right after the command.
        assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);

        // 4 pixels -> busy_ns = 100 + 4*10 = 140 ns. At 25 MHz (40 ns/clock),
        // three clocks (120 ns) leave it busy; the fourth clears it.
        machine.advance_devices(3);
        assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
        machine.advance_devices(1);
        assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 0);
    }

    #[test]
    fn dos_com_prints_string_and_exits() {
        // org 0x100: mov ah,9; mov dx,0x010c; int 21; mov ax,4c00; int 21; db 'Hi$'
        let com: &[u8] = &[
            0xb4, 0x09, 0xba, 0x0c, 0x01, 0xcd, 0x21, 0xb8, 0x00, 0x4c, 0xcd, 0x21, b'H', b'i',
            b'$',
        ];
        let mut machine =
            Machine::new_dos_com(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), com).unwrap();
        let reason = machine.run_until_halt_or_cycles(100_000).unwrap();
        assert_eq!(reason, StopReason::DosExit { code: 0 });
        assert_eq!(machine.dos_output(), b"Hi");
    }

    #[test]
    fn dos_com_exit_code_is_carried_through() {
        // org 0x100: mov ax,4c07; int 21
        let com: &[u8] = &[0xb8, 0x07, 0x4c, 0xcd, 0x21];
        let mut machine =
            Machine::new_dos_com(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), com).unwrap();
        let reason = machine.run_until_halt_or_cycles(100_000).unwrap();
        assert_eq!(reason, StopReason::DosExit { code: 7 });
        assert!(machine.dos_output().is_empty());
    }

    #[test]
    fn dos_com_unhandled_int21_returns_through_stub_and_exits() {
        // org 0x100: mov ah,0x30 (unhandled); int 21; mov ax,4c00; int 21
        let com: &[u8] = &[0xb4, 0x30, 0xcd, 0x21, 0xb8, 0x00, 0x4c, 0xcd, 0x21];
        let mut machine =
            Machine::new_dos_com(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), com).unwrap();
        let reason = machine.run_until_halt_or_cycles(100_000).unwrap();
        assert_eq!(reason, StopReason::DosExit { code: 0 });
        assert!(machine.dos_output().is_empty());
    }

    #[test]
    fn fill_through_the_mmio_aperture_writes_vram_and_times_busy() {
        let mut machine = test_machine();
        // Latch a 5x4 fill at (3, 2), pitch 640, depth 1, color 0xAB, solid.
        write_mmio_reg(&mut machine, 0x100, 0); // DST_BASE
        write_mmio_reg(&mut machine, 0x104, 640); // DST_PITCH
        write_mmio_reg(&mut machine, 0x110, 1); // DEPTH
        write_mmio_reg(&mut machine, 0x114, (2 << 16) | 3); // DST_XY: y=2, x=3
        write_mmio_reg(&mut machine, 0x11c, (4 << 16) | 5); // DIM: h=4, w=5
        write_mmio_reg(&mut machine, 0x120, 0xab); // FG_COLOR
        write_mmio_reg(&mut machine, 0x128, 0xf0); // ROP: PATCOPY
        write_mmio_reg(&mut machine, 0x150, 0x01); // COMMAND: FILL

        // VRAM filled (read the top-left filled pixel back through the LFB).
        let pixel = MARGO_LFB_BASE + 2 * 640 + 3;
        assert_eq!(machine.read_physical_u8(pixel), 0xab);
        // BUSY is set right after the command.
        assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);

        // 20 pixels -> busy_ns = 100 + 20*5 = 200 ns = 5 clocks at 25 MHz.
        // Four clocks (160 ns) leave it busy; the fifth clears it.
        machine.advance_devices(4);
        assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
        machine.advance_devices(1);
        assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 0);
    }

    #[test]
    fn dos_com_runs_the_committed_hello_fixture() {
        let mut machine = Machine::new_dos_com(
            MachineProfile::i386dx25(16, VideoCard::Et4000Ax),
            izarravm_firmware::HELLO_COM,
        )
        .unwrap();
        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::DosExit { code: 0 });
        assert_eq!(machine.dos_output(), b"Hello, world!\r\n");
    }

    #[test]
    fn dos_com_ah06_zf_reaches_the_guest() {
        // org 0x100: AH=06h DL=0xFF; INT 21h; JZ empty; echo AL via AH=02h; else '!'
        // Proves ZF returned by AH=06h survives the IRET (it is written to the pushed
        // FLAGS image, not just live eflags which the IRET would discard).
        let com: &[u8] = &[
            0xb4, 0x06, 0xb2, 0xff, 0xcd, 0x21, 0x74, 0x08, 0x88, 0xc2, 0xb4, 0x02, 0xcd, 0x21,
            0xeb, 0x06, 0xb2, 0x21, 0xb4, 0x02, 0xcd, 0x21, 0xb8, 0x00, 0x4c, 0xcd, 0x21,
        ];

        let mut available =
            Machine::new_dos_com(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), com).unwrap();
        available.set_dos_stdin(b"X");
        assert_eq!(
            available.run_until_halt_or_cycles(100_000).unwrap(),
            StopReason::DosExit { code: 0 }
        );
        assert_eq!(available.dos_output(), b"X"); // char path taken, AL echoed

        let mut empty =
            Machine::new_dos_com(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), com).unwrap();
        assert_eq!(
            empty.run_until_halt_or_cycles(100_000).unwrap(),
            StopReason::DosExit { code: 0 }
        );
        assert_eq!(empty.dos_output(), b"!"); // empty path taken (ZF=1)
    }

    #[test]
    fn dos_com_echoes_input() {
        // org 0x100: AH=01h; INT 21h (x2, each echoes); AH=4Ch exit
        let com: &[u8] = &[
            0xb4, 0x01, 0xcd, 0x21, 0xb4, 0x01, 0xcd, 0x21, 0xb8, 0x00, 0x4c, 0xcd, 0x21,
        ];
        let mut machine =
            Machine::new_dos_com(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), com).unwrap();
        machine.set_dos_stdin(b"hi");
        assert_eq!(
            machine.run_until_halt_or_cycles(100_000).unwrap(),
            StopReason::DosExit { code: 0 }
        );
        assert_eq!(machine.dos_output(), b"hi");
    }

    #[test]
    fn color_expand_data_through_the_mmio_aperture_draws_a_glyph_and_times_busy() {
        let mut machine = test_machine();
        // draw_glyph_8x8: an 8x8 glyph expanded at (10, 5), pitch 640, depth 1,
        // FG 0xAB, EXPAND_TRANSPARENT so clear bits leave the zeroed background.
        // Row 0 = 0x80 (only the leftmost pixel), row 1 = 0x01 (only the rightmost),
        // proving MSB-first ordering; the rest are blank.
        let glyph: [u8; 8] = [0x80, 0x01, 0, 0, 0, 0, 0, 0];

        write_mmio_reg(&mut machine, 0x100, 0); // DST_BASE
        write_mmio_reg(&mut machine, 0x104, 640); // DST_PITCH
        write_mmio_reg(&mut machine, 0x110, 1); // DEPTH
        write_mmio_reg(&mut machine, 0x114, (5 << 16) | 10); // DST_XY: y=5, x=10
        write_mmio_reg(&mut machine, 0x11c, (8 << 16) | 8); // DIM: 8x8
        write_mmio_reg(&mut machine, 0x120, 0xab); // FG_COLOR
        write_mmio_reg(&mut machine, 0x130, 0x04); // FLAGS: EXPAND_TRANSPARENT
        write_mmio_reg(&mut machine, 0x128, 0xcc); // ROP: SRCCOPY (S = expanded pixel)
        write_mmio_reg(&mut machine, 0x150, 0x03); // COMMAND: COLOR_EXPAND_DATA

        // Armed: BUSY set before any data, nothing drawn yet.
        assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
        assert_eq!(
            machine.read_physical_u8(MARGO_LFB_BASE + 5 * 640 + 10),
            0x00
        );

        // Stream the eight rows; the bits go in the high byte, MSB first.
        for (row, &bits) in glyph.iter().enumerate() {
            write_mmio_reg(&mut machine, 0x160, u32::from(bits) << 24); // MONO_DATA
            if row < 7 {
                // Still armed until the final word arrives.
                assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
            }
        }

        // Set bits painted FG; clear bits left untouched over the zeroed background.
        assert_eq!(
            machine.read_physical_u8(MARGO_LFB_BASE + 5 * 640 + 10),
            0xab
        ); // row 0, col 0
        assert_eq!(
            machine.read_physical_u8(MARGO_LFB_BASE + 6 * 640 + 17),
            0xab
        ); // row 1, col 7
        assert_eq!(
            machine.read_physical_u8(MARGO_LFB_BASE + 5 * 640 + 11),
            0x00
        ); // row 0, col 1 clear
        assert_eq!(
            machine.read_physical_u8(MARGO_LFB_BASE + 6 * 640 + 10),
            0x00
        ); // row 1, col 0 clear

        // 2 pixels written -> busy_ns = 100 + 2*5 = 110 ns. At 25 MHz (40 ns/clock),
        // two clocks (80 ns) leave it busy; the third clears it.
        assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
        machine.advance_devices(2);
        assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
        machine.advance_devices(1);
        assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 0);
    }

    #[test]
    fn dos_com_runs_the_committed_echo_fixture() {
        let mut machine = Machine::new_dos_com(
            MachineProfile::i386dx25(16, VideoCard::Et4000Ax),
            izarravm_firmware::ECHO_COM,
        )
        .unwrap();
        machine.set_dos_stdin(b"hi");
        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::DosExit { code: 0 });
        assert_eq!(machine.dos_output(), b"hi");
    }

    #[test]
    fn dos_com_reads_a_file_from_c_drive() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("HELLO.TXT"), b"File data 123").unwrap();
        let mut machine = Machine::new_dos_com(
            MachineProfile::i386dx25(16, VideoCard::Et4000Ax),
            izarravm_firmware::TYPE_COM,
        )
        .unwrap();
        machine.mount_c_drive(izarravm_dos::HostDrive::mount_c(dir.path()).unwrap());
        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::DosExit { code: 0 });
        assert_eq!(machine.dos_output(), b"File data 123");
    }

    #[test]
    fn line_through_the_mmio_aperture_draws_and_times_busy() {
        let mut machine = test_machine();
        // draw_line: a horizontal 5-pixel line at y=5 from x=10 to x=14, pitch 640,
        // depth 1, FG 0xAB. ROP 0xF0 (PATCOPY) draws solid; LINE has no source, so
        // the pattern (FG) is the right input, not SRCCOPY.
        write_mmio_reg(&mut machine, 0x100, 0); // DST_BASE
        write_mmio_reg(&mut machine, 0x104, 640); // DST_PITCH
        write_mmio_reg(&mut machine, 0x110, 1); // DEPTH
        write_mmio_reg(&mut machine, 0x13c, (5 << 16) | 10); // LINE_START: (10,5)
        write_mmio_reg(&mut machine, 0x140, (5 << 16) | 14); // LINE_END: (14,5)
        write_mmio_reg(&mut machine, 0x120, 0xab); // FG_COLOR
        write_mmio_reg(&mut machine, 0x128, 0xf0); // ROP: PATCOPY (solid; LINE has no source)
        write_mmio_reg(&mut machine, 0x150, 0x05); // COMMAND: LINE

        // The five pixels (x=10..14, y=5) are set; the pixel just left is not.
        for x in 10u32..=14 {
            assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 5 * 640 + x), 0xab);
        }
        assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 5 * 640 + 9), 0x00);
        // BUSY set right after the command.
        assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);

        // 5 pixels -> busy_ns = 100 + 5*10 = 150 ns. At 25 MHz (40 ns/clock), three
        // clocks (120 ns) leave it busy; the fourth clears it.
        machine.advance_devices(3);
        assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
        machine.advance_devices(1);
        assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 0);
    }

    #[test]
    fn clipped_xor_fill_through_the_mmio_aperture() {
        let mut machine = test_machine();
        // Seed x=0..3 at y=0 with 0xFF through the LFB.
        for x in 0u32..4 {
            machine.write_physical_u8(MARGO_LFB_BASE + x, 0xff);
        }
        // FILL the 4x1 row with FG 0x0F through ROP 0x5A (PATINVERT: D ^ P), but clip
        // to x in [0, 3): x=0,1,2 are XORed, x=3 is left alone.
        write_mmio_reg(&mut machine, 0x100, 0); // DST_BASE
        write_mmio_reg(&mut machine, 0x104, 640); // DST_PITCH
        write_mmio_reg(&mut machine, 0x110, 1); // DEPTH
        write_mmio_reg(&mut machine, 0x114, 0); // DST_XY: (0,0)
        write_mmio_reg(&mut machine, 0x11c, (1 << 16) | 4); // DIM: 4x1
        write_mmio_reg(&mut machine, 0x120, 0x0f); // FG_COLOR
        write_mmio_reg(&mut machine, 0x128, 0x5a); // ROP: PATINVERT
        write_mmio_reg(&mut machine, 0x134, 0); // CLIP_TL: (0,0)
        write_mmio_reg(&mut machine, 0x138, (1 << 16) | 3); // CLIP_BR: (3,1) exclusive
        write_mmio_reg(&mut machine, 0x130, 0x2); // FLAGS: CLIP_EN
        write_mmio_reg(&mut machine, 0x150, 0x01); // COMMAND: FILL

        assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE), 0xf0); // 0xff ^ 0x0f
        assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 1), 0xf0);
        assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 2), 0xf0);
        assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 3), 0xff); // clipped, untouched
        // 3 pixels written -> busy_ns = 100 + 3*5 = 115 ns. At 40 ns/clock, two clocks
        // (80 ns) leave it busy; the third clears it.
        assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
        machine.advance_devices(2);
        assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
        machine.advance_devices(1);
        assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 0);
    }

    #[test]
    fn vbe_mode_info_reports_hicolor_masks() {
        // ES = 0x4000 -> physical 0x40000, DI = 0, mode 0x0111 (R5G6B5).
        let rom = rom_with_code(&[
            0xb8, 0x00, 0x40, // mov ax, 4000h
            0x8e, 0xc0, // mov es, ax
            0xbf, 0x00, 0x00, // mov di, 0
            0xb8, 0x01, 0x4f, // mov ax, 4F01h
            0xb9, 0x11, 0x01, // mov cx, 0111h
            0xcd, 0x10, // int 10h
            0xf4, // hlt
        ]);
        let mut machine =
            Machine::new(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), rom).unwrap();
        assert_eq!(
            machine.run_until_halt_or_cycles(1_000_000).unwrap(),
            StopReason::Halted
        );
        assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);

        let base = 0x40000;
        assert_eq!(read_u16(&mut machine, base + 0x10), 1280); // BytesPerScanLine = 640 * 2
        assert_eq!(machine.read_physical_u8(base + 0x19), 16); // BitsPerPixel
        assert_eq!(machine.read_physical_u8(base + 0x1f), 5); // RedMaskSize
        assert_eq!(machine.read_physical_u8(base + 0x20), 11); // RedFieldPosition
        assert_eq!(machine.read_physical_u8(base + 0x21), 6); // GreenMaskSize
        assert_eq!(machine.read_physical_u8(base + 0x22), 5); // GreenFieldPosition
        assert_eq!(machine.read_physical_u8(base + 0x23), 5); // BlueMaskSize
        assert_eq!(machine.read_physical_u8(base + 0x24), 0); // BlueFieldPosition
        assert_eq!(machine.read_physical_u8(base + 0x25), 0); // RsvdMaskSize (R5G6B5 has none)
    }

    #[test]
    fn vbe_mode_info_reports_15bpp_masks() {
        // Mode 0x0110 (X1R5G5B5): five-bit channels plus a one-bit reserved field.
        let rom = rom_with_code(&[
            0xb8, 0x00, 0x40, // mov ax, 4000h
            0x8e, 0xc0, // mov es, ax
            0xbf, 0x00, 0x00, // mov di, 0
            0xb8, 0x01, 0x4f, // mov ax, 4F01h
            0xb9, 0x10, 0x01, // mov cx, 0110h
            0xcd, 0x10, // int 10h
            0xf4, // hlt
        ]);
        let mut machine =
            Machine::new(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), rom).unwrap();
        assert_eq!(
            machine.run_until_halt_or_cycles(1_000_000).unwrap(),
            StopReason::Halted
        );
        assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);

        let base = 0x40000;
        assert_eq!(read_u16(&mut machine, base + 0x10), 1280); // BytesPerScanLine = 640 * 2
        assert_eq!(machine.read_physical_u8(base + 0x19), 15); // BitsPerPixel
        assert_eq!(machine.read_physical_u8(base + 0x1f), 5); // RedMaskSize
        assert_eq!(machine.read_physical_u8(base + 0x20), 10); // RedFieldPosition
        assert_eq!(machine.read_physical_u8(base + 0x21), 5); // GreenMaskSize
        assert_eq!(machine.read_physical_u8(base + 0x22), 5); // GreenFieldPosition
        assert_eq!(machine.read_physical_u8(base + 0x23), 5); // BlueMaskSize
        assert_eq!(machine.read_physical_u8(base + 0x24), 0); // BlueFieldPosition
        assert_eq!(machine.read_physical_u8(base + 0x25), 1); // RsvdMaskSize (the X bit)
        assert_eq!(machine.read_physical_u8(base + 0x26), 15); // RsvdFieldPosition
    }

    #[test]
    fn hicolor_scanout_decodes_through_the_lfb_aperture() {
        let mut machine = test_machine();
        machine.margo_mut().set_mode(0x111); // 640x480x16, pitch 1280
        // Red pixel (0xf800) at (3, 2): offset 2*1280 + 3*2 = 2566.
        machine.write_physical_u8(MARGO_LFB_BASE + 2566, 0x00);
        machine.write_physical_u8(MARGO_LFB_BASE + 2567, 0xf8);

        let palette = machine.palette_argb();
        let argb = machine.margo().scanout_argb(&palette);
        assert_eq!(argb[2 * 640 + 3], 0x00ff_0000);
    }
}
