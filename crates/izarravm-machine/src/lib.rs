use izarravm_audio::{OplChip, Resampler, SbDsp};
use izarravm_bus::{BusAccessKind, BusCycle, BusError, BusTrace, BusWidth, CpuBus, Memory};
use izarravm_core::{CpuPreset, HardwareProfile, VideoCard};
use izarravm_cpu::{Cpu386, CpuError, SegmentIndex, SegmentRegister};
pub use izarravm_video::MARGO_ID_VALUE;
use izarravm_video::{
    DAC_ENTRIES, Framebuffer, MARGO_MMIO_SIZE, MARGO_VBE_MODES, MARGO_VRAM_SIZE,
    MODE13H_MEMORY_SIZE, Margo, TextFrame, VGA_MODE13H_BASE, VGA_PLANAR_WINDOW_SIZE, VGA_TEXT_BASE,
    VGA_TEXT_MEMORY_SIZE, Vga, VgaRaster, VideoMode, bytes_per_pixel, pixel_format, vbe_mode,
};
use thiserror::Error;

mod dma;
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
/// VGA 25.175 MHz dot clock (standard 640x480 and related modes).
const VGA_DOT_HZ: u64 = 25_175_000;
/// Sound Blaster 16 IRQ and DMA resources (fixed for the Resonique 2).
const SB_IRQ: u8 = 5;
const SB_DMA_CHANNEL_8: usize = 1;
const SB_DMA_CHANNEL_16: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveDisplay {
    Text,
    Mode13h,
    VgaRaster,
    MargoLfb,
}

#[derive(Debug)]
pub struct Machine {
    profile: MachineProfile,
    cpu: Cpu386,
    memory: Memory,
    video: Vga,
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
    dma: dma::DmaController,
    opl: OplChip,
    resampler: Resampler,
    opl_micros: f64, // fractional microseconds owed to the OPL timers
    dsp: SbDsp,
    /// DSP PCM resampler (rate_hz -> 44100), rebuilt when the programmed rate
    /// changes. Summed with the OPL stream in render_audio.
    dsp_resampler: Resampler,
    dsp_rate_hz: u32, // input rate the dsp_resampler is currently configured for
    dsp_micros: f64,  // fractional microseconds owed to the DSP reset-settle clock
    margo_ns: f64,    // fractional nanoseconds owed to the Margo busy countdown
    vga_dots: f64,    // fractional VGA dot clocks owed to the beam advance
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
            video: Vga::default(),
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
            dma: dma::DmaController::default(),
            opl: OplChip::default(),
            resampler: Resampler::new(OPL_NATIVE_HZ, DAC_HZ),
            opl_micros: 0.0,
            dsp: SbDsp::default(),
            // Placeholder; sync_dsp_resampler rebuilds this for the live rate on
            // first use, so the value here never reaches the DAC as-is.
            dsp_resampler: Resampler::new(OPL_NATIVE_HZ, DAC_HZ),
            dsp_rate_hz: 0,
            dsp_micros: 0.0,
            margo_ns: 0.0,
            vga_dots: 0.0,
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
            video: Vga::default(),
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
            dma: dma::DmaController::default(),
            opl: OplChip::default(),
            resampler: Resampler::new(OPL_NATIVE_HZ, DAC_HZ),
            opl_micros: 0.0,
            dsp: SbDsp::default(),
            // Placeholder; sync_dsp_resampler rebuilds this for the live rate on
            // first use, so the value here never reaches the DAC as-is.
            dsp_resampler: Resampler::new(OPL_NATIVE_HZ, DAC_HZ),
            dsp_rate_hz: 0,
            dsp_micros: 0.0,
            margo_ns: 0.0,
            vga_dots: 0.0,
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

    /// Build a machine with a DOS program loaded and ready to run. The format is
    /// detected by the "MZ" signature: an .EXE is relocated and entered at its
    /// header CS:IP / SS:SP with DS=ES=PSP; a .COM is loaded flat at
    /// DOS_LOAD_SEGMENT with CS=DS=ES=SS=that segment, IP=0x100, SP=0xFFFE. Run
    /// with run_until_halt_or_cycles and read dos_output plus the DosExit stop
    /// reason.
    ///
    /// Entry eflags has IF clear, unlike real DOS which hands control with
    /// interrupts enabled. This slice installs no BIOS interrupt handlers (IVT[8]
    /// and friends are zero), so a program that wants hardware IRQs must set them up
    /// and STI itself; the BIOS IVT and an interrupts-enabled handoff come with a
    /// later slice.
    pub fn new_dos_program(profile: MachineProfile, image: &[u8]) -> Result<Self, MachineError> {
        let mut machine = Self {
            memory: Memory::from_mib(profile.memory_mib)?,
            profile,
            cpu: Cpu386::default(),
            video: Vga::default(),
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
            dma: dma::DmaController::default(),
            opl: OplChip::default(),
            resampler: Resampler::new(OPL_NATIVE_HZ, DAC_HZ),
            opl_micros: 0.0,
            dsp: SbDsp::default(),
            // Placeholder; sync_dsp_resampler rebuilds this for the live rate on
            // first use, so the value here never reaches the DAC as-is.
            dsp_resampler: Resampler::new(OPL_NATIVE_HZ, DAC_HZ),
            dsp_rate_hz: 0,
            dsp_micros: 0.0,
            margo_ns: 0.0,
            vga_dots: 0.0,
            trace: BusTrace::default(),
            elapsed_clocks: 0,
        };
        debug_assert!(
            machine.memory.len() as u64 <= u64::from(MARGO_LFB_BASE),
            "system RAM overlaps the Margo LFB aperture at 0xE0000000"
        );
        install_boot_bios_stubs(&mut machine.memory)?;

        let entry = izarravm_dos::load_program(image, &mut machine.memory, DOS_LOAD_SEGMENT)?;
        machine.apply_program_entry(entry);
        // Seed the Toka-DOS per-program state (memory arena, DTA). prog_top is the
        // top-of-memory paragraph the loader wrote to PSP:0x02.
        let prog_top = machine
            .memory
            .read_u16(usize::from(DOS_LOAD_SEGMENT) * 16 + 2)?;
        machine.dos.init_program(DOS_LOAD_SEGMENT, prog_top);
        Ok(machine)
    }

    /// Set the CPU to a loaded program's entry: CS:IP, SS:SP, DS, ES, and a
    /// real-mode eflags with IF clear (no BIOS IVT is installed, so a program
    /// wanting hardware IRQs sets them up and STIs itself).
    fn apply_program_entry(&mut self, entry: izarravm_dos::ProgramEntry) {
        let r = &mut self.cpu.registers;
        r.set_segment(SegmentIndex::Cs, SegmentRegister::real(entry.cs));
        r.set_segment(SegmentIndex::Ds, SegmentRegister::real(entry.ds));
        r.set_segment(SegmentIndex::Es, SegmentRegister::real(entry.es));
        r.set_segment(SegmentIndex::Ss, SegmentRegister::real(entry.ss));
        r.eip = u32::from(entry.ip);
        r.set_esp(u32::from(entry.sp));
        r.eflags = 0x0000_0002;
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
            dma: &mut self.dma,
            opl: &mut self.opl,
            dsp: &mut self.dsp,
            trace: &mut self.trace,
            pending_soft_int: &mut self.pending_soft_int,
            wait_states: self.profile.wait_states,
        }
    }

    pub fn read_physical_u8(&mut self, address: u32) -> u8 {
        let mut bus = self.make_bus();
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
        matches!(
            self.video.active_mode(),
            VideoMode::Mode13h | VideoMode::Planar | VideoMode::ModeX
        )
    }

    pub fn margo(&self) -> &Margo {
        &self.margo
    }

    pub fn margo_mut(&mut self) -> &mut Margo {
        &mut self.margo
    }

    pub fn video(&self) -> &Vga {
        &self.video
    }

    pub fn video_mut(&mut self) -> &mut Vga {
        &mut self.video
    }

    pub fn set_vga_mode_0dh(&mut self) {
        self.video.set_mode_0dh();
    }

    /// Select a VGA planar mode by its INT 10h number from the host side. Returns
    /// false for an unimplemented number. On success it hands the display back to
    /// the VGA core by clearing the Margo latch.
    pub fn set_vga_mode(&mut self, mode: u8) -> bool {
        let ok = self.video.set_mode(mode);
        if ok {
            self.margo_active = false;
        }
        ok
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
        // AH=00h, AL = a planar mode number this slice implements.
        if (ax >> 8) == 0x00 && matches!(ax as u8, 0x0D | 0x0E | 0x10 | 0x12) {
            self.set_vga_mode(ax as u8); // clears the Margo latch internally
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
        // AH=35h (get vector) and AH=2Fh (get DTA) return a segment in ES. The
        // marshalling reads ES as an input at the top; write it back here so those
        // results reach the guest. For calls that do not touch ES, regs.es still
        // equals the input selector, so this re-sets the same real-mode base.
        self.cpu
            .registers
            .set_segment(SegmentIndex::Es, SegmentRegister::real(regs.es));
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
        } else if matches!(
            self.video.active_mode(),
            VideoMode::Planar | VideoMode::ModeX
        ) {
            ActiveDisplay::VgaRaster
        } else if self.video.active_mode() == VideoMode::Mode13h {
            ActiveDisplay::Mode13h
        } else {
            ActiveDisplay::Text
        }
    }

    pub fn vga_raster(&mut self) -> Option<VgaRaster> {
        self.video.last_presented().cloned()
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

        // The DSP reset-settle countdown advances with emulated time so a
        // detection routine's delay loop sees 0xAA become available. The
        // playback sample clock is paced by render_audio, not here.
        self.dsp_micros += clocks as f64 * 1_000_000.0 / self.profile.clock_hz as f64;
        let whole = self.dsp_micros.floor();
        self.dsp.advance_micros(whole);
        self.dsp_micros -= whole;

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

        self.vga_dots += clocks as f64 * VGA_DOT_HZ as f64 / self.profile.clock_hz as f64;
        let whole = self.vga_dots.floor();
        self.video.advance(whole as u64);
        self.vga_dots -= whole;

        self.pump_pusher();
    }

    /// Drive the DMA pusher (section 7.9). While the pusher is enabled, the engine
    /// is idle (`busy_ns == 0`), and the ring is not drained (`get != put`), read
    /// one command from the ring in system RAM and replay its data words as
    /// register writes through `margo.write_mmio_u8`, advancing PUSH_GET. A data
    /// word that writes COMMAND sets `busy_ns`, so the loop stalls there until the
    /// operation completes on a later `advance_devices`, which is why PUSH_GET
    /// trails PUSH_PUT. Latch-only packets consume instantly.
    ///
    /// A full ring holds at most `size / 4` words, so the engine consumes at most
    /// that many words per call: this backstops a malformed ring (a non-power-of-two
    /// `size`, or a `put` that the `(get + 4) % size` orbit never reaches) where the
    /// `get != put` guard alone would spin forever over latch-only or zeroed words.
    /// A well-formed ring always drains in fewer than `size / 4` words, so the budget
    /// never truncates legitimate work.
    fn pump_pusher(&mut self) {
        let p = self.margo.pusher();
        if !p.enabled || p.size == 0 {
            return;
        }
        let mut get = p.get;
        let mut budget = (p.size / 4) as u64;
        while self.margo.busy_ns() == 0 && get != p.put && budget > 0 {
            let header = self.read_ring_word(p.base, p.size, get);
            let method = (header & 0xffff) as usize;
            let count = header >> 16;
            get = (get + 4) % p.size;
            budget -= 1;
            let mut i = 0u32;
            while i < count && get != p.put && budget > 0 {
                let data = self.read_ring_word(p.base, p.size, get);
                for b in 0..4 {
                    self.margo
                        .write_mmio_u8(method + (i as usize) * 4 + b, (data >> (8 * b)) as u8);
                }
                get = (get + 4) % p.size;
                budget -= 1;
                i += 1;
            }
            self.margo.set_pusher_get(get);
        }
    }

    /// Read one 32-bit little-endian word from the command ring at byte offset
    /// `off`, wrapping within `size` (a power of two in practice; `% size` is used
    /// so any nonzero size is safe). Each byte is bounds-checked against system RAM;
    /// an out-of-range byte reads as 0 (no panic, no wrap into other state).
    fn read_ring_word(&self, base: u32, size: u32, off: u32) -> u32 {
        let mut bytes = [0u8; 4];
        for (b, slot) in bytes.iter_mut().enumerate() {
            let ring_off = (off as usize + b) % size as usize;
            *slot = self.memory.read_u8(base as usize + ring_off).unwrap_or(0);
        }
        u32::from_le_bytes(bytes)
    }

    /// Render `native_samples` of DSP DMA output as stereo frames. Each frame
    /// pulls one byte (8-bit mode, mono duplicated L/R) or one/two 16-bit words
    /// (16-bit mono/stereo) from the matching DMA channel; the DSP's half/end
    /// IRQ is forwarded to the PIC as IRQ5. The DSP renders one stereo frame per
    /// call; the mixer resamples and sums it with the OPL.
    pub fn render_dsp_audio(&mut self, native_samples: usize) -> Vec<(i16, i16)> {
        let Machine {
            dsp,
            dma,
            memory,
            pic,
            ..
        } = self;
        let mut out = Vec::with_capacity(native_samples);
        for _ in 0..native_samples {
            // Only the fetcher matching the armed mode is given the DMA/memory
            // borrow; the other is a no-op closure the DSP never calls. This keeps
            // the single &mut dma/&mut memory borrow sound while feeding both paths
            // through one render_frame call.
            let frame = if dsp.is_16bit() {
                dsp.render_frame(|| None, || dma.read_word(SB_DMA_CHANNEL_16, memory))
            } else {
                dsp.render_frame(|| dma.read_byte(SB_DMA_CHANNEL_8, memory), || None)
            };
            if let Some(sample) = frame {
                out.push(sample);
            }
            if dsp.take_irq() {
                pic.request(SB_IRQ);
            }
        }
        out
    }

    /// Rebuild the DSP resampler when the programmed sample rate changes, so it
    /// always runs rate_hz -> 44100.
    fn sync_dsp_resampler(&mut self) {
        let rate = self.dsp.rate_hz().max(1);
        if rate != self.dsp_rate_hz {
            self.dsp_resampler = Resampler::new(rate, DAC_HZ);
            self.dsp_rate_hz = rate;
        }
    }

    /// Render `native_samples` of mixed OPL3 + SB16 DSP audio at the 44100 Hz DAC
    /// rate (stereo, saturated to 16-bit). `native_samples` is counted in OPL
    /// native (49716 Hz) time; the DSP is advanced by the matching wall-clock
    /// duration at its own rate. Each stream is resampled to 44100 and summed.
    pub fn render_audio(&mut self, native_samples: usize) -> Vec<(i16, i16)> {
        let opl_native: Vec<(i32, i32)> = (0..native_samples)
            .map(|_| self.opl.render_sample())
            .collect();
        let opl_out = self.resampler.process(&opl_native);

        self.sync_dsp_resampler();
        // DSP native samples spanning the same wall-clock window as the OPL.
        let dsp_native_count = (native_samples as f64 * self.dsp.rate_hz() as f64
            / OPL_NATIVE_HZ as f64)
            .round() as usize;
        // The DSP already produces stereo frames; widen to i32 and resample.
        let dsp_stereo: Vec<(i32, i32)> = self
            .render_dsp_audio(dsp_native_count)
            .iter()
            .map(|&(l, r)| (i32::from(l), i32::from(r)))
            .collect();
        let dsp_out = self.dsp_resampler.process(&dsp_stereo);

        // Sum over the longer length; a silent (idle) DSP yields no frames, so the
        // OPL passes through unchanged when no DMA playback is armed.
        let len = opl_out.len().max(dsp_out.len());
        (0..len)
            .map(|i| {
                let (ol, or) = opl_out.get(i).copied().unwrap_or((0, 0));
                let (dl, dr) = dsp_out.get(i).copied().unwrap_or((0, 0));
                (clamp_i16(ol + dl), clamp_i16(or + dr))
            })
            .collect()
    }

    /// Raise a hardware interrupt request line into the PIC. The PIT and other
    /// devices call this; slice 2b wires the PIT's IRQ0 tick through here.
    pub fn request_irq(&mut self, line: u8) {
        self.pic.request(line);
    }

    /// Pull one byte from a DMA channel's memory transfer (memory->device read).
    /// Returns None when the channel is masked or has reached terminal count. The
    /// sound slice feeds this to the SB16 DSP for 8-bit playback.
    pub fn dma_read_byte(&mut self, channel: usize) -> Option<u8> {
        self.dma.read_byte(channel, &mut self.memory)
    }

    /// Pull one 16-bit word from a slave DMA channel's memory transfer
    /// (memory->device read). Returns None on the master channels (0-3, 8-bit) or
    /// when the slave channel is masked / at terminal count. The sound slice
    /// feeds this to the SB16 DSP for 16-bit playback (channel 5).
    pub fn dma_read_word(&mut self, channel: usize) -> Option<u16> {
        self.dma.read_word(channel, &mut self.memory)
    }

    /// Advance the DSP reset-settle clock by `micros` microseconds. The run loop
    /// drives this from CPU clocks in advance_devices; this exposes it directly
    /// so a reset-detection golden can settle the DSP without running the CPU.
    pub fn advance_dsp_micros(&mut self, micros: u64) {
        self.dsp.advance_micros(micros as f64);
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
                    dma,
                    opl,
                    dsp,
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
                    dma,
                    opl,
                    dsp,
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
    video: &'a mut Vga,
    margo: &'a mut Margo,
    rom: &'a [u8],
    serial: &'a mut SerialPort,
    device_ports: &'a mut DevicePorts,
    pic: &'a mut pic::Pic8259Pair,
    pit: &'a mut pit::Pit,
    dma: &'a mut dma::DmaController,
    opl: &'a mut OplChip,
    dsp: &'a mut SbDsp,
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
        if let Some(value) = self.dsp.read_port(port) {
            return Ok(u32::from(value));
        }
        if let Some(value) = self.pit.read_port(port) {
            return Ok(u32::from(value));
        }
        if let Some(value) = self.pic.read_port(port) {
            return Ok(u32::from(value));
        }
        if let Some(value) = self.dma.read_port(port) {
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
        if self.dsp.write_port(port, value as u8) {
            return Ok(());
        }
        if self.dma.write_port(port, value as u8) {
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
    fn read_memory_bytes(&mut self, address: u32, width: usize) -> Result<Vec<u8>, BusError> {
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

        // Unchained (mode X) and 16-color planar use the 64 KB planar datapath;
        // chained mode 13h uses the flat 64000-byte buffer. cpu_read loads the VGA
        // latches as a side effect, so the planar path needs &mut self.
        if matches!(
            self.video.active_mode(),
            VideoMode::Planar | VideoMode::ModeX
        ) {
            if let Some(offset) = vga_planar_offset(address, width) {
                return Ok((0..width)
                    .map(|i| self.video.cpu_read(offset + i))
                    .collect());
            }
        } else if let Some(offset) = video_mode13h_offset(address, width) {
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

        // Unchained (mode X) and 16-color planar route A0000 through the 64 KB
        // planar datapath (map mask, write mode, bit mask, latches); chained mode
        // 13h writes the flat 64000-byte buffer.
        if matches!(
            self.video.active_mode(),
            VideoMode::Planar | VideoMode::ModeX
        ) {
            if let Some(offset) = vga_planar_offset(address, 1) {
                self.video.cpu_write(offset, value);
                return Ok(());
            }
        } else if let Some(offset) = video_mode13h_offset(address, 1) {
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
            || vga_planar_offset(address, 1).is_some()
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

/// The A0000 window for unchained / 16-color planar access: the full 64 KB the
/// hardware decodes, wider than the 64000-byte chained mode-13h buffer.
fn vga_planar_offset(address: u32, width: usize) -> Option<usize> {
    let end = VGA_MODE13H_BASE + VGA_PLANAR_WINDOW_SIZE;
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

    #[test]
    fn dma_channel_one_transfers_from_memory_through_the_bus() {
        let mut machine = test_machine();
        // Seed memory at physical 0x01_0010 (page 0x01, offset 0x0010).
        machine.write_physical_u8(0x0001_0010, 0x77);
        with_bus(&mut machine, |bus| {
            bus.write_io(0x0B, BusWidth::Byte, 0x49).unwrap(); // mode ch1: single, read
            bus.write_io(0x02, BusWidth::Byte, 0x10).unwrap(); // address LSB
            bus.write_io(0x02, BusWidth::Byte, 0x00).unwrap(); // address MSB -> 0x0010
            bus.write_io(0x03, BusWidth::Byte, 0x00).unwrap(); // count LSB
            bus.write_io(0x03, BusWidth::Byte, 0x00).unwrap(); // count MSB -> 0 (1 transfer)
            bus.write_io(0x83, BusWidth::Byte, 0x01).unwrap(); // page -> 0x01_0010
            bus.write_io(0x0A, BusWidth::Byte, 0x01).unwrap(); // unmask channel 1
        });
        let byte = machine.dma_read_byte(1).expect("a byte from channel 1");
        assert_eq!(byte, 0x77);
    }

    #[test]
    fn sb_dsp_reset_handshake_through_the_bus() {
        let mut machine = test_machine();
        // Reset: write 1, then 0 to the DSP reset port 0x226.
        with_bus(&mut machine, |bus| {
            bus.write_io(0x226, BusWidth::Byte, 0x01).unwrap();
            bus.write_io(0x226, BusWidth::Byte, 0x00).unwrap();
        });
        // Advance emulated time past the ~100us DSP settle window.
        machine.advance_dsp_micros(200);
        let status = with_bus(&mut machine, |bus| {
            u8::try_from(bus.read_io(0x22E, BusWidth::Byte).unwrap()).unwrap()
        });
        assert_eq!(status & 0x80, 0x80, "data available after reset");
        let ack = with_bus(&mut machine, |bus| {
            u8::try_from(bus.read_io(0x22A, BusWidth::Byte).unwrap()).unwrap()
        });
        assert_eq!(ack, 0xAA);
    }

    #[test]
    fn sb_dsp_version_and_status_route_through_the_bus() {
        let mut machine = test_machine();
        with_bus(&mut machine, |bus| {
            bus.write_io(0x22C, BusWidth::Byte, 0xE1).unwrap(); // read version
        });
        let hi = with_bus(&mut machine, |bus| {
            u8::try_from(bus.read_io(0x22A, BusWidth::Byte).unwrap()).unwrap()
        });
        let lo = with_bus(&mut machine, |bus| {
            u8::try_from(bus.read_io(0x22A, BusWidth::Byte).unwrap()).unwrap()
        });
        assert_eq!([hi, lo], [4, 5]);
    }

    #[test]
    fn sb_8bit_dma_plays_a_buffer_through_the_dsp() {
        let mut machine = test_machine();
        // A 16-byte unsigned ramp in conventional memory at 0x01_0000.
        let bytes: Vec<u8> = (0..16).map(|i| (i * 16) as u8).collect();
        for (i, &b) in bytes.iter().enumerate() {
            machine.write_physical_u8(0x1_0000 + i as u32, b);
        }
        with_bus(&mut machine, |bus| {
            // DMA ch1: address 0x0000, page 0x01, count 15, single read.
            bus.write_io(0x0B, BusWidth::Byte, 0x49).unwrap(); // mode ch1
            bus.write_io(0x02, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x02, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x03, BusWidth::Byte, 0x0F).unwrap();
            bus.write_io(0x03, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x83, BusWidth::Byte, 0x01).unwrap(); // page -> 0x01_0000
            bus.write_io(0x0A, BusWidth::Byte, 0x01).unwrap(); // unmask ch1
            // DSP: 11025 Hz, block 16, single 8-bit DMA output.
            for &b in &[0x41u8, 0x2B, 0x11, 0x48, 0x0F, 0x00, 0x14] {
                bus.write_io(0x22C, BusWidth::Byte, u32::from(b)).unwrap();
            }
        });
        let out = machine.render_dsp_audio(16);
        assert_eq!(out.len(), 16);
        // Unsigned 0x00 maps to a centered negative sample; mono is duplicated L/R.
        assert!(out.iter().any(|&(l, _)| l < 0), "expected negative samples");
        assert!(
            out.iter().all(|&(l, r)| l == r),
            "8-bit mono duplicated L/R"
        );
        // Single mode masks channel 1 at terminal count.
        assert_eq!(machine.dma_read_byte(1), None);
    }

    #[test]
    fn sb_16bit_dma_plays_a_signed_stereo_buffer_through_the_dsp() {
        let mut machine = test_machine();
        // 8 signed-LE stereo frames (32 bytes). The slave 8237A (channel 5)
        // word-addresses its transfers, so page 0x01 at word addr 0 drives byte
        // base (0x01 << 17) = 0x2_0000 (page in A23-A17, A0 tied low). Each frame
        // is L = -1 (0xFFFF) then R = +1 (0x0001).
        let frame: [u8; 4] = [0xFF, 0xFF, 0x01, 0x00];
        for i in 0..8 {
            for (j, &b) in frame.iter().enumerate() {
                machine.write_physical_u8(0x2_0000 + (i * 4 + j) as u32, b);
            }
        }
        with_bus(&mut machine, |bus| {
            // Slave ch5 (local ch1): word addr 0, page 0x8B=0x01, count 15 (16
            // words), auto-init read.
            bus.write_io(0xD6, BusWidth::Byte, 0x59).unwrap(); // slave ch1 mode: auto-init, read
            bus.write_io(0xC4, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0xC4, BusWidth::Byte, 0x00).unwrap(); // word addr 0
            bus.write_io(0xC6, BusWidth::Byte, 0x0F).unwrap();
            bus.write_io(0xC6, BusWidth::Byte, 0x00).unwrap(); // count 15 -> 16 words
            bus.write_io(0x8B, BusWidth::Byte, 0x01).unwrap(); // page -> byte base 0x2_0000
            bus.write_io(0xD4, BusWidth::Byte, 0x01).unwrap(); // unmask slave ch1
            // DSP: 22050 Hz, 16-bit auto-init output, signed, stereo, count 15.
            for &b in &[0x41u8, 0x56, 0x22, 0xB6, 0x30, 0x0F, 0x00] {
                bus.write_io(0x22C, BusWidth::Byte, u32::from(b)).unwrap();
            }
        });
        let out = machine.render_dsp_audio(8);
        assert_eq!(out.len(), 8);
        assert_eq!(out[0].0, -1, "left channel is signed -1");
        assert_eq!(out[0].1, 1, "right channel is signed +1");
        assert!(
            out.iter().all(|&(l, r)| l == -1 && r == 1),
            "every stereo frame decodes L=-1, R=+1"
        );
        // Auto-init: channel 5 still feeds after draining the buffer.
        assert!(
            machine.dma_read_word(SB_DMA_CHANNEL_16).is_some(),
            "auto-init keeps feeding"
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
            dma: &mut machine.dma,
            opl: &mut machine.opl,
            dsp: &mut machine.dsp,
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
    fn boot_suite_reports_sb_dsp_reset_pass() {
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
                    && record.name == "sound.sb_dsp_reset"
            }),
            "boot suite should report PASS sound.sb_dsp_reset"
        );
    }

    #[test]
    fn boot_suite_reports_opl3_pass() {
        let mut machine = Machine::new_boot_image(
            MachineProfile::i386dx25(16, VideoCard::Et4000Ax),
            izarravm_firmware::X86_BOOT_TEST_IMAGE,
        )
        .unwrap();
        machine.run_until_halt_or_cycles(5_000_000).unwrap();
        let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();
        assert!(
            results.records.iter().any(|record| {
                record.status == izarravm_firmware::SuiteRecordStatus::Pass
                    && record.name == "sound.opl3"
            }),
            "boot suite should report PASS sound.opl3 (YMF262 status-at-rest signature)"
        );
    }

    #[test]
    fn boot_suite_reports_opl2_pass() {
        let mut machine = Machine::new_boot_image(
            MachineProfile::i386dx25(16, VideoCard::Et4000Ax),
            izarravm_firmware::X86_BOOT_TEST_IMAGE,
        )
        .unwrap();
        machine.run_until_halt_or_cycles(5_000_000).unwrap();
        let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();
        assert!(
            results.records.iter().any(|record| {
                record.status == izarravm_firmware::SuiteRecordStatus::Pass
                    && record.name == "sound.opl2"
            }),
            "boot suite should report PASS sound.opl2 (AdLib timer-overflow detection)"
        );
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
    fn render_audio_passes_through_when_the_dsp_is_idle() {
        // No DMA playback armed: the DSP produces nothing, so render_audio must
        // return the OPL-only output at the DAC rate (the existing contract).
        let mut machine = test_machine();
        let pcm = machine.render_audio(OPL_NATIVE_HZ as usize);
        assert!(
            (pcm.len() as i32 - DAC_HZ as i32).abs() < 50,
            "idle DSP must not truncate the OPL stream, got {} frames",
            pcm.len()
        );
    }

    #[test]
    fn render_audio_mixes_the_dsp_dc_level_with_the_opl() {
        let mut machine = test_machine();
        // A constant 256-byte DMA buffer; 0x40 maps to sample_u8(0x40) = -16384.
        const BYTE: u8 = 0x40;
        const EXPECTED: i32 = -16384; // (0x40 - 128) * 256
        for i in 0..256u32 {
            machine.write_physical_u8(0x1_0000 + i, BYTE);
        }
        with_bus(&mut machine, |bus| {
            // DMA ch1: page 0x01, address 0, count 255, auto-init read.
            bus.write_io(0x0B, BusWidth::Byte, 0x59).unwrap();
            bus.write_io(0x02, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x02, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x03, BusWidth::Byte, 0xFF).unwrap();
            bus.write_io(0x03, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x83, BusWidth::Byte, 0x01).unwrap();
            bus.write_io(0x0A, BusWidth::Byte, 0x01).unwrap();
            // DSP: 11025 Hz, block 256, auto-init 8-bit output.
            for &b in &[0x41u8, 0x2B, 0x11, 0x48, 0xFF, 0x00, 0x1C] {
                bus.write_io(0x22C, BusWidth::Byte, u32::from(b)).unwrap();
            }
        });
        // The OPL is silent (no voices keyed), so the steady output is the DSP DC
        // level after the resampler warmup. Render plenty of OPL-native time.
        let out = machine.render_audio(4_000);
        assert!(!out.is_empty());
        let mid = &out[out.len() / 3..out.len() * 2 / 3];
        let (min_l, max_l) = mid
            .iter()
            .map(|f| f.0)
            .fold((i16::MAX, i16::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
        let center = (i32::from(min_l) + i32::from(max_l)) / 2;
        assert!(
            (center - EXPECTED).abs() < 400,
            "DSP DC center {center}, expected ~{EXPECTED}"
        );
        // Mono is duplicated to both channels.
        assert!(mid.iter().all(|f| f.0 == f.1), "DSP mono duplicated L/R");
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
            Machine::new_dos_program(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), com)
                .unwrap();
        let reason = machine.run_until_halt_or_cycles(100_000).unwrap();
        assert_eq!(reason, StopReason::DosExit { code: 0 });
        assert_eq!(machine.dos_output(), b"Hi");
    }

    #[test]
    fn dos_com_exit_code_is_carried_through() {
        // org 0x100: mov ax,4c07; int 21
        let com: &[u8] = &[0xb8, 0x07, 0x4c, 0xcd, 0x21];
        let mut machine =
            Machine::new_dos_program(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), com)
                .unwrap();
        let reason = machine.run_until_halt_or_cycles(100_000).unwrap();
        assert_eq!(reason, StopReason::DosExit { code: 7 });
        assert!(machine.dos_output().is_empty());
    }

    #[test]
    fn dos_com_unhandled_int21_returns_through_stub_and_exits() {
        // org 0x100: mov ah,0x30 (unhandled); int 21; mov ax,4c00; int 21
        let com: &[u8] = &[0xb4, 0x30, 0xcd, 0x21, 0xb8, 0x00, 0x4c, 0xcd, 0x21];
        let mut machine =
            Machine::new_dos_program(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), com)
                .unwrap();
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
        let mut machine = Machine::new_dos_program(
            MachineProfile::i386dx25(16, VideoCard::Et4000Ax),
            izarravm_firmware::HELLO_COM,
        )
        .unwrap();
        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::DosExit { code: 0 });
        assert_eq!(machine.dos_output(), b"Hello, world!\r\n");
    }

    #[test]
    fn dos_exe_runs_with_relocation_applied() {
        // The committed .EXE loads DS from a relocated segment reference, then
        // prints via AH=09h. Correct output is only possible if load_exe applied
        // the relocation (otherwise DS is the link-time base and the bytes
        // diverge), so this doubles as the end-to-end relocation check.
        let mut machine = Machine::new_dos_program(
            MachineProfile::i386dx25(16, VideoCard::Et4000Ax),
            izarravm_firmware::EXEHELLO_EXE,
        )
        .unwrap();
        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::DosExit { code: 0 });
        assert_eq!(machine.dos_output(), b"Hello from a relocated .EXE!\r\n");
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
            Machine::new_dos_program(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), com)
                .unwrap();
        available.set_dos_stdin(b"X");
        assert_eq!(
            available.run_until_halt_or_cycles(100_000).unwrap(),
            StopReason::DosExit { code: 0 }
        );
        assert_eq!(available.dos_output(), b"X"); // char path taken, AL echoed

        let mut empty =
            Machine::new_dos_program(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), com)
                .unwrap();
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
            Machine::new_dos_program(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), com)
                .unwrap();
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
        let mut machine = Machine::new_dos_program(
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
        let mut machine = Machine::new_dos_program(
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
    fn pattern_fill_through_the_mmio_aperture_tiles_and_times_busy() {
        let mut machine = test_machine();
        // Seed an 8x8 tile in offscreen VRAM (offset 0x10000, clear of the
        // destination) through the LFB: cell (r, c) = r*8 + c + 1, depth 1.
        let pat_base = 0x1_0000u32;
        for r in 0..8u32 {
            for c in 0..8u32 {
                machine.write_physical_u8(
                    MARGO_LFB_BASE + pat_base + r * 8 + c,
                    (r * 8 + c + 1) as u8,
                );
            }
        }
        write_mmio_reg(&mut machine, 0x100, 0); // DST_BASE
        write_mmio_reg(&mut machine, 0x104, 640); // DST_PITCH
        write_mmio_reg(&mut machine, 0x110, 1); // DEPTH
        write_mmio_reg(&mut machine, 0x144, pat_base); // PAT_BASE
        write_mmio_reg(&mut machine, 0x114, (2 << 16) | 3); // DST_XY: (x=3, y=2)
        write_mmio_reg(&mut machine, 0x11c, (4 << 16) | 4); // DIM: 4x4
        write_mmio_reg(&mut machine, 0x128, 0xf0); // ROP: PATCOPY (P = pattern, no source)
        write_mmio_reg(&mut machine, 0x150, 0x06); // COMMAND: PATTERN_FILL

        // Absolute-phase tiling: dst (x, y) -> tile[y & 7][x & 7] = (y & 7)*8 + (x & 7) + 1.
        assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 2 * 640 + 3), 20); // (3,2) tile[2][3]
        assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 2 * 640 + 6), 23); // (6,2) tile[2][6]
        assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 5 * 640 + 3), 44); // (3,5) tile[5][3]
        assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 2 * 640 + 2), 0); // left of the rect
        assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1); // BUSY set

        // 16 pixels -> busy_ns = 100 + 16*5 = 180 ns. At 25 MHz (40 ns/clock), four
        // clocks (160 ns) leave it busy; the fifth clears it.
        machine.advance_devices(4);
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

    #[test]
    fn hardware_cursor_composites_through_the_apertures() {
        let mut machine = test_machine();
        machine.margo_mut().set_mode(0x111); // 640x480x16 (R5G6B5)
        // Seed the cursor planes offscreen (1 MiB in, past the 16bpp visible surface)
        // through the LFB. FG pixel at cursor (0,0): XOR plane byte 0 bit 0x80, AND clear.
        let addr = 0x10_0000u32;
        machine.write_physical_u8(MARGO_LFB_BASE + addr + 512, 0x80);
        write_mmio_reg(&mut machine, 0x2c, addr); // CURSOR_ADDR
        write_mmio_reg(&mut machine, 0x30, (5 << 16) | 3); // CURSOR_POS: (x=3, y=5)
        write_mmio_reg(&mut machine, 0x34, 0xf800); // CURSOR_FG = pure red
        write_mmio_reg(&mut machine, 0x38, 0x0000); // CURSOR_BG
        write_mmio_reg(&mut machine, 0x28, 1); // CURSOR_CTRL = ENABLE

        let palette = machine.palette_argb();
        let argb = machine.margo().scanout_argb(&palette);
        // Cursor pixel (0,0) lands at the positioned screen pixel (3, 5), proving the
        // packed CURSOR_POS encoding routes through the aperture.
        assert_eq!(argb[5 * 640 + 3], 0x00ff_0000); // FG decoded as red at (3,5)
        assert_eq!(argb[0], 0x0000_0000); // the origin is outside the cursor: black surface
    }

    #[test]
    fn machine_advances_the_vga_beam_with_cpu_clocks() {
        let mut machine = test_machine();
        machine.set_vga_mode_0dh();
        let before = machine.video().beam_dots();
        // 10 000 CPU clocks at 25 MHz with a 25.175 MHz dot clock advances
        // roughly 10 070 dots — well above zero.
        machine.advance_devices(10_000);
        assert!(machine.video().beam_dots() != before || machine.video().frames_completed() > 0);
    }

    #[test]
    fn planar_mode_presents_a_vga_raster() {
        let mut machine = test_machine();
        machine.set_vga_mode_0dh();
        // Mode 0Dh frame is ~359 200 dots; 600 000 CPU clocks at 25 MHz yields
        // ~603 600 dot clocks — enough to complete at least one full frame.
        machine.advance_devices(600_000);
        assert!(matches!(machine.active_display(), ActiveDisplay::VgaRaster));
        assert!(machine.vga_raster().is_some());
    }

    #[test]
    fn a0000_writes_route_to_the_planar_datapath_in_mode_0dh() {
        let mut machine = test_machine();
        machine.set_vga_mode_0dh();
        // Enable plane 0 only, copy write mode, full bit mask, via the VGA ports.
        machine.video_mut().write_port(0x3C4, 0x02);
        machine.video_mut().write_port(0x3C5, 0x01); // map mask = plane 0
        machine.video_mut().write_port(0x3CE, 0x05);
        machine.video_mut().write_port(0x3CF, 0x00); // write mode 0
        machine.video_mut().write_port(0x3CE, 0x08);
        machine.video_mut().write_port(0x3CF, 0xFF); // bit mask 0xFF
        // Write a byte to A0000 through the machine memory path.
        machine.write_physical_u8(0x000A_0000, 0xFF);
        // Plane 0 byte 0 should now be 0xFF (planar datapath), confirming routing.
        assert_eq!(machine.video().plane_byte(0, 0), 0xFF);
    }

    #[test]
    fn copper_bar_split_through_the_machine() {
        let mut machine = test_machine();
        machine.set_vga_mode_0dh();
        // Set up so A0000 writes fill plane 0 (attribute index 1) with a full bit
        // mask. Write mode 0 is the reset default.
        machine.video_mut().write_port(0x3C4, 0x02);
        machine.video_mut().write_port(0x3C5, 0x01); // map mask = plane 0
        machine.video_mut().write_port(0x3CE, 0x08);
        machine.video_mut().write_port(0x3CF, 0xFF); // bit mask 0xFF
        // Fill the visible region of plane 0 (offset 0..8000 covers 200 lines * 40
        // bytes) through the machine memory path — exercises the A0000 routing.
        for off in 0..8000u32 {
            machine.write_physical_u8(0x000A_0000 + off, 0xFF);
        }
        // Identity attribute palette so index 1 -> DAC 1. Reading 3DA resets the
        // flip-flop to "index" first; each entry is an index write then a value
        // write, so after 16 entries the flip-flop is back in "index" mode.
        machine.video_mut().read_status1(); // reset attr flip-flop
        for i in 0..16u8 {
            machine.video_mut().write_port(0x3C0, i); // index
            machine.video_mut().write_port(0x3C0, i); // value: palette[i] = i
        }
        // Advance to roughly counter line 50, change palette[1] -> 9, then finish
        // the frame. dots = clocks * VGA_DOT_HZ / clock_hz (~1.007 dots/clock);
        // 39_700 clocks ≈ 39_980 dots ≈ counter line 49 (htotal 800).
        machine.advance_devices(39_700);
        // The flip-flop is in "index" mode here (even number of writes above).
        machine.video_mut().write_port(0x3C0, 0x01); // attr index 1
        machine.video_mut().write_port(0x3C0, 9); // palette[1] = 9
        machine.advance_devices(400_000); // complete the frame
        let raster = machine.vga_raster().expect("a frame presented");
        let w = raster.width as usize;
        // The principle: a contiguous top region uses the old palette (DAC 1) and a
        // lower region uses the new palette (DAC 9), separated by the beam row at
        // the time of the palette change. Scan for that transition rather than
        // hard-coding the split row, so the test survives small timing drift.
        assert_eq!(raster.pixels[0], 1, "top of frame uses the old palette");
        let height = raster.height as usize;
        let mut split = None;
        for row in 0..height {
            let p = raster.pixels[row * w];
            if p == 9 {
                split = Some(row);
                break;
            }
            assert_eq!(p, 1, "row {row} above the split must use the old palette");
        }
        let split = split.expect("a row using the new palette exists below the split");
        // The split must land in the active region (200 raster rows of content),
        // not at the very top or beyond the visible area.
        assert!(
            (1..200).contains(&split),
            "split row {split} should fall inside the active picture"
        );
        // Every active row at or below the split uses the new palette.
        for row in split..200 {
            assert_eq!(
                raster.pixels[row * w],
                9,
                "row {row} below the split must use the new palette"
            );
        }
    }

    #[test]
    fn line_compare_split_through_the_machine() {
        let mut machine = test_machine();
        machine.set_vga_mode_0dh(); // double-scanned byte mode
        // A0000 writes fill plane 0 with a full bit mask, write mode 0 (reset default).
        machine.video_mut().write_port(0x3C4, 0x02);
        machine.video_mut().write_port(0x3C5, 0x01); // map mask = plane 0
        machine.video_mut().write_port(0x3CE, 0x08);
        machine.video_mut().write_port(0x3CF, 0xFF); // bit mask 0xFF
        // Mark the top of VRAM (plane 0 offset 0) with bit 7 only: pixel 0 set, the rest
        // clear. The split region reads this; a non-uniform byte also detects a
        // wrongly-applied pel-pan below the split.
        machine.write_physical_u8(0x000A_0000, 0x80);
        // Identity attribute palette so index 1 -> DAC 1. read_status1 resets the
        // flip-flop to "index"; 16 entries * 2 writes leaves it in "index" mode.
        machine.video_mut().read_status1();
        for i in 0..16u8 {
            machine.video_mut().write_port(0x3C0, i); // index
            machine.video_mut().write_port(0x3C0, i); // value: palette[i] = i
        }
        // Lock pel-pan below the split (Attribute Mode Control 10h bit 5) and pan the
        // top by 4. The flip-flop is in "index" mode here.
        machine.video_mut().write_port(0x3C0, 0x10); // attr index 0x10 (mode control)
        machine.video_mut().write_port(0x3C0, 0x20); // bit 5: pel-pan up to line compare
        machine.video_mut().write_port(0x3C0, 0x13); // attr index 0x13 (pixel pan)
        machine.video_mut().write_port(0x3C0, 0x04); // pan 4
        // Program a split at scan-counter line 100. The mode default line compare is
        // 0x3FF, so the overflow (07h) bit 8 and max-scan (09h) bit 9 must be cleared.
        // The 09h write touches only line compare bit 9, not the double-scan bit.
        machine.video_mut().write_port(0x3D4, 0x07);
        machine.video_mut().write_port(0x3D5, 0x00); // line compare bit 8 = 0
        machine.video_mut().write_port(0x3D4, 0x09);
        machine.video_mut().write_port(0x3D5, 0x00); // line compare bit 9 = 0
        machine.video_mut().write_port(0x3D4, 0x18);
        machine.video_mut().write_port(0x3D5, 0x64); // line compare low 8 bits = 100
        // Scroll the top region to a cleared area of VRAM (start address 0x4000),
        // buffered until the next vertical retrace.
        machine.video_mut().write_port(0x3D4, 0x0C);
        machine.video_mut().write_port(0x3D5, 0x40); // start address high
        machine.video_mut().write_port(0x3D4, 0x0D);
        machine.video_mut().write_port(0x3D5, 0x00); // start address low
        // First frame latches the buffered start address; the second renders with it.
        machine.advance_devices(400_000);
        machine.advance_devices(400_000);
        let raster = machine.vga_raster().expect("a frame presented");
        let w = raster.width as usize; // 320
        // A top scanline (50 < 100) reads the scrolled, cleared region: index 0.
        assert_eq!(
            raster.pixels[50 * w],
            0,
            "top region is scrolled to cleared VRAM"
        );
        // The first split scanline (101 = line_compare + 1) reads offset 0 (the marked
        // byte), with pel-pan forced to 0 below the split: pixel 0 is the marked index 1.
        assert_eq!(
            raster.pixels[101 * w],
            1,
            "split region reads offset 0 with pel-pan forced to 0"
        );
    }

    #[test]
    fn display_address_wrap_seam_through_the_machine() {
        let mut machine = test_machine();
        machine.set_vga_mode_0dh(); // byte mode
        // Plane 0 datapath: map mask plane 0, full bit mask, write mode 0 (reset default).
        machine.video_mut().write_port(0x3C4, 0x02);
        machine.video_mut().write_port(0x3C5, 0x01);
        machine.video_mut().write_port(0x3CE, 0x08);
        machine.video_mut().write_port(0x3CF, 0xFF);
        // Mark the top of VRAM: plane 0 offset 0 = 0xFF (pixels 0..7 -> attribute index 1).
        machine.write_physical_u8(0x000A_0000, 0xFF);
        // Identity palette so index 1 -> DAC 1.
        machine.video_mut().read_status1(); // reset attr flip-flop to index
        for i in 0..16u8 {
            machine.video_mut().write_port(0x3C0, i);
            machine.video_mut().write_port(0x3C0, i);
        }
        // Set start_address = 0xFFF8 through the CRTC ports (buffered until vretrace).
        machine.video_mut().write_port(0x3D4, 0x0C); // start address high
        machine.video_mut().write_port(0x3D5, 0xFF);
        machine.video_mut().write_port(0x3D4, 0x0D); // start address low
        machine.video_mut().write_port(0x3D5, 0xF8);
        // First frame latches the buffered start address; the second renders with it.
        machine.advance_devices(400_000);
        machine.advance_devices(400_000);
        let raster = machine.vga_raster().expect("a frame presented");
        let w = raster.width as usize; // 320
        // Row 0: pixels 0..63 read 0xFFF8..0xFFFF (clear), pixels 64..71 wrap to offset 0.
        assert_eq!(raster.pixels[0], 0, "pre-wrap pixel reads the cleared tail");
        assert_eq!(
            raster.pixels[64], 1,
            "wrapped scanout pixel equals the top-of-VRAM pixel (no tear)"
        );
        // Sanity: still on row 0 of the active area.
        assert!(w >= 72);
    }

    #[test]
    fn set_vga_mode_selects_planar_geometry_per_number() {
        let mut machine = test_machine();

        assert!(machine.set_vga_mode(0x0E));
        assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
        assert_eq!(machine.video().raster_width(), 640);
        assert_eq!(machine.video().raster_height(), 449);

        assert!(machine.set_vga_mode(0x12));
        assert_eq!(machine.video().raster_width(), 640);
        assert_eq!(machine.video().raster_height(), 525);

        assert!(!machine.set_vga_mode(0x99));
    }

    #[test]
    fn int10_sets_mode_12h_then_draws_and_presents_640x480() {
        // mov ax, 0012h; int 10h; hlt
        let rom = rom_with_code(&[0xb8, 0x12, 0x00, 0xcd, 0x10, 0xf4]);
        let mut machine =
            Machine::new(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), rom).unwrap();

        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
        assert_eq!(machine.video().raster_width(), 640);
        assert_eq!(machine.video().raster_height(), 525);

        // Draw attribute index 1 into the first byte of plane 0 (first 8 pixels of
        // the top row) through the A0000 datapath, with an identity palette.
        machine.video_mut().write_port(0x3C4, 0x02);
        machine.video_mut().write_port(0x3C5, 0x01); // map mask = plane 0
        machine.video_mut().write_port(0x3CE, 0x08);
        machine.video_mut().write_port(0x3CF, 0xFF); // bit mask 0xFF
        machine.write_physical_u8(0x000A_0000, 0xFF);
        machine.video_mut().read_status1(); // reset attr flip-flop to index
        for i in 0..16u8 {
            machine.video_mut().write_port(0x3C0, i); // index
            machine.video_mut().write_port(0x3C0, i); // palette[i] = i
        }

        // A 12h frame is 800 * 525 = 420 000 dots; 600 000 clocks (~604 000 dots)
        // completes at least one frame.
        machine.advance_devices(600_000);
        let raster = machine.vga_raster().expect("a frame presented");
        assert_eq!(raster.width, 640);
        assert_eq!(raster.height, 525);
        assert_eq!(raster.pixels[0], 1, "top-left pixel is attribute index 1");
    }

    #[test]
    fn overlay_color_key_gates_on_the_primary_pixel() {
        let mut machine = test_machine();
        machine.margo_mut().set_mode(0x14a); // 640x480x32, pitch 2560
        // Primary at (10, 20) holds the key; (11, 20) holds an occluding window pixel.
        let key = 0x0011_2233u32;
        let occluder = 0x0044_5566u32;
        let p0 = 20 * 2560 + 10 * 4;
        let p1 = 20 * 2560 + 11 * 4;
        for (i, b) in key.to_le_bytes().into_iter().enumerate() {
            machine.write_physical_u8(MARGO_LFB_BASE + p0 + i as u32, b);
        }
        for (i, b) in occluder.to_le_bytes().into_iter().enumerate() {
            machine.write_physical_u8(MARGO_LFB_BASE + p1 + i as u32, b);
        }
        // YUY2 source: Y0=235 (white), Y1=16 (black).
        let src = 0x0020_0000u32;
        machine.write_physical_u8(MARGO_LFB_BASE + src, 235);
        machine.write_physical_u8(MARGO_LFB_BASE + src + 1, 128);
        machine.write_physical_u8(MARGO_LFB_BASE + src + 2, 16);
        machine.write_physical_u8(MARGO_LFB_BASE + src + 3, 128);

        write_mmio_reg(&mut machine, 0x44, src);
        write_mmio_reg(&mut machine, 0x48, 4);
        write_mmio_reg(&mut machine, 0x4c, (1 << 16) | 2);
        write_mmio_reg(&mut machine, 0x58, (20 << 16) | 10);
        write_mmio_reg(&mut machine, 0x5c, (1 << 16) | 2);
        write_mmio_reg(&mut machine, 0x60, key); // OVL_COLORKEY
        write_mmio_reg(&mut machine, 0x40, 1 | (1 << 3)); // ENABLE + KEY_EN, FORMAT YUY2

        let palette = machine.palette_argb();
        let argb = machine.margo().scanout_argb(&palette);
        // Where the primary equals the key, the overlay shows (white).
        assert_eq!(argb[20 * 640 + 10], 0x00ff_ffff);
        // Where another value occludes the key, the overlay is hidden and the
        // decoded primary pixel (0x00445566 in X8R8G8B8) remains.
        assert_eq!(argb[20 * 640 + 11], 0x0044_5566);
    }

    #[test]
    fn overlay_yuy2_composites_through_the_apertures() {
        let mut machine = test_machine();
        machine.margo_mut().set_mode(0x14a); // 640x480x32
        // One YUY2 group offscreen (2 MiB in, past the 32bpp visible surface):
        // Y0=235 (white), U=128, Y1=16 (black), V=128. Byte order Y0, U, Y1, V.
        let src = 0x0020_0000u32;
        machine.write_physical_u8(MARGO_LFB_BASE + src, 235);
        machine.write_physical_u8(MARGO_LFB_BASE + src + 1, 128);
        machine.write_physical_u8(MARGO_LFB_BASE + src + 2, 16);
        machine.write_physical_u8(MARGO_LFB_BASE + src + 3, 128);

        write_mmio_reg(&mut machine, 0x44, src); // OVL_SRC_Y (packed surface)
        write_mmio_reg(&mut machine, 0x48, 4); // OVL_SRC_PITCH
        write_mmio_reg(&mut machine, 0x4c, (1 << 16) | 2); // OVL_SRC_DIM: w=2, h=1
        write_mmio_reg(&mut machine, 0x58, (20 << 16) | 10); // OVL_DST_XY: x=10, y=20
        write_mmio_reg(&mut machine, 0x5c, (1 << 16) | 2); // OVL_DST_DIM: w=2, h=1 (1:1)
        write_mmio_reg(&mut machine, 0x40, 1); // OVL_CTRL: ENABLE, FORMAT YUY2, no key

        let palette = machine.palette_argb();
        let argb = machine.margo().scanout_argb(&palette);
        assert_eq!(argb[20 * 640 + 10], 0x00ff_ffff); // Y0 -> white
        assert_eq!(argb[20 * 640 + 11], 0x0000_0000); // Y1 -> black
    }

    #[test]
    fn overlay_scales_by_point_sampling() {
        let mut machine = test_machine();
        machine.margo_mut().set_mode(0x14a);
        // The same one YUY2 group, scaled 2x horizontally: dst width 4.
        let src = 0x0020_0000u32;
        machine.write_physical_u8(MARGO_LFB_BASE + src, 235);
        machine.write_physical_u8(MARGO_LFB_BASE + src + 1, 128);
        machine.write_physical_u8(MARGO_LFB_BASE + src + 2, 16);
        machine.write_physical_u8(MARGO_LFB_BASE + src + 3, 128);

        write_mmio_reg(&mut machine, 0x44, src);
        write_mmio_reg(&mut machine, 0x48, 4);
        write_mmio_reg(&mut machine, 0x4c, (1 << 16) | 2); // src w=2, h=1
        write_mmio_reg(&mut machine, 0x58, (20 << 16) | 10);
        write_mmio_reg(&mut machine, 0x5c, (1 << 16) | 4); // dst w=4, h=1 (2x)
        write_mmio_reg(&mut machine, 0x40, 1);

        let palette = machine.palette_argb();
        let argb = machine.margo().scanout_argb(&palette);
        // sx = dx * src_w / dst_w = dx * 2 / 4 = dx / 2:
        // dst 0,1 sample src pixel 0 (white); dst 2,3 sample src pixel 1 (black).
        assert_eq!(argb[20 * 640 + 10], 0x00ff_ffff);
        assert_eq!(argb[20 * 640 + 11], 0x00ff_ffff);
        assert_eq!(argb[20 * 640 + 12], 0x0000_0000);
        assert_eq!(argb[20 * 640 + 13], 0x0000_0000);
    }

    #[test]
    fn overlay_yv12_upsamples_chroma_through_the_apertures() {
        let mut machine = test_machine();
        machine.margo_mut().set_mode(0x14a); // 640x480x32
        // YV12 source, 2x2. Y plane (pitch 2): [16, 235; 16, 235]. A single shared
        // chroma sample (U=128, V=255) covers the whole 2x2 block (4:2:0 upsample).
        let yp = 0x0020_0000u32;
        let up = 0x0020_1000u32;
        let vp = 0x0020_2000u32;
        machine.write_physical_u8(MARGO_LFB_BASE + yp, 16); // (0,0)
        machine.write_physical_u8(MARGO_LFB_BASE + yp + 1, 235); // (1,0)
        machine.write_physical_u8(MARGO_LFB_BASE + yp + 2, 16); // (0,1)
        machine.write_physical_u8(MARGO_LFB_BASE + yp + 3, 235); // (1,1)
        machine.write_physical_u8(MARGO_LFB_BASE + up, 128); // U plane
        machine.write_physical_u8(MARGO_LFB_BASE + vp, 255); // V plane

        write_mmio_reg(&mut machine, 0x44, yp); // OVL_SRC_Y
        write_mmio_reg(&mut machine, 0x48, 2); // OVL_SRC_PITCH (Y plane)
        write_mmio_reg(&mut machine, 0x4c, (2 << 16) | 2); // OVL_SRC_DIM: 2x2
        write_mmio_reg(&mut machine, 0x50, up); // OVL_SRC_U
        write_mmio_reg(&mut machine, 0x54, vp); // OVL_SRC_V
        write_mmio_reg(&mut machine, 0x58, (20 << 16) | 10); // OVL_DST_XY
        write_mmio_reg(&mut machine, 0x5c, (2 << 16) | 2); // OVL_DST_DIM: 2x2 (1:1)
        write_mmio_reg(&mut machine, 0x40, 1 | (1 << 1)); // ENABLE + FORMAT YV12

        let palette = machine.palette_argb();
        let argb = machine.margo().scanout_argb(&palette);
        // Y=16 with (U=128, V=255) -> 0x00cb0000; Y=235 -> 0x00ff98ff. The same
        // chroma sample applies across the 2x2 block.
        assert_eq!(argb[20 * 640 + 10], 0x00cb_0000);
        assert_eq!(argb[20 * 640 + 11], 0x00ff_98ff);
        assert_eq!(argb[21 * 640 + 10], 0x00cb_0000);
        assert_eq!(argb[21 * 640 + 11], 0x00ff_98ff);
    }

    #[test]
    fn overlay_yv12_chroma_traversal_addresses_each_cell() {
        let mut machine = test_machine();
        machine.margo_mut().set_mode(0x14a); // 640x480x32
        // 4x4 YV12 source with a flat Y of 128, so each output pixel's color is set
        // solely by which 2x2 chroma cell it samples. The 2x2 chroma grid (chroma
        // pitch = Y pitch / 2 = 2) holds a distinct (U, V) per cell, so this proves
        // cx = sx/2, cy = sy/2, and the chroma-plane stride, which the 2x2 test (only
        // cell 0,0) does not exercise.
        let yp = 0x0020_0000u32;
        let up = 0x0020_1000u32;
        let vp = 0x0020_2000u32;
        for i in 0..16u32 {
            machine.write_physical_u8(MARGO_LFB_BASE + yp + i, 128);
        }
        // Chroma cells indexed cy * 2 + cx.
        let us = [128u8, 128, 255, 255];
        let vs = [128u8, 255, 128, 255];
        for i in 0..4u32 {
            machine.write_physical_u8(MARGO_LFB_BASE + up + i, us[i as usize]);
            machine.write_physical_u8(MARGO_LFB_BASE + vp + i, vs[i as usize]);
        }

        write_mmio_reg(&mut machine, 0x44, yp); // OVL_SRC_Y
        write_mmio_reg(&mut machine, 0x48, 4); // OVL_SRC_PITCH (Y plane)
        write_mmio_reg(&mut machine, 0x4c, (4 << 16) | 4); // OVL_SRC_DIM: 4x4
        write_mmio_reg(&mut machine, 0x50, up); // OVL_SRC_U
        write_mmio_reg(&mut machine, 0x54, vp); // OVL_SRC_V
        write_mmio_reg(&mut machine, 0x58, (20 << 16) | 10); // OVL_DST_XY
        write_mmio_reg(&mut machine, 0x5c, (4 << 16) | 4); // OVL_DST_DIM: 4x4 (1:1)
        write_mmio_reg(&mut machine, 0x40, 1 | (1 << 1)); // ENABLE + FORMAT YV12

        let palette = machine.palette_argb();
        let argb = machine.margo().scanout_argb(&palette);
        // Cell (0,0) U=128 V=128 -> gray; two pixels in the same cell share it.
        assert_eq!(argb[20 * 640 + 10], 0x0082_8282);
        assert_eq!(argb[21 * 640 + 11], 0x0082_8282);
        // Cell (1,0) U=128 V=255.
        assert_eq!(argb[20 * 640 + 12], 0x00ff_1b82);
        // Cell (0,1) U=255 V=128.
        assert_eq!(argb[22 * 640 + 10], 0x0082_51ff);
        // Cell (1,1) U=255 V=255.
        assert_eq!(argb[22 * 640 + 12], 0x00ff_00ff);
    }

    #[test]
    fn pusher_runs_a_fill_packet_from_the_ring() {
        let mut machine = test_machine();
        // A command ring in system RAM that issues one FILL: a 2x2 rect of 0xAB at
        // (x=1, y=1) on a depth-1 surface, pitch 8, base 0. Mirrors the guide's
        // fill_via_pusher: header words are (count << 16) | method.
        let ring_base = 0x0001_0000u32;
        let ring: [u32; 16] = [
            (3 << 16) | 0x0100,
            0, // DST_BASE = 0
            8, // DST_PITCH = 8
            0, // SRC_BASE = 0 (unused by FILL)
            (1 << 16) | 0x0110,
            1, // DEPTH = 1
            (1 << 16) | 0x0114,
            (1 << 16) | 1, // DST_XY: y=1, x=1
            (1 << 16) | 0x011c,
            (2 << 16) | 2, // DIM: h=2, w=2
            (1 << 16) | 0x0120,
            0xab, // FG_COLOR = 0xAB
            (1 << 16) | 0x0128,
            0xf0, // ROP = PATCOPY
            (1 << 16) | 0x0150,
            0x01, // COMMAND = FILL
        ];
        for (i, word) in ring.iter().enumerate() {
            for (b, byte) in word.to_le_bytes().into_iter().enumerate() {
                machine.write_physical_u8(ring_base + (i * 4 + b) as u32, byte);
            }
        }
        let put = (ring.len() * 4) as u32; // 64

        write_mmio_reg(&mut machine, 0x84, ring_base); // PUSH_BASE
        write_mmio_reg(&mut machine, 0x88, 0x1000); // PUSH_SIZE (4 KiB, power of two)
        write_mmio_reg(&mut machine, 0x80, 1); // PUSH_CTRL = ENABLE
        write_mmio_reg(&mut machine, 0x8c, put); // PUSH_PUT = doorbell

        // One device tick drives the pump; the FILL applies immediately.
        machine.advance_devices(1);

        // The fill landed in VRAM (read back through the LFB).
        assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 8 + 1), 0xab); // (1,1)
        assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 2 * 8 + 2), 0xab); // (2,2)
        assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE), 0x00); // (0,0) untouched
        // The ring drained: GET reached PUT.
        assert_eq!(read_mmio_reg(&mut machine, 0x90), put);
    }

    #[test]
    fn pusher_does_not_spin_on_a_malformed_ring() {
        let mut machine = test_machine();
        // A non-power-of-two size with a PUT that the (get + 4) % size orbit never
        // reaches, over zeroed RAM (every header decodes to method 0, count 0, so no
        // COMMAND ever sets busy_ns). Without the word budget this would spin forever.
        write_mmio_reg(&mut machine, 0x84, 0x0001_0000); // PUSH_BASE
        write_mmio_reg(&mut machine, 0x88, 10); // PUSH_SIZE: not a multiple of 4
        write_mmio_reg(&mut machine, 0x80, 1); // PUSH_CTRL = ENABLE
        write_mmio_reg(&mut machine, 0x8c, 1); // PUSH_PUT = 1 (never on the orbit)

        // Must return rather than hang. GET stays within the ring.
        machine.advance_devices(1);
        assert!(read_mmio_reg(&mut machine, 0x90) < 10);
    }

    #[test]
    fn pusher_get_trails_put_until_commands_complete() {
        let mut machine = test_machine();
        // Two single-pixel FILLs in the ring. Common setup (DST_BASE, DST_PITCH,
        // DEPTH, ROP) first, then per-fill DST_XY, DIM, FG_COLOR, COMMAND: 0xAA at
        // (1,1) and 0xBB at (3,3). Header words are (count << 16) | method.
        let ring_base = 0x0001_0000u32;
        let ring: [u32; 23] = [
            // Common setup: 7 words.
            (2 << 16) | 0x0100,
            0, // DST_BASE = 0
            8, // DST_PITCH = 8
            (1 << 16) | 0x0110,
            1, // DEPTH = 1
            (1 << 16) | 0x0128,
            0xf0, // ROP = PATCOPY
            // Fill 1: 8 words (cumulative 15 words = 60 bytes after this).
            (1 << 16) | 0x0114,
            (1 << 16) | 1, // DST_XY: y=1, x=1
            (1 << 16) | 0x011c,
            (1 << 16) | 1, // DIM: h=1, w=1
            (1 << 16) | 0x0120,
            0xaa, // FG_COLOR = 0xAA
            (1 << 16) | 0x0150,
            0x01, // COMMAND = FILL
            // Fill 2: 8 words (cumulative 23 words = 92 bytes = PUT).
            (1 << 16) | 0x0114,
            (3 << 16) | 3, // DST_XY: y=3, x=3
            (1 << 16) | 0x011c,
            (1 << 16) | 1, // DIM: h=1, w=1
            (1 << 16) | 0x0120,
            0xbb, // FG_COLOR = 0xBB
            (1 << 16) | 0x0150,
            0x01, // COMMAND = FILL
        ];
        for (i, word) in ring.iter().enumerate() {
            for (b, byte) in word.to_le_bytes().into_iter().enumerate() {
                machine.write_physical_u8(ring_base + (i * 4 + b) as u32, byte);
            }
        }
        let put = (ring.len() * 4) as u32; // 92
        let after_fill1 = 15 * 4u32; // 60: offset just past fill 1's COMMAND packet

        write_mmio_reg(&mut machine, 0x84, ring_base); // PUSH_BASE
        write_mmio_reg(&mut machine, 0x88, 0x1000); // PUSH_SIZE
        write_mmio_reg(&mut machine, 0x80, 1); // PUSH_CTRL = ENABLE
        write_mmio_reg(&mut machine, 0x8c, put); // PUSH_PUT = doorbell

        // One tick: the pump consumes the setup plus fill 1, which sets busy_ns and
        // stalls the pump. GET trails PUT, fill 1 landed, fill 2 has not run yet.
        machine.advance_devices(1);
        assert_eq!(read_mmio_reg(&mut machine, 0x90), after_fill1); // GET lags PUT
        assert_ne!(read_mmio_reg(&mut machine, 0x90), put);
        assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 8 + 1), 0xaa); // (1,1)
        assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 3 * 8 + 3), 0x00); // (3,3) not yet

        // Enough ticks to drain fill 1's busy_ns (a 1-pixel fill is 105 ns; 10
        // clocks at 25 MHz = 400 ns), letting the pump consume fill 2.
        machine.advance_devices(10);
        assert_eq!(read_mmio_reg(&mut machine, 0x90), put); // GET caught up
        assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 3 * 8 + 3), 0xbb); // (3,3) now
    }

    #[test]
    fn pusher_streams_color_expand_data_through_the_ring() {
        let mut machine = test_machine();
        // The pusher arms COLOR_EXPAND_DATA and then streams its MONO_DATA words from
        // the ring. This works only because the pump gates on busy_ns (arming leaves
        // busy_ns at 0, so the pump keeps feeding the stream) rather than STATUS.BUSY.
        // An 8x2 glyph at (0,0), depth 1, pitch 8, FG 0xAB, BG 0x00, ROP SRCCOPY: row
        // 0 bits 0xA0 (x=0,2 set), row 1 bits 0x50 (x=1,3 set); MONO_DATA is MSB-first
        // in the high byte. Each MONO_DATA word is its own packet (the port is a single
        // register at 0x0160, so a count>1 run would scatter to 0x0164 and beyond).
        let ring_base = 0x0001_0000u32;
        let ring: [u32; 22] = [
            (2 << 16) | 0x0100,
            0, // DST_BASE = 0
            8, // DST_PITCH = 8
            (1 << 16) | 0x0110,
            1, // DEPTH = 1
            (1 << 16) | 0x0114,
            0, // DST_XY = (0, 0)
            (1 << 16) | 0x011c,
            (2 << 16) | 8, // DIM: h=2, w=8
            (2 << 16) | 0x0120,
            0xab, // FG_COLOR
            0x00, // BG_COLOR
            (1 << 16) | 0x0128,
            0xcc, // ROP = SRCCOPY (S = expanded pixel)
            (1 << 16) | 0x0130,
            0, // FLAGS = 0 (clear bits painted with BG)
            (1 << 16) | 0x0150,
            0x03, // COMMAND = COLOR_EXPAND_DATA (arms the stream; no busy_ns yet)
            (1 << 16) | 0x0160,
            0xa000_0000, // MONO_DATA row 0: bits 0xA0 in the high byte
            (1 << 16) | 0x0160,
            0x5000_0000, // MONO_DATA row 1: bits 0x50 in the high byte
        ];
        for (i, word) in ring.iter().enumerate() {
            for (b, byte) in word.to_le_bytes().into_iter().enumerate() {
                machine.write_physical_u8(ring_base + (i * 4 + b) as u32, byte);
            }
        }
        let put = (ring.len() * 4) as u32; // 88

        write_mmio_reg(&mut machine, 0x84, ring_base); // PUSH_BASE
        write_mmio_reg(&mut machine, 0x88, 0x1000); // PUSH_SIZE
        write_mmio_reg(&mut machine, 0x80, 1); // PUSH_CTRL = ENABLE
        write_mmio_reg(&mut machine, 0x8c, put); // PUSH_PUT = doorbell

        machine.advance_devices(1);

        // Row 0: set bits at x=0,2 -> 0xAB; clear bits -> 0x00 (BG).
        assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE), 0xab); // (0,0)
        assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 1), 0x00); // (1,0)
        assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 2), 0xab); // (2,0)
        assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 3), 0x00); // (3,0)
        // Row 1: set bits at x=1,3 -> 0xAB.
        assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 8), 0x00); // (0,1)
        assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 9), 0xab); // (1,1)
        assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 10), 0x00); // (2,1)
        assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 11), 0xab); // (3,1)
        // The whole ring drained.
        assert_eq!(read_mmio_reg(&mut machine, 0x90), put);
    }

    #[test]
    fn dos_program_startup_services() {
        // org 0x100:
        //   mov ah,0x30 / int 0x21            ; get version (AL=6, AH=10), no fault
        //   mov bx,0x10 / mov ah,0x48 / int 21 ; allocate 16 paras -> AX=0x1100
        //   mov [0x0204],ax                    ; store the allocated segment
        //   mov ax,0x3521 / int 0x21           ; get INT 21h vector -> ES=0, BX=0x0600
        //   mov [0x0200],es / mov [0x0202],bx  ; store ES then BX
        //   mov ax,0x4c00 / int 0x21           ; exit 0
        let com: &[u8] = &[
            0xb4, 0x30, 0xcd, 0x21, 0xbb, 0x10, 0x00, 0xb4, 0x48, 0xcd, 0x21, 0xa3, 0x04, 0x02,
            0xb8, 0x21, 0x35, 0xcd, 0x21, 0x8c, 0x06, 0x00, 0x02, 0x89, 0x1e, 0x02, 0x02, 0xb8,
            0x00, 0x4c, 0xcd, 0x21,
        ];
        let mut machine =
            Machine::new_dos_program(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), com)
                .unwrap();
        let reason = machine.run_until_halt_or_cycles(100_000).unwrap();
        assert_eq!(reason, StopReason::DosExit { code: 0 });
        let mem = machine.memory();
        // PSP at 0x0100 -> linear 0x1000; the program stored words at offsets 0x200..0x205.
        assert_eq!(mem.read_u16(0x1200).unwrap(), 0x0000); // ES from IVT[0x21] (stub segment)
        assert_eq!(mem.read_u16(0x1202).unwrap(), 0x0600); // BX from IVT[0x21] (stub offset)
        assert_eq!(mem.read_u16(0x1204).unwrap(), 0x1100); // AH=48h allocated segment
    }

    #[test]
    fn mode_x_a0000_writes_route_to_the_planar_datapath() {
        let mut machine = test_machine();
        // Mode 13h then unchained (chain-4 off).
        machine.video_mut().set_mode13h();
        machine.video_mut().write_port(0x3C4, 0x04);
        machine.video_mut().write_port(0x3C5, 0x06);
        assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
        // Map mask = plane 2, full bit mask, write mode 0 (reset default).
        machine.video_mut().write_port(0x3C4, 0x02);
        machine.video_mut().write_port(0x3C5, 0x04); // plane 2
        machine.video_mut().write_port(0x3CE, 0x08);
        machine.video_mut().write_port(0x3CF, 0xFF); // bit mask 0xFF
        machine.write_physical_u8(0x000A_0000 + 5, 0x9C);
        assert_eq!(machine.video().plane_byte(2, 5), 0x9C);
        // An offset past the old 64000-byte mode-13h cap is reachable in the 64 KB
        // unchained planar window.
        machine.write_physical_u8(0x000A_0000 + 0xFB00, 0x3C);
        assert_eq!(machine.video().plane_byte(2, 0xFB00), 0x3C);
        // Read back through the bus read path: select plane 2 as the read-map source,
        // then the A0000 reads return the bytes written above (proving cpu_read routes
        // through the 64 KB window too, including past the old 64000-byte cap).
        machine.video_mut().write_port(0x3CE, 0x04); // GC Read Map Select
        machine.video_mut().write_port(0x3CF, 0x02); // plane 2
        assert_eq!(machine.read_physical_u8(0x000A_0000 + 5), 0x9C);
        assert_eq!(machine.read_physical_u8(0x000A_0000 + 0xFB00), 0x3C);
    }

    #[test]
    fn mode_x_320x240_through_the_machine() {
        let mut machine = test_machine();
        // Mode 13h, then unchained mode X.
        machine.video_mut().set_mode13h();
        machine.video_mut().write_port(0x3C4, 0x04);
        machine.video_mut().write_port(0x3C5, 0x06);
        assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
        // Abrash's 320x240 vertical timing through the CRTC ports.
        for (idx, val) in [
            (0x06u8, 0x0Du8),
            (0x07, 0x3E),
            (0x09, 0x41),
            (0x10, 0xEA),
            (0x11, 0xAC),
            (0x12, 0xDF),
            (0x15, 0xE7),
            (0x16, 0x06),
        ] {
            machine.video_mut().write_port(0x3D4, idx);
            machine.video_mut().write_port(0x3D5, val);
        }
        // Draw a pixel at column 6: plane 6 & 3 = 2, plane offset 6 >> 2 = 1.
        machine.video_mut().write_port(0x3C4, 0x02);
        machine.video_mut().write_port(0x3C5, 0x04); // map mask = plane 2
        machine.video_mut().write_port(0x3CE, 0x08);
        machine.video_mut().write_port(0x3CF, 0xFF); // bit mask 0xFF
        machine.write_physical_u8(0x000A_0000 + 1, 0xC2); // plane 2, offset 1; bits 6-7 set prove no 6-bit mask
        // Complete a frame (mode-X 320x240 frame is ~421 600 dots; 500 000 clocks is
        // ~503 500 dots, enough to cross one frame and present).
        machine.advance_devices(500_000);
        let raster = machine.vga_raster().expect("a frame presented");
        assert_eq!(raster.width, 320);
        assert_eq!(raster.height, 527, "320x240 vertical total");
        // Column 6 of row 0 scans out the drawn 0xC2, as the 8-bit DAC index directly.
        assert_eq!(
            raster.pixels[6], 0xC2,
            "mode-X pixel scans out at its column with its full 8-bit value"
        );
    }

    #[test]
    fn mode_x_line_compare_split_through_the_machine() {
        let mut machine = test_machine();
        // Mode 13h, then unchained mode X.
        machine.video_mut().set_mode13h();
        machine.video_mut().write_port(0x3C4, 0x04);
        machine.video_mut().write_port(0x3C5, 0x06);
        assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
        // Abrash's 320x240 vertical timing through the CRTC ports (Black Book Listing
        // 47.1): double-scanned, 240 source rows over 480 scanlines.
        for (idx, val) in [
            (0x06u8, 0x0Du8),
            (0x07, 0x3E),
            (0x09, 0x41),
            (0x10, 0xEA),
            (0x11, 0xAC),
            (0x12, 0xDF),
            (0x15, 0xE7),
            (0x16, 0x06),
        ] {
            machine.video_mut().write_port(0x3D4, idx);
            machine.video_mut().write_port(0x3D5, val);
        }
        // Program a split at scan-counter line 200. The 320x240 bang sets 07h bit 4
        // (line-compare bit 8) and 09h bit 6 (line-compare bit 9); rewrite both with
        // their other overflow / max-scan bits intact but those two line-compare bits
        // clear, then the low byte. The kept bits reproduce vtotal 527, vdisp_end 480
        // and keep double-scan on; only line-compare bits 8 and 9 are forced to 0.
        machine.video_mut().write_port(0x3D4, 0x07);
        machine.video_mut().write_port(0x3D5, 0x2E); // overflow minus line-compare bit 8
        machine.video_mut().write_port(0x3D4, 0x09);
        machine.video_mut().write_port(0x3D5, 0x01); // max scan 1 (double-scan), bit 6 clear
        machine.video_mut().write_port(0x3D4, 0x18);
        machine.video_mut().write_port(0x3D5, 0xC8); // line compare low 8 = 200
        // Mark the status panel: plane 0, offset 0 (pixel 0 of any scanline reading
        // offset 0). 0xC2 has bits above 0x3F set, proving the 8-bit DAC index is read
        // directly with no attribute 6-bit mask.
        machine.video_mut().write_port(0x3C4, 0x02);
        machine.video_mut().write_port(0x3C5, 0x01); // map mask = plane 0
        machine.video_mut().write_port(0x3CE, 0x08);
        machine.video_mut().write_port(0x3CF, 0xFF); // bit mask 0xFF
        machine.write_physical_u8(0x000A_0000, 0xC2);
        // Scroll the top region to cleared VRAM, buffered until the next vertical
        // retrace. Two frame periods: the first latches the start address, the second
        // renders with it (the vretrace latch is exercised the same way as the 16-color
        // split test).
        machine.video_mut().write_port(0x3D4, 0x0C);
        machine.video_mut().write_port(0x3D5, 0x40); // start address high = 0x40
        machine.video_mut().write_port(0x3D4, 0x0D);
        machine.video_mut().write_port(0x3D5, 0x00); // start address low = 0x00 -> 0x4000
        machine.advance_devices(500_000);
        machine.advance_devices(500_000);
        let raster = machine.vga_raster().expect("a frame presented");
        assert_eq!(raster.width, 320, "mode-X width");
        let w = raster.width as usize;
        // A top scanline (50 < 200) reads the scrolled, cleared region: 0.
        assert_eq!(
            raster.pixels[50 * w],
            0,
            "top region is scrolled to cleared VRAM"
        );
        // The first split scanline (201 = line_compare + 1) reads offset 0 (the marked
        // status panel), as the full 8-bit DAC index.
        assert_eq!(
            raster.pixels[201 * w],
            0xC2,
            "split region reads offset 0 at the full 8-bit value"
        );
    }

    #[test]
    fn dos_program_writes_and_reads_back_a_file() {
        // org 0x100: create C:\OUT.TXT (AH=3Ch), write "HI!" (AH=40h to the file
        // handle), seek to 0 (AH=42h), read 3 bytes back (AH=3Fh), close (AH=3Eh),
        // write the buffer to stdout (AH=40h, BX=1), exit (AH=4Ch). Data follows the
        // code: fname at 0x13E, msg "HI!" at 0x149, buf at 0x14C.
        let com: &[u8] = &[
            0xb4, 0x3c, 0x31, 0xc9, 0xba, 0x3e, 0x01, 0xcd, 0x21, 0x89, 0xc3, 0xb4, 0x40, 0xb9,
            0x03, 0x00, 0xba, 0x49, 0x01, 0xcd, 0x21, 0xb8, 0x00, 0x42, 0x31, 0xc9, 0x31, 0xd2,
            0xcd, 0x21, 0xb4, 0x3f, 0xb9, 0x03, 0x00, 0xba, 0x4c, 0x01, 0xcd, 0x21, 0xb4, 0x3e,
            0xcd, 0x21, 0xb4, 0x40, 0xbb, 0x01, 0x00, 0xb9, 0x03, 0x00, 0xba, 0x4c, 0x01, 0xcd,
            0x21, 0xb8, 0x00, 0x4c, 0xcd, 0x21, // fname "C:\\OUT.TXT\0"
            0x43, 0x3a, 0x5c, 0x4f, 0x55, 0x54, 0x2e, 0x54, 0x58, 0x54, 0x00,
            // msg "HI!"
            0x48, 0x49, 0x21, // buf (3 bytes)
            0x00, 0x00, 0x00,
        ];
        let dir = tempfile::tempdir().unwrap();
        let mut machine =
            Machine::new_dos_program(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), com)
                .unwrap();
        machine.mount_c_drive(izarravm_dos::HostDrive::mount_c(dir.path()).unwrap());
        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::DosExit { code: 0 });
        assert_eq!(machine.dos_output(), b"HI!");
        assert_eq!(std::fs::read(dir.path().join("OUT.TXT")).unwrap(), b"HI!");
    }

    #[test]
    fn dos_program_enumerates_files() {
        // org 0x100: FindFirst "C:\\*.TXT" (AH=4Eh, CX=0). Then loop: if CF set, exit;
        // else write the 13-byte DTA name field (PSP:0x9E) to stdout (AH=40h, BX=1,
        // CX=13), FindNext (AH=4Fh), repeat. The default DTA is PSP:0x80, so the name
        // field is at PSP:0x9E. Pattern "C:\\*.TXT\0" sits at 0x123.
        //
        //   off 0:  b4 4e        mov ah, 4Eh
        //   off 2:  31 c9        xor cx, cx
        //   off 4:  ba 23 01     mov dx, 0x123
        //   off 7:  cd 21        int 21h
        // loop (off 9):
        //   off 9:  72 13        jc done (+0x13 -> off 30)
        //   off 11: b4 40        mov ah, 40h
        //   off 13: bb 01 00     mov bx, 1
        //   off 16: b9 0d 00     mov cx, 13
        //   off 19: ba 9e 00     mov dx, 0x9E
        //   off 22: cd 21        int 21h
        //   off 24: b4 4f        mov ah, 4Fh
        //   off 26: cd 21        int 21h
        //   off 28: eb eb        jmp loop (-0x15 -> off 9)
        // done (off 30):
        //   off 30: b8 00 4c     mov ax, 4C00h
        //   off 33: cd 21        int 21h
        //   off 35: "C:\\*.TXT", 0
        let com: &[u8] = &[
            0xb4, 0x4e, 0x31, 0xc9, 0xba, 0x23, 0x01, 0xcd, 0x21, 0x72, 0x13, 0xb4, 0x40, 0xbb,
            0x01, 0x00, 0xb9, 0x0d, 0x00, 0xba, 0x9e, 0x00, 0xcd, 0x21, 0xb4, 0x4f, 0xcd, 0x21,
            0xeb, 0xeb, 0xb8, 0x00, 0x4c, 0xcd, 0x21, 0x43, 0x3a, 0x5c, 0x2a, 0x2e, 0x54, 0x58,
            0x54, 0x00,
        ];
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ONE.TXT"), b"a").unwrap();
        std::fs::write(dir.path().join("TWO.TXT"), b"bb").unwrap();
        let mut machine =
            Machine::new_dos_program(MachineProfile::i386dx25(16, VideoCard::Et4000Ax), com)
                .unwrap();
        machine.mount_c_drive(izarravm_dos::HostDrive::mount_c(dir.path()).unwrap());
        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::DosExit { code: 0 });

        // Each found name was written as the 13-byte ASCIIZ field; split on NUL.
        let mut names: Vec<String> = machine
            .dos_output()
            .split(|&b| b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["ONE.TXT", "TWO.TXT"]);
    }

    #[test]
    fn overlay_quantizes_to_16bpp_display_without_dither() {
        let mut machine = test_machine();
        machine.margo_mut().set_mode(0x111); // 640x480x16 (R5G6B5)
        // A uniform gray YUY2 source (Y=130, U=128, V=128 -> yuv_to_argb = 0x858585),
        // 4 pixels (2 packed groups: Y0,U,Y1,V), offscreen at 1 MiB.
        let src = 0x0010_0000u32;
        for g in 0..2u32 {
            let base = src + g * 4;
            machine.write_physical_u8(MARGO_LFB_BASE + base, 130); // Y0
            machine.write_physical_u8(MARGO_LFB_BASE + base + 1, 128); // U
            machine.write_physical_u8(MARGO_LFB_BASE + base + 2, 130); // Y1
            machine.write_physical_u8(MARGO_LFB_BASE + base + 3, 128); // V
        }
        write_mmio_reg(&mut machine, 0x44, src); // OVL_SRC_Y
        write_mmio_reg(&mut machine, 0x48, 8); // OVL_SRC_PITCH
        write_mmio_reg(&mut machine, 0x4c, (1 << 16) | 4); // OVL_SRC_DIM: 4x1
        write_mmio_reg(&mut machine, 0x58, 0); // OVL_DST_XY: (0, 0)
        write_mmio_reg(&mut machine, 0x5c, (1 << 16) | 4); // OVL_DST_DIM: 4x1 (1:1)
        write_mmio_reg(&mut machine, 0x0c, 0); // CONTROL: DITHER_EN off
        write_mmio_reg(&mut machine, 0x40, 1); // OVL_CTRL: ENABLE, YUY2

        let palette = machine.palette_argb();
        let argb = machine.margo().scanout_argb(&palette);
        // On a 16bpp display the overlay is reduced to R5G6B5 and bit-expanded back:
        // 0x858585 -> 0x848684 (R/B truncate to 0x84, G to 0x86), uniform (no dither).
        for (x, &pixel) in argb.iter().enumerate().take(4) {
            assert_eq!(pixel, 0x0084_8684, "pixel {x}");
        }
    }

    #[test]
    fn overlay_orders_dither_on_a_16bpp_display() {
        let mut machine = test_machine();
        machine.margo_mut().set_mode(0x111); // 640x480x16
        let src = 0x0010_0000u32;
        for g in 0..2u32 {
            let base = src + g * 4;
            machine.write_physical_u8(MARGO_LFB_BASE + base, 130);
            machine.write_physical_u8(MARGO_LFB_BASE + base + 1, 128);
            machine.write_physical_u8(MARGO_LFB_BASE + base + 2, 130);
            machine.write_physical_u8(MARGO_LFB_BASE + base + 3, 128);
        }
        write_mmio_reg(&mut machine, 0x44, src);
        write_mmio_reg(&mut machine, 0x48, 8);
        write_mmio_reg(&mut machine, 0x4c, (1 << 16) | 4);
        write_mmio_reg(&mut machine, 0x58, 0);
        write_mmio_reg(&mut machine, 0x5c, (1 << 16) | 4);
        write_mmio_reg(&mut machine, 0x0c, 0x2); // CONTROL: DITHER_EN on
        write_mmio_reg(&mut machine, 0x40, 1); // OVL_CTRL: ENABLE, YUY2

        let palette = machine.palette_argb();
        let argb = machine.margo().scanout_argb(&palette);
        // Row 0 Bayer cells are 0, 8, 2, 10. For gray 0x858585 the R/B (5-bit) jump
        // a step where the cell offset pushes 133 past the 17th code: cells 8 and 10
        // dither up to 0x8C, cells 0 and 2 stay at 0x84. G (6-bit) stays 0x86.
        assert_eq!(argb[0], 0x0084_8684); // cell 0
        assert_eq!(argb[1], 0x008c_868c); // cell 8
        assert_eq!(argb[2], 0x0084_8684); // cell 2
        assert_eq!(argb[3], 0x008c_868c); // cell 10
    }

    #[test]
    fn overlay_dithers_on_a_15bpp_display() {
        let mut machine = test_machine();
        machine.margo_mut().set_mode(0x110); // 640x480x15 (X1R5G5B5): all channels 5-bit
        let src = 0x0010_0000u32;
        for g in 0..2u32 {
            let base = src + g * 4;
            machine.write_physical_u8(MARGO_LFB_BASE + base, 130); // Y0
            machine.write_physical_u8(MARGO_LFB_BASE + base + 1, 128); // U
            machine.write_physical_u8(MARGO_LFB_BASE + base + 2, 130); // Y1
            machine.write_physical_u8(MARGO_LFB_BASE + base + 3, 128); // V
        }
        write_mmio_reg(&mut machine, 0x44, src);
        write_mmio_reg(&mut machine, 0x48, 8);
        write_mmio_reg(&mut machine, 0x4c, (1 << 16) | 4);
        write_mmio_reg(&mut machine, 0x58, 0);
        write_mmio_reg(&mut machine, 0x5c, (1 << 16) | 4);
        write_mmio_reg(&mut machine, 0x0c, 0x2); // CONTROL: DITHER_EN on
        write_mmio_reg(&mut machine, 0x40, 1); // OVL_CTRL: ENABLE, YUY2

        let palette = machine.palette_argb();
        let argb = machine.margo().scanout_argb(&palette);
        // 15bpp makes G 5-bit too (unlike 16bpp's 6-bit G), so a dithered-up pixel is
        // gray 0x8C8C8C, not 0x8C868C. Row 0 cells 0, 8, 2, 10 -> 0x84, 0x8C, 0x84, 0x8C.
        assert_eq!(argb[0], 0x0084_8484); // cell 0: truncated gray
        assert_eq!(argb[1], 0x008c_8c8c); // cell 8: dithered up
        assert_eq!(argb[2], 0x0084_8484); // cell 2
        assert_eq!(argb[3], 0x008c_8c8c); // cell 10
    }

    #[test]
    fn overlay_dither_is_locked_to_screen_position() {
        let mut machine = test_machine();
        machine.margo_mut().set_mode(0x111); // 640x480x16
        // Uniform gray YUY2 source, 4x4 (4 rows x 2 packed groups = 8 groups), offscreen.
        let src = 0x0010_0000u32;
        for g in 0..8u32 {
            let base = src + g * 4;
            machine.write_physical_u8(MARGO_LFB_BASE + base, 130); // Y0
            machine.write_physical_u8(MARGO_LFB_BASE + base + 1, 128); // U
            machine.write_physical_u8(MARGO_LFB_BASE + base + 2, 130); // Y1
            machine.write_physical_u8(MARGO_LFB_BASE + base + 3, 128); // V
        }
        write_mmio_reg(&mut machine, 0x44, src);
        write_mmio_reg(&mut machine, 0x48, 8); // src pitch: 2 groups per row
        write_mmio_reg(&mut machine, 0x4c, (4 << 16) | 4); // OVL_SRC_DIM: 4x4
        write_mmio_reg(&mut machine, 0x58, (2 << 16) | 1); // OVL_DST_XY: x=1, y=2 (non-aligned)
        write_mmio_reg(&mut machine, 0x5c, (4 << 16) | 4); // OVL_DST_DIM: 4x4 (1:1)
        write_mmio_reg(&mut machine, 0x0c, 0x2); // CONTROL: DITHER_EN on
        write_mmio_reg(&mut machine, 0x40, 1); // OVL_CTRL: ENABLE, YUY2

        let palette = machine.palette_argb();
        let argb = machine.margo().scanout_argb(&palette);
        // The dither cell is BAYER[screen_y & 3][screen_x & 3] in ABSOLUTE screen
        // coordinates, not destination-relative. If it were dst-relative, screen (1,2)
        // would be cell 0 (0x848684); screen-locked it is BAYER[2][1] = 11.
        assert_eq!(argb[2 * 640 + 1], 0x008c_868c); // screen (1,2): cell 11
        assert_eq!(argb[2 * 640 + 4], 0x0084_8684); // screen (4,2): cell 3
        assert_eq!(argb[5 * 640 + 2], 0x008c_8a8c); // screen (2,5): cell 14
    }
}
