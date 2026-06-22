pub use fat12::build_fat12;
pub use fat32::{
    FAT_ATTR_DIRECTORY, FAT32_EOC, Fat32Geometry, Fat32Table, fat32_boot_sector, fat32_dir_entry,
    fat32_dot_entries, fat32_fsinfo_sector, fat32_geometry, fat32_is_eoc,
};
pub use fat32_volume::{Fat32Volume, build_fat32};
use izarravm_audio::{Ad1848, Ad1848Config, OplChip, Resampler, SbDsp, SbMixer};
use izarravm_bus::{BusAccessKind, BusCycle, BusError, BusTrace, BusWidth, CpuBus, Memory};
use izarravm_core::{GswMode, HardwareProfile, SoundBlasterConfig, VideoCard, WssConfig};
use izarravm_cpu::{
    Cpu386, CpuError, CpuLevel, CycleOutcome, Registers, SegmentIndex, SegmentRegister,
};
pub use izarravm_video::MARGO_ID_VALUE;
use izarravm_video::{
    DAC_ENTRIES, MARGO_MMIO_SIZE, MARGO_VBE_MODES, MARGO_VRAM_SIZE, Margo, TextFrame,
    VGA_MODE13H_BASE, VGA_PLANAR_WINDOW_SIZE, VGA_TEXT_BASE, VGA_TEXT_MEMORY_SIZE,
    VGA_TEXT_PAGE_STRIDE, Vga, VgaRaster, VideoMode, bytes_per_pixel, pixel_format, vbe_mode,
};
use thiserror::Error;

mod ata;
mod atapi;
mod cdimage;
mod dma;
mod fat12;
mod fat32;
mod fat32_volume;
mod fat_name;
mod fdc;
mod floppy;
mod ide;
mod keyboard;
mod lpt;
mod memmap;
mod pic;
mod pit;
mod rtc;
mod speaker;
mod uart;
mod uma;
mod unittester;
mod xms;

pub use cdimage::CdImage;
pub use memmap::{
    CONVENTIONAL_TOP, HMA_BASE, HMA_TOP, MemRegion, SYSTEM_ROM_BASE, UPPER_MEMORY_BASE,
    VIDEO_RAM_BASE, classify, is_hma, is_umb_window,
};
pub use uma::{EMS_FRAME_SIZE, UmaReservation, UmaReservationMap, UmaUse};

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

/// Lotura system-controller identifier, mirroring the Margo card's MARGO_ID_VALUE
/// convention (a fixed nonzero byte the guest can probe).
pub const LOTURA_ID_VALUE: u8 = 0x5a;

/// Drive number the ICDEX HLE exposes the CD-ROM at (0 = A:). The CD is D:,
/// after A: floppy and C: host drive.
///
/// ICDEX = Izarra CD-ROM Extensions, the Toka-DOS CD redirector. Its INT 2Fh
/// interface is intentionally ABI-compatible with the CD extension interface
/// DOS games probe for, so titles detect the drive without a real driver.
pub const CD_DRIVE_NUMBER: u8 = 3;

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
    pub cpu: GswMode,
    pub clock_hz: u64,
    pub memory_mib: u16,
    pub video: VideoCard,
    /// Power-on CT1745 mixer routing (IRQ/DMA) + host enable flag, applied to
    /// the mixer at construction. A guest mixer reset still restores the
    /// hardware factory default (IRQ5/DMA1/DMA5).
    pub sound_blaster: SoundBlasterConfig,
    /// Power-on Windows Sound System (AD1848 codec) base/IRQ/DMA + enable flag.
    /// The codec decodes its own resources concurrently with the SB16; disabling
    /// it leaves the SB16/OPL paths untouched.
    pub wss: WssConfig,
    pub wait_states: WaitStateProfile,
    pub address_pipelining: bool,
    pub cache_enabled: bool,
}

impl MachineProfile {
    pub fn gsw_386(memory_mib: u16, video: VideoCard) -> Self {
        Self {
            cpu: GswMode::Gsw386,
            clock_hz: GswMode::Gsw386.clock_hz(),
            memory_mib,
            video,
            sound_blaster: SoundBlasterConfig::default(),
            wss: WssConfig::default(),
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
            sound_blaster: profile.sound_blaster,
            wss: profile.wss,
            wait_states: WaitStateProfile::default(),
            address_pipelining: false,
            cache_enabled: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    Halted,
    CycleLimit {
        requested: u64,
    },
    CpuError(String),
    DosExit {
        code: u8,
    },
    /// The guest issued the unit tester's Exit command (Lotura port 0xE6) with
    /// this code. A CI harness maps it straight to a process exit status.
    TestExit {
        code: u8,
    },
}

/// A frozen parent CPU state for EXEC (AH=4Bh AL=0) resume: the register file as
/// handle_dos_int left it at the parent's AH=4Bh INT, so restoring it lands the
/// CPU back on the IRET stub with the parent's INT-return frame on the stack.
#[derive(Debug)]
struct ProgramFrame {
    registers: Registers,
}

/// The OPL3 renders at this native rate; the Resonique 2 DAC outputs at 44100.
const OPL_NATIVE_HZ: u32 = 49_716;
const DAC_HZ: u32 = 44_100;
/// Standard PC PIT input clock frequency.
const PIT_INPUT_HZ: u32 = 1_193_182;
/// VGA 25.175 MHz dot clock (standard 640x480 and related modes).
const VGA_DOT_HZ: u64 = 25_175_000;
/// Fallback cadence (Hz) for retiring the AD1848 autocal (ACI) window when the
/// programmed sample rate is one of the two unsupported XTAL1 selects
/// (`rate_hz()==0`). On real hardware the autocal converter clock retires the
/// ~128-sample window regardless of the programmed rate; this is only used to
/// clock the ACI countdown, never to produce audio. 8000 Hz is the lowest
/// documented WSS rate (XTAL1, CFS=0).
const WSS_AUTOCAL_FALLBACK_HZ: u32 = 8000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveDisplay {
    VgaRaster,
    MargoLfb,
}

/// Per-clock conversion factors, recomputed once whenever the active mode (clock)
/// changes, so the per-instruction device pacing multiplies instead of dividing.
#[derive(Debug, Clone, Copy)]
struct TimingFactors {
    micros_per_clock: f64,   // 1e6 / clock_hz (OPL and DSP settle)
    pit_per_clock: f64,      // PIT_INPUT_HZ / clock_hz
    margo_ns_per_clock: f64, // 1e9 / clock_hz
    vga_dots_per_clock: f64, // VGA_DOT_HZ / clock_hz
    inv_clock: f64,          // 1 / clock_hz (DSP sample phase and the speaker)
    // CPU clocks in one 44.1 kHz DAC sample. The run loop batches instructions
    // up to this many clocks before servicing devices once, so the per-clock
    // fine-samplers (the PC speaker reads ch2 OUT once per advance_devices, the
    // DSP/CD producers step at the DAC rate) still see at most one sample of
    // time per call and never alias. >=1 in every mode (clock_hz >> 44100).
    clocks_per_audio_sample: u64,
}

impl TimingFactors {
    fn for_clock(clock_hz: u64) -> Self {
        let c = clock_hz as f64;
        Self {
            micros_per_clock: 1_000_000.0 / c,
            pit_per_clock: PIT_INPUT_HZ as f64 / c,
            margo_ns_per_clock: 1_000_000_000.0 / c,
            vga_dots_per_clock: VGA_DOT_HZ as f64 / c,
            inv_clock: 1.0 / c,
            clocks_per_audio_sample: (clock_hz / u64::from(DAC_HZ)).max(1),
        }
    }
}

#[derive(Debug)]
pub struct Machine {
    profile: MachineProfile,
    active_mode: GswMode,
    pending_mode: Option<GswMode>,
    timing: TimingFactors,
    cpu: Cpu386,
    memory: Memory,
    // Boxed: Vga is ~99 KB. Inline, the Machine value (and its Result wrapper)
    // got copied through the constructors enough times in debug builds to
    // overflow the main-thread stack before the binary did any work. On the heap
    // it costs one pointer and the copies stay cheap.
    video: Box<Vga>,
    margo: Margo,
    margo_active: bool,
    pending_soft_int: Option<u8>, // software-INT vector awaiting deferred dispatch
    // Set by MachineBus on any port I/O; the run loop's instruction batch reads
    // it to know when to stop and service devices (see run_until_clock). A field
    // rather than a loop local so make_bus's one-off host accesses share it.
    io_touched: bool,
    // Toka-DOS service (Lotura port 0xE3): a write records the command here, the
    // run loop performs it after the cycle (it needs &mut self for host I/O), and
    // the resulting status is read back at 0xE3.
    pending_toka_service: Option<u8>,
    toka_service_status: u8,
    toka_c_root: Option<std::path::PathBuf>, // host C: root for Repair/Format
    // How many bytes of the DOS console output have already been teletyped onto
    // the VGA text screen. DOS CON output goes to the kernel's stdout buffer; the
    // machine mirrors the new bytes onto the framebuffer so the screen shows them.
    dos_screen_shown: usize,
    dos: izarravm_dos::DosKernel, // DOS kernel state: open files, drive, stdin/stdout
    rom: Vec<u8>,
    serial: uart::Uart16450,
    // COM2 (0x2F8-0x2FF, IRQ3). Same UART model as COM1; no host input source.
    serial2: uart::Uart16450,
    lpt: lpt::Lpt,
    // LPT2 (0x278-0x27A, IRQ5). Same printer model as LPT1.
    lpt2: lpt::Lpt,
    device_ports: DevicePorts,
    pic: pic::Pic8259Pair,
    pit: pit::Pit,
    keyboard: keyboard::Keyboard8042,
    speaker: speaker::Speaker,
    pit_clocks: f64, // fractional PIT input clocks owed to the counters
    dma: dma::DmaController,
    // 8272A floppy disk controller (ports 0x3F0-0x3F7). A guest that programs the
    // FDC directly drives it here; the INT 13h path stays HLE and does not use it.
    // READ/WRITE DATA move sector bytes over DMA channel 2 against `floppy`.
    fdc: fdc::Fdc,
    opl: OplChip,
    resampler: Resampler,
    opl_micros: f64, // fractional microseconds owed to the OPL timers
    dsp: SbDsp,
    /// DSP PCM resampler (rate_hz -> 44100), rebuilt when the programmed rate
    /// changes. Summed with the OPL stream in render_audio.
    dsp_resampler: Resampler,
    dsp_rate_hz: u32, // input rate the dsp_resampler is currently configured for
    dsp_micros: f64,  // fractional microseconds owed to the DSP reset-settle clock
    dsp_sample_phase: f64, // fractional DSP samples owed to the DMA playback clock
    mixer: SbMixer,   // the CT1745 mixer: IRQ/DMA routing + volume attenuation
    // AD1848 / Windows Sound System codec. An always-on combo-card device that
    // decodes its own base/IRQ/DMA concurrently with the SB16 + OPL3 (no mode
    // switch). The codec is independent of the CT1745 mixer; its I6/I7 DAC
    // attenuation is applied inside the codec at drain time, so render_audio sums
    // its resampled stream directly without the SB16 voice/master gain.
    wss: Ad1848,
    /// WSS PCM resampler (output_frame_rate -> 44100), rebuilt when the codec's
    /// programmed rate changes. Summed with the OPL + DSP streams in render_audio.
    wss_resampler: Resampler,
    wss_rate_hz: u32, // input rate the wss_resampler is currently configured for
    wss_sample_phase: f64, // fractional WSS frames owed to the DMA playback clock
    wss_base: u16,    // I/O base of the 4-port config region (codec sits at base+4)
    wss_irq: u8,      // PIC line the codec's terminal-count interrupt forwards to
    wss_dma: usize,   // byte-wide DMA channel the codec pulls playback bytes from
    wss_enabled: bool, // false drops all WSS work (port decode, tick, IRQ, render)
    margo_ns: f64,    // fractional nanoseconds owed to the Margo busy countdown
    vga_dots: f64,    // fractional VGA dot clocks owed to the beam advance
    trace: BusTrace,
    elapsed_clocks: u64,
    // Of elapsed_clocks, the clocks consumed by device I/O stalls (floppy seek/
    // read, later ATA) rather than executed instructions. A realtime host can
    // subtract these so blocking on a drive does not read as running over 100%.
    io_stall_clocks: u64,
    // Parent CPU snapshots for EXEC (AH=4Bh AL=0); popped on child exit.
    program_frames: Vec<ProgramFrame>,
    // Mounted A: floppy image, geometry inferred from the image length. INT 13h
    // disk services read and write it; None means the drive is empty.
    floppy: Option<floppy::Floppy>,
    // Monotonic counters bumped each time drive A: (INT 13h) or C: (DOS file I/O)
    // is touched. The GUI samples them per frame to flash a drive-access LED; a
    // counter never misses an event the way a poll-and-clear bool would.
    floppy_accesses: u64,
    c_accesses: u64,
    // ATAPI CD-ROM on the secondary IDE channel (0x170-0x177/0x376, IRQ15). It
    // owns the mounted disc image, the ATA register file, and the CD-audio
    // playback state the mixer streams.
    ide: ide::IdeChannel,
    // ATA hard disk on the primary IDE channel (0x1F0-0x1F7/0x3F6, IRQ14). The
    // boot drive C:; None when no image is mounted. INT 13h DL>=0x80 and the
    // primary-channel ports drive it.
    ata: Option<ata::AtaDisk>,
    // Synthesized read-only FAT32 volume serving drive C: to the DOS absolute-disk
    // interface (INT 25h read; INT 26h write is write-protected). Optional and
    // consulted only by INT 25h/26h for AL=2, so it does not touch the ATA / INT
    // 13h path. None until one is mounted. The eventual single C: backing (ATA
    // vs this) is the install-layout decision (P2).
    fat32_c: Option<Fat32Volume>,
    cd_accesses: u64,
    // Fractional Red Book frames owed to the CD-audio mixer from the DAC clock.
    cd_audio_frac: f64,
    // MC146818 RTC and CMOS NVRAM at ports 0x70/0x71.
    rtc: rtc::Rtc,
    // Fractional seconds owed to the RTC from the machine clock; whole seconds
    // are folded into the clock in advance_devices.
    rtc_seconds: f64,
    // Cosmetic POST pacing flag, read by the BIOS at port 0xE2. True (the
    // default) tells the ROM to skip the ~8 s RAM count-up and chime delays so
    // headless runs and unit tests finish inside their cycle budgets. The GUI
    // clears it after construction to keep the full power-on experience.
    fast_post: bool,
    // Booter-inert mode: when set, the Toka-DOS HLE and IEMM stand down so a
    // self-booting disk owns the DOS/memory-manager interrupts through the IVT
    // (the BIOS services stay intercepted). The single chokepoint that other
    // phases branch on; nothing auto-detects a booter yet, so it defaults off.
    booter_inert: bool,
    // INT 33h mouse-driver HLE state: virtual cursor position, button mask,
    // visibility, motion-counter accumulators, and the configured ranges. The
    // PS/2 aux device is the hardware side; this is the DOS driver a game calls.
    mouse: MouseState,
    // Guest-visible regression-test device (Lotura ports 0xE4-0xE6). A command
    // write records the request here; the run loop performs it after the cycle
    // (it needs &mut self for the framebuffer, host I/O, and the stop).
    unittester: unittester::UnitTester,
    // Where the unit tester's Snapshot command writes PPM frames, set by the
    // host. None disables snapshots (the command becomes a no-op).
    test_snapshot_path: Option<std::path::PathBuf>,
    // XMS (eXtended Memory Specification) driver state: the EMB allocator over
    // RAM above 1 MB, the HMA-allocation flag, and the local-A20 nesting count. A
    // guest reaches it via INT 2Fh AX=4310h (an INT 66h entry stub in ROM).
    xms: xms::XmsState,
}

/// INT 33h mouse-driver state. Coordinates are in a virtual screen space that
/// matches mode 13h (320x200) doubled horizontally, so x runs 0..639 and y runs
/// 0..199, the convention the Microsoft driver presents to graphics-mode games.
/// Host pixel deltas drive both this position and the mickey counters.
#[derive(Debug, Clone, PartialEq)]
struct MouseState {
    x: i32,          // virtual cursor column (clamped to [min_x, max_x])
    y: i32,          // virtual cursor row (clamped to [min_y, max_y])
    buttons: u8,     // bit0 left, bit1 right, bit2 middle
    show_count: i32, // cursor visibility counter (>=0 visible); hide decrements
    mickey_x: i32,   // horizontal motion accumulator (mickeys) since last read
    mickey_y: i32,   // vertical motion accumulator (mickeys) since last read
    min_x: i32,
    max_x: i32,
    min_y: i32,
    max_y: i32,
}

impl Default for MouseState {
    fn default() -> Self {
        Self {
            x: MOUSE_MAX_X / 2,
            y: MOUSE_MAX_Y / 2,
            buttons: 0,
            show_count: -1, // hidden until the driver calls Show (AX=0001)
            mickey_x: 0,
            mickey_y: 0,
            min_x: 0,
            max_x: MOUSE_MAX_X,
            min_y: 0,
            max_y: MOUSE_MAX_Y,
        }
    }
}

/// Virtual-screen bounds for the INT 33h cursor: a mode-13h game sees 0..639 x
/// 0..199. 320-wide modes scale x internally; the driver always reports this
/// doubled space, matching the Microsoft convention.
const MOUSE_MAX_X: i32 = 639;
const MOUSE_MAX_Y: i32 = 199;

/// Return `(min, max)` so a range function accepts its limits in either order.
fn order(a: i32, b: i32) -> (i32, i32) {
    if a <= b { (a, b) } else { (b, a) }
}

impl MouseState {
    /// Fold a host pixel delta into the cursor position and the mickey counters,
    /// and latch the new button mask. The mapping is one mickey per host pixel
    /// (a sane 1:1 default), so the cursor tracks the host pointer directly. The
    /// position is clamped to the configured ranges; the mickey counters are
    /// raw motion and are not clamped (they reset on read, AX=000Bh).
    fn apply_motion(&mut self, dx: i32, dy: i32, buttons: u8) {
        self.mickey_x += dx;
        self.mickey_y += dy;
        self.x = (self.x + dx).clamp(self.min_x, self.max_x);
        self.y = (self.y + dy).clamp(self.min_y, self.max_y);
        self.buttons = buttons & 0x07;
    }
}

/// Build the CT1745 mixer from the profile's Sound Blaster power-on routing.
/// The host config is applied once at construction like `SBCONFIG`; a guest
/// mixer reset (write `0x00`) still restores the hardware IRQ5/DMA1/DMA5.
fn power_on_mixer(profile: &MachineProfile) -> SbMixer {
    let sb = profile.sound_blaster;
    SbMixer::with_power_on(sb.irq.line(), sb.dma.channel(), sb.high_dma.channel())
}

/// Derive the DOS environment entries that advertise the Sound Blaster to
/// auto-detecting games. `BLASTER` and `SETSOUND` carry the same value:
/// `A220` (the fixed Resonique 2 base), `I`/`D`/`H` from the host config, and
/// `T6` (the SB16 card type). The MPU-401 base (`P`) is omitted until MIDI is
/// modeled. Returns an empty list when the card is disabled, so no `BLASTER`
/// leaks into a machine that has no SB16; the value always matches the routing
/// the CT1745 mixer answers, since both are derived from the same config.
fn sound_blaster_env_entries(config: &SoundBlasterConfig) -> Vec<(String, String)> {
    if !config.enabled {
        return Vec::new();
    }
    let value = format!(
        "A220 I{} D{} H{} T6",
        config.irq.line(),
        config.dma.channel(),
        config.high_dma.channel()
    );
    vec![
        ("BLASTER".to_string(), value.clone()),
        ("SETSOUND".to_string(), value),
    ]
}

impl Machine {
    /// Shared field initialization for the public constructors. They differ only
    /// in the CPU entry state and the ROM image, so each hands those in and
    /// shares the rest (devices, audio chips, timing accumulators). The caller
    /// installs the BIOS stubs and any boot/program image afterwards, where the
    /// ordering relative to those memory writes matters.
    fn base(profile: MachineProfile, cpu: Cpu386, mut rom: Vec<u8>) -> Result<Self, MachineError> {
        let mixer = power_on_mixer(&profile);
        // Build the AD1848 codec from the WSS board config. The codec's IRQ/DMA
        // jumper readback comes from the same WssConfig the env/detection use, so
        // the config region answers exactly what the codec is wired to. The base
        // and resource numbers are cached on the bus for the port decode and the
        // advance_devices DMA/IRQ feed (kept separate from the SB16's mixer).
        let wss_enabled = profile.wss.enabled;
        let wss_base = profile.wss.base;
        let wss_irq = profile.wss.irq.line();
        let wss_dma = profile.wss.dma.channel();
        let wss = Ad1848::new(Ad1848Config {
            irq: wss_irq,
            dma: wss_dma as u8,
        });
        let active_mode = profile.cpu;
        let memory_mib = profile.memory_mib;
        let timing = TimingFactors::for_clock(active_mode.clock_hz());
        // Lay the XMS driver entry stub into ROM. INT 2Fh AX=4310h hands the guest
        // a far pointer to it; a guest FAR-CALLs there, the INT 66h traps into the
        // host XMS handler, and the RETF returns. Placed at a fixed ROM offset that
        // no other BIOS stub uses (the IRET is at 0xF000, the reset vector at
        // 0xFFF0). See XMS_ENTRY_ROM_OFFSET for the bytes.
        rom[XMS_ENTRY_ROM_OFFSET..XMS_ENTRY_ROM_OFFSET + XMS_ENTRY_STUB.len()]
            .copy_from_slice(&XMS_ENTRY_STUB);
        let machine = Self {
            memory: Memory::from_mib(profile.memory_mib)?,
            profile,
            active_mode,
            pending_mode: None,
            timing,
            cpu,
            video: Box::new(Vga::default()),
            margo: Margo::default(),
            margo_active: false,
            pending_soft_int: None,
            io_touched: false,
            pending_toka_service: None,
            toka_service_status: 0,
            toka_c_root: None,
            dos_screen_shown: 0,
            dos: izarravm_dos::DosKernel::default(),
            rom,
            serial: uart::Uart16450::default(),
            serial2: uart::Uart16450::com2(),
            lpt: lpt::Lpt::default(),
            lpt2: lpt::Lpt::lpt2(),
            device_ports: DevicePorts::default(),
            pic: pic::Pic8259Pair::default(),
            pit: pit::Pit::default(),
            keyboard: keyboard::Keyboard8042::default(),
            speaker: speaker::Speaker::default(),
            pit_clocks: 0.0,
            dma: dma::DmaController::default(),
            fdc: fdc::Fdc::default(),
            opl: OplChip::default(),
            resampler: Resampler::new(OPL_NATIVE_HZ, DAC_HZ),
            opl_micros: 0.0,
            dsp: SbDsp::default(),
            // Placeholder; sync_dsp_resampler rebuilds this for the live rate on
            // first use, so the value here never reaches the DAC as-is.
            dsp_resampler: Resampler::new(OPL_NATIVE_HZ, DAC_HZ),
            dsp_rate_hz: 0,
            dsp_micros: 0.0,
            dsp_sample_phase: 0.0,
            mixer,
            wss,
            // Placeholder; sync_wss_resampler rebuilds this for the live rate on
            // first use, so the value here never reaches the DAC as-is.
            wss_resampler: Resampler::new(OPL_NATIVE_HZ, DAC_HZ),
            wss_rate_hz: 0,
            wss_sample_phase: 0.0,
            wss_base,
            wss_irq,
            wss_dma,
            wss_enabled,
            margo_ns: 0.0,
            vga_dots: 0.0,
            trace: BusTrace::default(),
            elapsed_clocks: 0,
            io_stall_clocks: 0,
            program_frames: Vec::new(),
            floppy: None,
            floppy_accesses: 0,
            c_accesses: 0,
            ide: ide::IdeChannel::new(),
            ata: None,
            fat32_c: None,
            cd_accesses: 0,
            cd_audio_frac: 0.0,
            rtc: rtc::Rtc::new(),
            rtc_seconds: 0.0,
            fast_post: true,
            booter_inert: false,
            mouse: MouseState::default(),
            unittester: unittester::UnitTester::default(),
            test_snapshot_path: None,
            xms: xms::XmsState::new(memory_mib),
        };
        // The Margo LFB aperture is decoded before RAM, so system memory must
        // stay below it. Validated config caps memory far under this bound.
        debug_assert!(
            machine.memory.len() as u64 <= u64::from(MARGO_LFB_BASE),
            "system RAM overlaps the Margo LFB aperture at 0xE0000000"
        );
        Ok(machine)
    }

    pub fn new(profile: MachineProfile, rom: impl AsRef<[u8]>) -> Result<Self, MachineError> {
        let rom = rom.as_ref();
        if rom.len() != BIOS_ROM_SIZE {
            return Err(MachineError::InvalidRomSize(rom.len()));
        }

        let mut machine = Self::base(profile, Cpu386::default(), rom.to_vec())?;
        install_boot_bios_stubs(&mut machine.memory)?;
        Ok(machine)
    }

    /// Control the cosmetic POST pacing the BIOS reads at port 0xE2. The default
    /// is fast (true): the ROM skips the ~8 s RAM count-up and the chime so
    /// headless runs and tests stay inside their cycle budgets. Pass false from
    /// the GUI to keep the full power-on screen and timing.
    pub fn set_fast_post(&mut self, fast: bool) {
        self.fast_post = fast;
    }

    /// Enter or leave booter-inert mode. When set, the Toka-DOS HLE and IEMM stop
    /// intercepting the DOS/memory-manager interrupts (0x20/0x21/0x25/0x26/0x28/
    /// 0x29/0x2F/0x66), so a self-booting disk's own handlers run through the IVT;
    /// the BIOS services stay intercepted. The booter track sets this; nothing
    /// auto-detects a booter yet.
    pub fn set_booter_inert(&mut self, inert: bool) {
        self.booter_inert = inert;
    }

    /// Whether booter-inert mode is active.
    pub fn booter_inert(&self) -> bool {
        self.booter_inert
    }

    /// Whether the PC speaker was ever enabled (port 0x61 bit 1 driven high). The
    /// power-on chime sets this during POST, so a headless run can assert the
    /// speaker was exercised without draining the audio ring.
    pub fn speaker_ever_enabled(&self) -> bool {
        self.speaker.ever_enabled()
    }

    /// Mount a raw floppy image into drive A:. The geometry is derived from the
    /// image length; an unrecognized size returns an error and leaves any
    /// previously mounted image in place.
    pub fn mount_floppy(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        self.floppy = Some(floppy::Floppy::from_image(bytes)?);
        self.set_equipment_floppy(true);
        // Tell the FDC media is present so SENSE DRIVE STATUS reports the drive
        // ready and a DIR read latches the disk-change line.
        self.fdc.set_media_present(true);
        Ok(())
    }

    /// Track drive A: in the BDA equipment word (0040:0010) that INT 11h returns. Bit 0 is
    /// the floppy-installed flag and bits 7-6 the drive count minus one; with one drive
    /// modeled, present means bit 0 set with bits 7-6 clear, absent means both cleared.
    fn set_equipment_floppy(&mut self, present: bool) {
        let mut word = self.memory.read_u16(0x410).unwrap_or(BIOS_EQUIPMENT_WORD);
        if present {
            word = (word & !0x00C0) | 0x0001;
        } else {
            word &= !0x00C1;
        }
        let _ = self.memory.write_u16(0x410, word);
    }

    /// Eject the A: floppy, returning its current image bytes (including any
    /// in-session writes) so the caller can flush them back to disk. Returns
    /// None when the drive is empty.
    pub fn eject_floppy(&mut self) -> Option<Vec<u8>> {
        let bytes = self.floppy.take().map(|f| f.bytes().to_vec());
        self.set_equipment_floppy(false);
        self.fdc.set_media_present(false);
        bytes
    }

    /// Whether the mounted A: floppy took a guest write this session. The host
    /// flushes the image back to its source IMG only when this is true, so an
    /// unwritten disk is ejected without rewriting the file. False when the drive
    /// is empty.
    pub fn floppy_dirty(&self) -> bool {
        self.floppy.as_ref().is_some_and(|f| f.dirty)
    }

    /// Monotonic access counts for drives A: (floppy) and C: (host). The GUI
    /// samples these per frame and flashes a drive LED when one advances.
    pub fn drive_access_counts(&self) -> (u64, u64) {
        (self.floppy_accesses, self.c_accesses)
    }

    /// Monotonic CD-ROM access count. The GUI samples this to flash the optical
    /// drive's access LED; it advances on every data read the ATAPI device serves.
    pub fn cd_access_count(&self) -> u64 {
        self.cd_accesses
    }

    /// Mount a CD image into the ATAPI drive. The image is a parsed `CdImage`
    /// built by the caller from an ISO or a CUE/BIN pair, so the machine stays
    /// agnostic to the host file layout.
    pub fn mount_cd(&mut self, image: CdImage) {
        self.ide.device_mut().insert(image);
    }

    /// Eject the CD, leaving the ATAPI drive empty.
    pub fn eject_cd(&mut self) {
        self.ide.device_mut().eject();
    }

    /// Mount a flat hard-disk image as the primary master (C:). The geometry is
    /// derived from the image length, padded up to a whole sector. INT 13h
    /// DL>=0x80 and the primary-channel ports then serve it. Seeds the BDA fixed-
    /// disk count to 1 so a guest reading 0040:0075 sees the drive.
    pub fn mount_hdd(&mut self, bytes: Vec<u8>) {
        self.ata = Some(ata::AtaDisk::new(bytes));
        let _ = self.memory.write_u8(0x475, 1); // BDA fixed-disk count
    }

    /// Mount a synthesized FAT32 volume as drive C: for the DOS absolute-disk
    /// interface. INT 25h reads its sectors; INT 26h writes are write-protected
    /// (the volume is read-only). Build one with `build_fat32`.
    pub fn mount_fat32(&mut self, volume: Fat32Volume) {
        self.fat32_c = Some(volume);
    }

    /// Eject the hard disk, returning its current image bytes (including any
    /// in-session writes) so the caller can flush them back. None when no disk is
    /// mounted. Clears the BDA fixed-disk count.
    pub fn eject_hdd(&mut self) -> Option<Vec<u8>> {
        let bytes = self.ata.take().map(|d| d.bytes().to_vec());
        let _ = self.memory.write_u8(0x475, 0);
        bytes
    }

    /// Whether the mounted hard disk took a guest write this session, so the host
    /// flushes the image back only when it changed. False when no disk is mounted.
    pub fn hdd_dirty(&self) -> bool {
        self.ata.as_ref().is_some_and(|d| d.dirty)
    }

    /// Whether a disc is currently mounted in the ATAPI drive.
    pub fn cd_loaded(&self) -> bool {
        self.ide.device().is_loaded()
    }

    /// Seed the RTC clock from host-provided local time. `weekday` is 1..=7 with
    /// 1 = Sunday. Call this once at startup; the clock self-advances on the
    /// machine clock afterward.
    #[allow(clippy::too_many_arguments)]
    pub fn seed_rtc(
        &mut self,
        year: u16,
        month: u8,
        day: u8,
        weekday: u8,
        hour: u8,
        minute: u8,
        second: u8,
    ) {
        self.rtc
            .seed(year, month, day, weekday, hour, minute, second);
    }

    /// The full 64-byte CMOS image (clock registers plus NVRAM) for persisting
    /// to cmos.bin.
    pub fn cmos_bytes(&self) -> [u8; 64] {
        self.rtc.nvram()
    }

    /// Load a 64-byte CMOS image from a persisted cmos.bin, restoring NVRAM and
    /// the saved time. Returns false if the image had a bad NVRAM checksum (the
    /// bytes are kept and the checksum is repaired), so the host can log it.
    pub fn load_cmos(&mut self, bytes: &[u8; 64]) -> bool {
        self.rtc.load_nvram(bytes)
    }

    /// Whether the guest wrote a CMOS NVRAM byte since the last poll, clearing
    /// the flag. The host flushes cmos.bin when this returns true.
    pub fn take_cmos_dirty(&mut self) -> bool {
        self.rtc.take_nvram_dirty()
    }

    /// Whether the RTC clock has been seeded from the host.
    pub fn rtc_seeded(&self) -> bool {
        self.rtc.is_seeded()
    }

    /// Read one CMOS NVRAM byte by index (0x00..=0x3F).
    pub fn cmos_byte(&self, index: usize) -> u8 {
        self.rtc.nvram_byte(index)
    }

    /// Set one CMOS NVRAM byte by index and refresh the stored checksum, the way
    /// a host-side configuration change would. Out-of-range indices are ignored.
    pub fn set_cmos_byte(&mut self, index: usize, value: u8) {
        self.rtc.set_nvram(index, value);
        self.rtc.refresh_checksum();
    }

    pub fn new_boot_image(
        profile: MachineProfile,
        image: impl AsRef<[u8]>,
    ) -> Result<Self, MachineError> {
        let image = image.as_ref();
        if image.len() != BOOT_IMAGE_SIZE {
            return Err(MachineError::InvalidBootImageSize(image.len()));
        }

        // The BIOS service vectors return through the ROM IRET at offset 0xF000
        // (FF00:0000); supply it even on this synthetic boot ROM.
        let mut rom = vec![0u8; BIOS_ROM_SIZE];
        rom[0xF000] = 0xCF;
        let mut machine = Self::base(profile, boot_sector_cpu(), rom)?;

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
    /// Entry eflags has IF set, matching real DOS which hands control with
    /// interrupts enabled. The resident keyboard BIOS is installed (IVT[09h]/[16h]
    /// point at its ROM handlers, the PIC is programmed, and IRQ1 is unmasked), so
    /// a typed key flows 8042 -> IRQ1 -> INT 09h ISR -> BDA ring. IRQ0 (timer) is
    /// masked, so with no key injected no hardware interrupt fires.
    pub fn new_dos_program(profile: MachineProfile, image: &[u8]) -> Result<Self, MachineError> {
        let env_entries = sound_blaster_env_entries(&profile.sound_blaster);
        let mut rom = vec![0u8; BIOS_ROM_SIZE];
        let kb = izarravm_firmware::kbd_resident_bios();
        rom[..kb.len()].copy_from_slice(kb);
        // The BIOS service vectors return through the ROM IRET at offset 0xF000
        // (FF00:0000); supply it on this synthetic ROM. The resident keyboard
        // BIOS image is short and never reaches that offset.
        rom[0xF000] = 0xCF;
        let mut machine = Self::base(profile, Cpu386::default(), rom)?;
        install_boot_bios_stubs(&mut machine.memory)?;
        machine.install_keyboard_bios()?;

        let entry = izarravm_dos::load_program(image, &mut machine.memory, DOS_LOAD_SEGMENT)?;
        machine.apply_program_entry(entry);
        // Seed the Toka-DOS per-program state (memory arena, DTA). prog_top is the
        // top-of-memory paragraph the loader wrote to PSP:0x02.
        let prog_top = machine
            .memory
            .read_u16(usize::from(DOS_LOAD_SEGMENT) * 16 + 2)?;
        machine.dos.init_program(DOS_LOAD_SEGMENT, prog_top);
        // Seed the DOS environment segment (BLASTER=/SETSOUND=) and record it in
        // PSP:0x2C so auto-detecting games find the SB16. The entries are derived
        // from the host config above; the borrow is split so the kernel and memory
        // are reached as disjoint fields.
        let entries: Vec<(&str, &str)> = env_entries
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        {
            let Machine { dos, memory, .. } = &mut machine;
            dos.install_environment(memory, &entries)?;
        }
        Ok(machine)
    }

    /// Set the CPU to a loaded program's entry: CS:IP, SS:SP, DS, ES, and a
    /// real-mode eflags with IF set, matching real DOS which hands control with
    /// interrupts enabled so the keyboard ISR can run while a program polls.
    fn apply_program_entry(&mut self, entry: izarravm_dos::ProgramEntry) {
        let r = &mut self.cpu.registers;
        r.set_segment(SegmentIndex::Cs, SegmentRegister::real(entry.cs));
        r.set_segment(SegmentIndex::Ds, SegmentRegister::real(entry.ds));
        r.set_segment(SegmentIndex::Es, SegmentRegister::real(entry.es));
        r.set_segment(SegmentIndex::Ss, SegmentRegister::real(entry.ss));
        r.eip = u32::from(entry.ip);
        r.set_esp(u32::from(entry.sp));
        r.eflags = 0x0000_0202; // IF set: DOS programs start with interrupts on
    }

    /// Install the resident keyboard BIOS for the DOS machine: point IVT[09h] and
    /// IVT[16h] at the handlers in the BIOS ROM (mapped at F000:0000), clear the
    /// BDA ring, program the PIC, and unmask IRQ1. IF is set at program entry so
    /// the ISR can run while a program polls for input.
    fn install_keyboard_bios(&mut self) -> Result<(), MachineError> {
        let kb = izarravm_firmware::kbd_resident_bios();
        let seg = izarravm_firmware::KBD_RESIDENT_BIOS_SEG;
        let int09 = u16::from_le_bytes([kb[0], kb[1]]);
        let int16 = u16::from_le_bytes([kb[2], kb[3]]);
        self.memory.write_u16(0x09 * 4, int09)?;
        self.memory.write_u16(0x09 * 4 + 2, seg)?;
        self.memory.write_u16(0x16 * 4, int16)?;
        self.memory.write_u16(0x16 * 4 + 2, seg)?;
        // BDA keyboard ring: head = tail = ring start, shift flags = 0.
        self.memory.write_u16(0x41a, 0x1e)?;
        self.memory.write_u16(0x41c, 0x1e)?;
        self.memory.write_u8(0x417, 0)?;
        // Program the 8259 pair (master IRQ0..7 -> INT 08h..0Fh), then mask all
        // but IRQ1 on the master so an unhandled timer INT cannot fire.
        {
            let mut bus = self.make_bus();
            for (port, value) in [
                (0x20u16, 0x11u16),
                (0x21, 0x08),
                (0x21, 0x04),
                (0x21, 0x01),
                (0xa0, 0x11),
                (0xa1, 0x70),
                (0xa1, 0x02),
                (0xa1, 0x01),
                (0x21, 0xfd), // master IMR: unmask IRQ1 only
                (0xa1, 0xff), // slave IMR: all masked
            ] {
                bus.write_io(port, BusWidth::Byte, u32::from(value))?;
            }
        }
        Ok(())
    }

    pub fn profile(&self) -> &MachineProfile {
        &self.profile
    }

    /// The IRQ line the CT1745 mixer currently routes the DSP interrupt to
    /// (decoded from register `0x80`).
    pub fn sb_selected_irq(&self) -> u8 {
        self.mixer.selected_irq()
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

    /// Bytes captured by the LPT1 printer port (strobed prints, in order).
    pub fn lpt_output(&self) -> &[u8] {
        self.lpt.output()
    }

    /// The LPT1 capture decoded as text, the printer-side mirror of serial_text.
    pub fn lpt_text(&self) -> String {
        String::from_utf8_lossy(self.lpt_output()).into_owned()
    }

    /// Feed Set 1 scancodes to the keyboard controller (make on press, break on
    /// release). Requests IRQ1 immediately so a halted or idle CPU wakes to it.
    pub fn inject_key_scancodes(&mut self, codes: &[u8]) {
        self.keyboard.push_scancodes(codes);
        if self.keyboard.take_irq() {
            self.pic.request(1);
        }
    }

    /// Feed a host mouse delta and button mask to the PS/2 aux device. `dx`/`dy`
    /// are host pixels (y down positive); `buttons` is bit0 left, bit1 right,
    /// bit2 middle. The aux device queues a movement packet and, when data
    /// reporting is enabled, this requests IRQ12 so a guest ISR runs. The same
    /// delta drives the INT 33h cursor and mickey counters so the HLE driver
    /// tracks the pointer even when no guest ISR consumes the hardware packets.
    pub fn inject_mouse(&mut self, dx: i32, dy: i32, buttons: u8) {
        if self.keyboard.inject_mouse(dx, dy, buttons) {
            self.pic.request(12);
        }
        self.mouse.apply_motion(dx, dy, buttons);
    }

    /// Set the absolute INT 33h cursor position directly, for the GUI's
    /// absolute-pointer mode: the host maps the pointer's position over the screen
    /// straight to the guest cursor, so there is no relative drift and nothing to
    /// confine. `x` is 0..639, `y` is 0..199; both clamp to the active range.
    /// Buttons use the same bit layout as `inject_mouse` (bit0 left, bit1 right,
    /// bit2 middle). The BIOS setup/boot menus read this through INT 33h AX=0003h.
    pub fn set_mouse_absolute(&mut self, x: i32, y: i32, buttons: u8) {
        self.mouse.x = x.clamp(self.mouse.min_x, self.mouse.max_x);
        self.mouse.y = y.clamp(self.mouse.min_y, self.mouse.max_y);
        self.mouse.buttons = buttons;
    }

    #[cfg(test)]
    fn read_io_port_u8(&mut self, port: u16) -> u8 {
        let mut bus = self.make_bus();
        bus.read_io(port, BusWidth::Byte).unwrap_or(0) as u8
    }

    #[cfg(test)]
    fn irq1_pending(&self) -> bool {
        self.pic.irr_bit(1)
    }

    #[cfg(test)]
    fn irq12_pending(&self) -> bool {
        self.pic.irr_bit(12)
    }

    #[cfg(test)]
    fn memory_read_u16_for_test(&self, linear: usize) -> u16 {
        self.memory.read_u16(linear).unwrap_or(0)
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

    fn make_bus(&mut self) -> MachineBus<'_> {
        MachineBus {
            memory: &mut self.memory,
            video: &mut self.video,
            margo: &mut self.margo,
            rom: &self.rom,
            serial: &mut self.serial,
            serial2: &mut self.serial2,
            lpt: &mut self.lpt,
            lpt2: &mut self.lpt2,
            device_ports: &mut self.device_ports,
            pic: &mut self.pic,
            pit: &mut self.pit,
            keyboard: &mut self.keyboard,
            speaker: &mut self.speaker,
            rtc: &mut self.rtc,
            dma: &mut self.dma,
            fdc: &mut self.fdc,
            floppy: &mut self.floppy,
            opl: &mut self.opl,
            dsp: &mut self.dsp,
            mixer: &mut self.mixer,
            wss: &mut self.wss,
            wss_base: self.wss_base,
            wss_enabled: self.wss_enabled,
            ide: &mut self.ide,
            ata: &mut self.ata,
            trace: &mut self.trace,
            pending_soft_int: &mut self.pending_soft_int,
            active_mode: self.active_mode,
            pending_mode: &mut self.pending_mode,
            fast_post: self.fast_post,
            booter_inert: self.booter_inert,
            pending_toka_service: &mut self.pending_toka_service,
            toka_service_status: self.toka_service_status,
            unittester: &mut self.unittester,
            wait_states: self.profile.wait_states,
            io_touched: &mut self.io_touched,
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
            VideoMode::Mode13h | VideoMode::Planar | VideoMode::ModeX | VideoMode::Cga
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
        let ah = (ax >> 8) as u8;
        let al = ax as u8;
        let bx = self.cpu.registers.ebx() as u16;
        let bh = (bx >> 8) as u8;
        let bl = bx as u8;
        if ah == 0x00 {
            match al {
                // The 16-color planar modes this slice implements.
                0x0D | 0x0E | 0x10 | 0x12 => {
                    self.set_vga_mode(al); // clears the Margo latch internally
                    let cols = if al == 0x0D { 40 } else { 80 };
                    self.set_bda_video_mode(al, cols, 25);
                    return;
                }
                // Chained mode 13h.
                0x13 => {
                    self.video.set_mode13h();
                    self.margo_active = false;
                    self.set_bda_video_mode(0x13, 40, 25);
                    return;
                }
                // CGA graphics: 04h/05h are 320x200x4, 06h is 640x200x2. The B800
                // framebuffer renders through the CGA personality (set_cga_mode).
                0x04..=0x06 => {
                    self.video.set_cga_mode(al);
                    self.margo_active = false;
                    let cols = if al == 0x06 { 80 } else { 40 };
                    self.set_bda_video_mode(al, cols, 25);
                    return;
                }
                // The 80x25 color text family (2/3), monochrome text (7), and the
                // 40x25 variants (0/1) map to the single text personality.
                0x00..=0x03 | 0x07 => {
                    self.video.set_text_mode();
                    self.margo_active = false;
                    let cols = if al <= 0x01 { 40 } else { 80 };
                    self.set_bda_video_mode(al, cols, 25);
                    // A mode set clears the screen and homes the BDA cursor, so
                    // teletyped output starts at the top left.
                    let _ = self.memory.write_u16(0x450, 0);
                    return;
                }
                _ => {}
            }
        }
        if ah == 0x05 {
            // INT 10h AH=05h SELECT ACTIVE DISPLAY PAGE (RBIL INTERRUP.A:2162).
            // AL is the page number. The 80x25 color text page stride is 4096
            // bytes (0x1000); the CRTC start address is a word/cell address in
            // mode 03h's word mode, so page N sits at cell N*2048 and eight pages
            // fill the 32 KB aperture. Routed through set_start_address so the
            // change latches at the next vretrace (no mid-frame tear), matching
            // what the BIOS writes to CRTC 0C/0Dh. Page 0 is the default.
            let page = u32::from(al);
            self.video
                .set_start_address(page * (VGA_TEXT_PAGE_STRIDE / 2) as u32);
            return;
        }
        if ah == 0x0b {
            // BH=0: BL is the border/overscan color (Attribute register 11h). BH=1
            // is the CGA palette select, a rarely-used CGA-compat path; deferred.
            if bh == 0x00 {
                self.video.set_overscan(bl);
            }
            return;
        }
        if ah == 0x0c {
            self.int10_write_pixel(al);
            return;
        }
        if ah == 0x0d {
            self.int10_read_pixel();
            return;
        }
        if ah == 0x10 {
            self.handle_int10_palette(al);
            return;
        }
        if ah == 0x11 {
            self.handle_int10_font(al);
            return;
        }
        if ah == 0x13 {
            self.int10_write_string();
            return;
        }
        if ah == 0x1c {
            self.int10_save_restore_state(al);
            return;
        }
        if matches!(
            ah,
            0x01 | 0x02 | 0x03 | 0x06 | 0x07 | 0x08 | 0x09 | 0x0A | 0x0E
        ) {
            self.handle_int10_text(ah);
            return;
        }
        if ah == 0x0f {
            let mode = self.read_physical_u8(0x449);
            let cols = self.read_guest_word(0x44a);
            let eax = (self.cpu.registers.eax() & !0xFFFF)
                | (u32::from(cols & 0xff) << 8)
                | u32::from(mode);
            self.cpu.registers.set_eax(eax);
            // BH = active page 0; leave the rest of EBX intact.
            let ebx = self.cpu.registers.ebx() & !0xFF00;
            self.cpu.registers.set_ebx(ebx);
            return;
        }
        if ah == 0x1a {
            // AH=1Ah display combination code. AL=00h reads, AL=01h writes (the write
            // is cosmetic here). Report a VGA with an analog colour monitor: AL=1Ah
            // marks the function supported, BL=08h is the active display code.
            self.set_eax_al(0x1A);
            if al == 0x00 {
                self.set_bx(0x0008);
            }
            return;
        }
        if ah == 0x1b {
            // AH=1Bh functionality/state information (VGA). Fills the 64-byte block at
            // ES:DI and returns AL=1Bh so callers detect a VGA BIOS.
            self.int10_state_info();
            return;
        }
        if ah == 0x4f {
            self.handle_vbe(al);
        }
    }

    /// INT 10h AH=1Bh. Writes the 64-byte video state-information block at ES:DI with the
    /// live mode, geometry, and display-combination fields, plus a static functionality
    /// table pointer. ponytail: only the commonly-read fields are populated and the static
    /// table is pointed at the video BIOS segment rather than a fully built table; the
    /// VGA-present check that programs run only tests AL == 0x1B.
    fn int10_state_info(&mut self) {
        let es = self.cpu.registers.segment(SegmentIndex::Es).base;
        let di = self.cpu.registers.edi() as u16;
        let addr = es.wrapping_add(u32::from(di));
        let mode = self.read_physical_u8(0x449);
        let cols = self.read_guest_word(0x44a);
        let page = self.read_physical_u8(0x462);
        let rows_minus_1 = self.read_physical_u8(0x484);
        let mut block = [0u8; 64];
        block[0..4].copy_from_slice(&0xC000_0000u32.to_le_bytes()); // C000:0000 func table
        block[4] = mode;
        block[5..7].copy_from_slice(&cols.to_le_bytes());
        block[0x1D] = page;
        block[0x1E..0x20].copy_from_slice(&0x03D4u16.to_le_bytes()); // CRTC base port
        block[0x22] = rows_minus_1.wrapping_add(1); // rows on screen
        block[0x25] = 0x08; // active display combination code (VGA colour)
        block[0x29] = 8; // pages
        block[0x2A] = 0x03; // 480 scan lines (VGA)
        self.write_guest_block(addr, &block);
        self.set_eax_al(0x1B);
    }

    /// Record the current video mode in the BDA so apps that read it directly
    /// (and INT 10h AH=0Fh) see a sane state. Columns and rows are the text-cell
    /// geometry the BIOS publishes for the mode.
    fn set_bda_video_mode(&mut self, mode: u8, columns: u16, rows: u8) {
        let _ = self.memory.write_u8(0x449, mode);
        let _ = self.memory.write_u16(0x44a, columns);
        let _ = self.memory.write_u8(0x484, rows.saturating_sub(1));
        let _ = self.memory.write_u16(0x463, 0x03d4); // VGA CRTC base port
    }

    /// INT 10h AH=0Ch WRITE GRAPHICS PIXEL. AL = colour (bit 7 set XORs into the
    /// existing pixel), CX = column, DX = row. In mode 13h the pixel is the byte at
    /// `row*320 + col` in the A0000 framebuffer; the chain-4 datapath routes that
    /// linear offset to the right plane the same way the CPU bus write does.
    /// ponytail: only the linear 320x200 mode 13h is handled; the 16-colour planar
    /// modes (0Dh/0Eh/10h/12h) need a read-modify-write through the bit-mask and
    /// map-mask, which is not wired here, so a pixel write in those modes is ignored.
    /// In text mode the call does nothing, matching a real BIOS.
    fn int10_write_pixel(&mut self, al: u8) {
        if self.video.active_mode() != VideoMode::Mode13h {
            return;
        }
        let col = self.cpu.registers.ecx() as u16;
        let row = self.cpu.registers.edx() as u16;
        let offset = usize::from(row) * 320 + usize::from(col);
        if offset >= 320 * 200 {
            return;
        }
        // Mode 13h is a 256-color mode: AL is the full 8-bit pixel value, bit 7
        // included. The bit-7 XOR-onto-screen convention applies only to the
        // 16-color planar modes, which this handler does not service.
        self.video.cpu_write_chain4(offset, al);
    }

    /// INT 10h AH=0Dh READ GRAPHICS PIXEL. CX = column, DX = row; returns AL = the
    /// pixel colour at `row*320 + col`. Only mode 13h is read back (see
    /// int10_write_pixel); other modes return AL = 0.
    fn int10_read_pixel(&mut self) {
        let color = if self.video.active_mode() == VideoMode::Mode13h {
            let col = self.cpu.registers.ecx() as u16;
            let row = self.cpu.registers.edx() as u16;
            let offset = usize::from(row) * 320 + usize::from(col);
            if offset < 320 * 200 {
                self.video.cpu_read_chain4(offset)
            } else {
                0
            }
        } else {
            0
        };
        self.set_eax_al(color);
    }

    /// INT 10h AH=13h WRITE STRING. AL = write mode (bit 0 advance cursor, bit 1
    /// the source carries interleaved attribute bytes), BH = page (ignored, page 0
    /// only), BL = attribute when bit 1 is clear, CX = character count, DH/DL =
    /// start row/col, ES:BP = the string. Characters land in the page-0 text buffer
    /// with their attribute, advancing the column and wrapping rows; the cursor is
    /// left at the end only when AL bit 0 is set.
    fn int10_write_string(&mut self) {
        let al = self.cpu.registers.eax() as u8;
        let bx = self.cpu.registers.ebx() as u16;
        let bl = bx as u8;
        let count = self.cpu.registers.ecx() as u16;
        let dx = self.cpu.registers.edx() as u16;
        let mut row = usize::from((dx >> 8) as u8);
        let mut col = usize::from(dx as u8);
        let es = self.cpu.registers.segment(SegmentIndex::Es).base;
        let bp = self.cpu.registers.ebp() as u16;
        let mut src = es.wrapping_add(u32::from(bp));
        let with_attr = al & 0x02 != 0;
        for _ in 0..count {
            let ch = self.read_physical_u8(src);
            src += 1;
            let attr = if with_attr {
                let a = self.read_physical_u8(src);
                src += 1;
                a
            } else {
                bl
            };
            // Control characters move the cursor without placing a glyph, the way
            // the BIOS write-string handles CR/LF/BS/BEL.
            match ch {
                b'\r' => col = 0,
                b'\n' => row += 1,
                0x08 => col = col.saturating_sub(1),
                0x07 => {}
                _ => {
                    if row < 25 && col < 80 {
                        let off = (row * 80 + col) * 2;
                        let _ = self.video.write_u8(off, ch);
                        let _ = self.video.write_u8(off + 1, attr);
                    }
                    col += 1;
                    if col >= 80 {
                        col = 0;
                        row += 1;
                    }
                }
            }
            while row >= 25 {
                self.scroll_text_up();
                row -= 1;
            }
        }
        // AL bit 0: leave the cursor at the end of the string; otherwise the caller
        // keeps its prior cursor (the BDA cursor is untouched).
        if al & 0x01 != 0 {
            let row = row.min(24) as u16;
            let col = col.min(79) as u16;
            let _ = self.memory.write_u16(0x450, (row << 8) | col);
            self.video.set_cursor_offset(row * 80 + col);
        }
    }

    /// INT 10h AH=1Ch SAVE/RESTORE VIDEO STATE. AL=00 returns the buffer size in
    /// 64-byte blocks (BX), AL=01 saves the modeled state into ES:BX, AL=02 restores
    /// it. CX is the requested-state bitmap (bit 0 hardware, bit 1 BDA, bit 2 DAC).
    /// Saves and restores the full BDA video state (0040:0049-0040:00A8, 96 bytes,
    /// two 64-byte blocks) per RBIL. ponytail: the hardware-register and DAC-palette
    /// state (CX bits 0 and 2) are not captured, so a save/restore round-trips the
    /// BDA block, not the full VGA hardware. AL is set to 0x1C so callers detect the
    /// service.
    fn int10_save_restore_state(&mut self, al: u8) {
        const BDA_VIDEO_START: u32 = 0x449;
        const BDA_VIDEO_LEN: usize = 0x4a8 - 0x449 + 1; // 96 bytes, two 64-byte blocks
        match al {
            0x00 => {
                self.set_bx(2); // two 64-byte blocks hold the 96-byte BDA video state
                self.set_eax_al(0x1c);
                self.set_int_frame_carry(false);
            }
            0x01 => {
                let es = self.cpu.registers.segment(SegmentIndex::Es).base;
                let bx = self.cpu.registers.ebx() as u16;
                let dst = es.wrapping_add(u32::from(bx));
                let block = self.read_guest_block(BDA_VIDEO_START, BDA_VIDEO_LEN);
                self.write_guest_block(dst, &block);
                self.set_eax_al(0x1c);
                self.set_int_frame_carry(false);
            }
            0x02 => {
                let es = self.cpu.registers.segment(SegmentIndex::Es).base;
                let bx = self.cpu.registers.ebx() as u16;
                let from = es.wrapping_add(u32::from(bx));
                let block = self.read_guest_block(from, BDA_VIDEO_LEN);
                self.write_guest_block(BDA_VIDEO_START, &block);
                self.set_eax_al(0x1c);
                self.set_int_frame_carry(false);
            }
            _ => self.set_int_frame_carry(true),
        }
    }

    /// INT 10h text-mode output and cursor services. Operates on the same VGA text
    /// framebuffer and BDA cursor (0040:0050) the teletype helper uses. Page
    /// arguments are ignored: this BIOS renders page 0 only.
    fn handle_int10_text(&mut self, ah: u8) {
        let ax = self.cpu.registers.eax() as u16;
        let al = ax as u8;
        let bx = self.cpu.registers.ebx() as u16;
        let bl = bx as u8;
        let cx = self.cpu.registers.ecx() as u16;
        let dx = self.cpu.registers.edx() as u16;
        let dl = dx as u8;
        let dh = (dx >> 8) as u8;
        match ah {
            // AH=01h set cursor shape: store CX in the BDA cursor-type word.
            0x01 => {
                let _ = self.memory.write_u16(0x460, cx);
            }
            // AH=02h set cursor position: DH=row, DL=col.
            0x02 => {
                let _ = self
                    .memory
                    .write_u16(0x450, (u16::from(dh) << 8) | u16::from(dl));
                self.video
                    .set_cursor_offset(u16::from(dh) * 80 + u16::from(dl));
            }
            // AH=03h get cursor position and shape.
            0x03 => {
                let pos = self.read_guest_word(0x450);
                let edx = (self.cpu.registers.edx() & !0xFFFF) | u32::from(pos);
                self.cpu.registers.set_edx(edx);
                let shape = self.read_guest_word(0x460);
                let shape = if shape == 0 { 0x0607 } else { shape };
                let ecx = (self.cpu.registers.ecx() & !0xFFFF) | u32::from(shape);
                self.cpu.registers.set_ecx(ecx);
            }
            // AH=06h/07h scroll the window up/down. AL=0 blanks it.
            0x06 | 0x07 => self.scroll_window(ah == 0x06, al, bx >> 8, cx, dx),
            // AH=08h read char+attr at the cursor.
            0x08 => {
                let pos = self.read_guest_word(0x450);
                let off = (usize::from(pos >> 8) * 80 + usize::from(pos & 0xff)) * 2;
                let ch = self.video.read_u8(off).unwrap_or(b' ');
                let at = self.video.read_u8(off + 1).unwrap_or(0x07);
                let eax =
                    (self.cpu.registers.eax() & !0xFFFF) | (u32::from(at) << 8) | u32::from(ch);
                self.cpu.registers.set_eax(eax);
            }
            // AH=09h write char+attr, AH=0Ah write char only, CX times, no advance.
            0x09 | 0x0A => {
                let pos = self.read_guest_word(0x450);
                let base = (usize::from(pos >> 8) * 80 + usize::from(pos & 0xff)) * 2;
                for i in 0..usize::from(cx) {
                    let off = base + i * 2;
                    let _ = self.video.write_u8(off, al);
                    if ah == 0x09 {
                        let _ = self.video.write_u8(off + 1, bl);
                    }
                }
            }
            // AH=0Eh teletype.
            0x0E => self.teletype_char(al),
            _ => {}
        }
    }

    /// Scroll a text window. `up` selects direction; `lines`==0 blanks the whole
    /// window. `attr` fills the vacated rows; `cx`=top-left (CH row, CL col),
    /// `dx`=bottom-right (DH row, DL col). Clamped to the 80x25 screen.
    fn scroll_window(&mut self, up: bool, lines: u8, attr: u16, cx: u16, dx: u16) {
        let attr = attr as u8;
        let top = usize::from((cx >> 8) as u8).min(24);
        let left = usize::from(cx as u8).min(79);
        let bottom = usize::from((dx >> 8) as u8).min(24).max(top);
        let right = usize::from(dx as u8).min(79).max(left);
        let height = bottom - top + 1;
        let n = if lines == 0 {
            height
        } else {
            usize::from(lines)
        };
        if n >= height {
            for row in top..=bottom {
                self.blank_text_row(row, left, right, attr);
            }
            return;
        }
        if up {
            for row in top..=(bottom - n) {
                self.copy_text_row(row + n, row, left, right, attr);
            }
            for row in (bottom - n + 1)..=bottom {
                self.blank_text_row(row, left, right, attr);
            }
        } else {
            for row in ((top + n)..=bottom).rev() {
                self.copy_text_row(row - n, row, left, right, attr);
            }
            for row in top..(top + n) {
                self.blank_text_row(row, left, right, attr);
            }
        }
    }

    /// Copy a span of text cells from `src_row` to `dst_row` (inclusive columns).
    fn copy_text_row(
        &mut self,
        src_row: usize,
        dst_row: usize,
        left: usize,
        right: usize,
        attr: u8,
    ) {
        for col in left..=right {
            let src = (src_row * 80 + col) * 2;
            let dst = (dst_row * 80 + col) * 2;
            let b0 = self.video.read_u8(src).unwrap_or(b' ');
            let b1 = self.video.read_u8(src + 1).unwrap_or(attr);
            let _ = self.video.write_u8(dst, b0);
            let _ = self.video.write_u8(dst + 1, b1);
        }
    }

    /// Blank a span of text cells to spaces with `attr` (inclusive columns).
    fn blank_text_row(&mut self, row: usize, left: usize, right: usize, attr: u8) {
        for col in left..=right {
            let off = (row * 80 + col) * 2;
            let _ = self.video.write_u8(off, b' ');
            let _ = self.video.write_u8(off + 1, attr);
        }
    }

    /// Service INT 11h (GET EQUIPMENT LIST). Returns the BDA equipment word in AX,
    /// the way a real BIOS reads it from 0040:0010. The high word of EAX is left
    /// alone: callers that test the 386 EAX bits clear it themselves before the
    /// call, per RBIL. No flags change (the IRET restores the caller's FLAGS).
    fn handle_int11(&mut self) {
        let word = self.memory.read_u16(0x410).unwrap_or(BIOS_EQUIPMENT_WORD);
        let eax = (self.cpu.registers.eax() & !0xFFFF) | u32::from(word);
        self.cpu.registers.set_eax(eax);
    }

    /// Service INT 12h (GET MEMORY SIZE). Returns the conventional memory size in
    /// KiB in AX, read from the BDA word at 0040:0013 the way a real BIOS does. No
    /// flags change (the IRET restores the caller's FLAGS).
    fn handle_int12(&mut self) {
        let kib = self.memory.read_u16(0x413).unwrap_or(BIOS_BASE_MEMORY_KIB);
        let eax = (self.cpu.registers.eax() & !0xFFFF) | u32::from(kib);
        self.cpu.registers.set_eax(eax);
    }

    /// Service INT 14h (SERIAL) over the COM1 UART. DX selects the port; only COM1
    /// (DX=0) is wired. AH=00h initializes from the AL parameter byte, AH=01h sends
    /// AL, AH=02h receives into AL, AH=03h reads status. AH returns the line-status
    /// byte and AL the modem-status byte, the way the BIOS reports the 16450
    /// registers.
    fn handle_int14(&mut self) {
        const COM1: u16 = 0x03f8;
        let ax = self.cpu.registers.eax() as u16;
        let ah = (ax >> 8) as u8;
        let al = ax as u8;
        if self.cpu.registers.edx() as u16 != 0 {
            self.set_eax_ah(0x80); // bit7 timeout: no such serial port
            return;
        }
        match ah {
            0x00 => {
                self.uart_init(al);
                let lsr = self.serial.read_port(COM1 + 5).unwrap_or(0);
                let msr = self.serial.read_port(COM1 + 6).unwrap_or(0);
                self.set_eax_ah(lsr);
                self.set_eax_al(msr);
            }
            0x01 => {
                // THRE is always set (instant transmit), so the send never times out.
                self.serial.write_port(COM1, al);
                let lsr = self.serial.read_port(COM1 + 5).unwrap_or(0);
                self.set_eax_ah(lsr & 0x7f); // bit7 clear = sent
            }
            0x02 => {
                let lsr = self.serial.read_port(COM1 + 5).unwrap_or(0);
                if lsr & 0x01 != 0 {
                    let byte = self.serial.read_port(COM1).unwrap_or(0);
                    self.set_eax_al(byte);
                    self.set_eax_ah(lsr & 0x1e); // line status, data-ready/timeout clear
                } else {
                    // No byte available, and no serial input source is wired, so the
                    // honest result is a receive timeout.
                    self.set_eax_ah(0x80);
                }
            }
            0x03 => {
                let lsr = self.serial.read_port(COM1 + 5).unwrap_or(0);
                let msr = self.serial.read_port(COM1 + 6).unwrap_or(0);
                self.set_eax_ah(lsr);
                self.set_eax_al(msr);
            }
            _ => self.set_eax_ah(0x80),
        }
    }

    /// Program the COM1 UART from an INT 14h AH=00h parameter byte: bits 7-5 baud
    /// rate, 4-3 parity, 2 stop bits, 1-0 word length. The divisor is stored for
    /// fidelity but does not gate transmit timing.
    fn uart_init(&mut self, params: u8) {
        const COM1: u16 = 0x03f8;
        let divisor: u16 = match params >> 5 {
            0 => 1047, // 110 baud at 1.8432 MHz
            1 => 768,  // 150
            2 => 384,  // 300
            3 => 192,  // 600
            4 => 96,   // 1200
            5 => 48,   // 2400
            6 => 24,   // 4800
            _ => 12,   // 9600
        };
        // Word length (bits 1-0) and stop bits (bit 2) sit in the same positions in
        // the LCR; add the parity bits from AL bits 4-3 (01 odd, 11 even).
        let mut lcr = params & 0x07;
        match (params >> 3) & 0x03 {
            0b01 => lcr |= 0x08,        // parity enable, odd
            0b11 => lcr |= 0x08 | 0x10, // parity enable, even
            _ => {}                     // no parity
        }
        self.serial.write_port(COM1 + 3, 0x80); // LCR DLAB=1
        self.serial.write_port(COM1, (divisor & 0xff) as u8); // DLL
        self.serial.write_port(COM1 + 1, (divisor >> 8) as u8); // DLM
        self.serial.write_port(COM1 + 3, lcr); // LCR, clears DLAB
    }

    /// Service INT 17h (PRINTER) over LPT1. DX selects the port; only LPT1 (DX=0)
    /// is wired. AH=00h prints AL, AH=01h initializes, AH=02h reads status. AH
    /// returns the BIOS printer-status byte.
    fn handle_int17(&mut self) {
        const LPT1: u16 = 0x0378;
        let ax = self.cpu.registers.eax() as u16;
        let ah = (ax >> 8) as u8;
        let al = ax as u8;
        if self.cpu.registers.edx() as u16 != 0 {
            self.set_eax_ah(0x01); // bit0 timeout: no such printer
            return;
        }
        if ah == 0x00 {
            // Latch the byte and pulse -Strobe so the LPT captures it.
            self.lpt.write_port(LPT1, al);
            let base = self.lpt.read_port(LPT1 + 2).unwrap_or(0) & 0x1e; // keep bits 1-4
            self.lpt.write_port(LPT1 + 2, base | 0x01); // assert -Strobe (edge captures)
            self.lpt.write_port(LPT1 + 2, base); // de-assert
        }
        // AH=01h initialize and AH=02h status are status-only on this always-ready
        // model, so every subfunction returns the current printer status.
        let status = self.int17_printer_status();
        self.set_eax_ah(status);
    }

    /// Translate the LPT1 status port into the INT 17h status byte: keep bits 7-3
    /// and flip -ACK (bit6) and -Error (bit3) so "acknowledge" and "I/O error" read
    /// in the BIOS sense. An always-ready printer yields 0x90 (not busy, selected).
    fn int17_printer_status(&self) -> u8 {
        let port = self.lpt.read_port(0x0379).unwrap_or(0);
        (port & 0xf8) ^ 0x48
    }

    /// Service the host side of INT 15h. AH=88h returns the extended memory size
    /// (KiB above 1 MiB) in AX with CF clear, the standard way a BIOS learns RAM
    /// size on a machine with no probing path. Capped at 0xFFFF KiB (64 MiB) to
    /// fit the 16-bit AX return; other subfunctions report CF set (unsupported).
    fn handle_int15(&mut self) {
        let ah = (self.cpu.registers.eax() as u16 >> 8) as u8;
        let al = self.cpu.registers.eax() as u8;
        match ah {
            // AH=88h extended memory size in KiB (existing behavior).
            0x88 => {
                let extended_kib = u32::from(self.profile.memory_mib.saturating_sub(1)) * 1024;
                let value = extended_kib.min(0xFFFF) as u16;
                let eax = (self.cpu.registers.eax() & !0xFFFF) | u32::from(value);
                self.cpu.registers.set_eax(eax);
                self.set_int_frame_carry(false);
            }
            // AH=86h WAIT: CX:DX microseconds. Convert to seconds and stall.
            0x86 => {
                let micros = (u64::from(self.cpu.registers.ecx() as u16) << 16)
                    | u64::from(self.cpu.registers.edx() as u16);
                self.stall_for(micros as f64 / 1_000_000.0);
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            // AH=87h block move: ES:SI -> GDT; copy CX words src->dst across 1 MB.
            0x87 => self.int15_block_move(),
            // AH=8Ah extended memory size in KiB as a 32-bit DX:AX (the >64 MB-capable
            // sibling of AH=88h, which saturates at 0xFFFF).
            0x8A => {
                let ext_kib = u32::from(self.profile.memory_mib).saturating_sub(1) * 1024;
                self.set_ax(ext_kib as u16);
                self.set_dx((ext_kib >> 16) as u16);
                self.set_int_frame_carry(false);
            }
            // AX=E801h/E820h/E881h memory-size and memory-map queries (AH=E8h group).
            0xE8 => match self.cpu.registers.eax() as u8 {
                0x01 => self.int15_e801(false),
                0x81 => self.int15_e801(true),
                0x20 => self.int15_e820(),
                _ => self.set_int_frame_carry(true),
            },
            // AH=24h A20 gate (later PS/2s). The 8042 output-port bit 1 is the
            // single A20 state, shared with the fast-A20 port 0x92. The address
            // space is already flat, so this tracks and reports state without
            // masking. AL selects: 00 disable, 01 enable, 02 status, 03 support.
            0x24 => match al {
                0x00 => {
                    self.keyboard.set_a20(false);
                    self.set_eax_ah(0x00);
                    self.set_int_frame_carry(false);
                }
                0x01 => {
                    self.keyboard.set_a20(true);
                    self.set_eax_ah(0x00);
                    self.set_int_frame_carry(false);
                }
                0x02 => {
                    self.set_eax_ah(0x00);
                    self.set_eax_al(u8::from(self.keyboard.a20_enabled()));
                    self.set_int_frame_carry(false);
                }
                0x03 => {
                    self.set_eax_ah(0x00);
                    // Bit 0 keyboard controller, bit 1 port 0x92: both supported.
                    self.set_bx(0x0003);
                    self.set_int_frame_carry(false);
                }
                // Undefined subfunction: report function-not-supported.
                _ => {
                    self.set_eax_ah(0x86);
                    self.set_int_frame_carry(true);
                }
            },
            // AH=90h device-wait / AH=91h device-post are OS hooks. With no OS hook
            // installed the BIOS returns "no wait performed" with CF clear, rather than
            // the unsupported-function carry the catch-all would set.
            0x90 | 0x91 => self.set_int_frame_carry(false),
            // AH=C0h get system-configuration table: ES:BX -> the table seeded at POST.
            0xC0 => {
                let seg = (BIOS_CONFIG_TABLE_ADDR >> 4) as u16;
                let off = (BIOS_CONFIG_TABLE_ADDR & 0xf) as u16;
                self.cpu
                    .registers
                    .set_segment(SegmentIndex::Es, SegmentRegister::real(seg));
                self.set_bx(off);
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            // AH=C1h get extended BIOS data area segment: ES = the EBDA segment.
            0xC1 => {
                self.cpu
                    .registers
                    .set_segment(SegmentIndex::Es, SegmentRegister::real(EBDA_SEGMENT));
                self.set_int_frame_carry(false);
            }
            // AH=C2h PS/2 pointing-device (mouse) BIOS interface. AL selects the
            // subfunction.
            0xC2 => self.int15_c2_pointing_device(al),
            _ => self.set_int_frame_carry(true),
        }
    }

    /// INT 15h AH=C2h PS/2 pointing-device interface (RBIL INTERRUP.C). Handles the
    /// query subset a guest probes the BIOS mouse with: enable/disable (C200), reset
    /// (C201), set sample rate (C202), set resolution (C203), get device type
    /// (C204), initialize (C205), and the extended-command group (C206). The aux
    /// device is the same standard PS/2 mouse INT 33h models, so the reset reports
    /// the self-test-passed/device-id bytes a real mouse returns. C207 (set the
    /// device handler) stores the ES:BX far pointer in the EBDA and returns success;
    /// the pointer is never called yet (see the C207 arm). C208/C209 (read/write the
    /// raw device port) report function-not-supported (AH=86h, CF set).
    fn int15_c2_pointing_device(&mut self, al: u8) {
        let bh = (self.cpu.registers.ebx() as u16 >> 8) as u8;
        match al {
            // C200 enable/disable (BH=0 disable, 1 enable). Accept either; the cursor
            // visibility is driven through INT 33h, so this only acknowledges.
            0x00 => {
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            // C201 reset: BH=0x00 (device id, a standard mouse), BL=0xAA (the
            // reset-complete/BAT-passed signature the device returns; drivers probe
            // for AAh here). Re-centre the modeled mouse like a reset.
            0x01 => {
                self.mouse = MouseState::default();
                let ebx = (self.cpu.registers.ebx() & !0xFFFF) | 0x00AA;
                self.cpu.registers.set_ebx(ebx);
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            // C202 set sample rate (BH=rate code 0-6) and C203 set resolution
            // (BH=0-3): no hardware rate/resolution is modeled, so accept and ignore.
            0x02 | 0x03 => {
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            // C204 get device type: BH=0x00 (a standard PS/2 mouse).
            0x04 => {
                let ebx = self.cpu.registers.ebx() & !0xFF00; // BH=0
                self.cpu.registers.set_ebx(ebx);
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            // C205 initialize (BH=packet size, 3 for a standard mouse): reset the
            // modeled state and acknowledge.
            0x05 => {
                self.mouse = MouseState::default();
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            // C206 extended commands: BH=00 return device status (3 bytes in BL/CL/DL),
            // BH=01/02 set scaling 1:1 / 2:1, BH=03 set resolution. The status bytes
            // describe a stream-mode, scaling-1:1, enabled mouse at the default
            // resolution and sample rate.
            0x06 => {
                if bh == 0x00 {
                    // Status byte 1 (BL): bit5 mouse enabled. Status byte 2 (CL):
                    // resolution code 2. Status byte 3 (DL): sample rate 100.
                    let ebx = (self.cpu.registers.ebx() & !0xFF) | 0x20;
                    self.cpu.registers.set_ebx(ebx);
                    let ecx = (self.cpu.registers.ecx() & !0xFF) | 0x02;
                    self.cpu.registers.set_ecx(ecx);
                    let edx = (self.cpu.registers.edx() & !0xFF) | 100;
                    self.cpu.registers.set_edx(edx);
                }
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            // C207 set device handler: store the ES:BX far pointer the guest is
            // installing into the EBDA (offset word then segment word) and report
            // success. ES=0:BX=0 deregisters. ponytail: the handler far-pointer is
            // stored but never called: invocation needs the outbound host->guest
            // far-call trampoline plus a mouse-packet source, and no producer is
            // wired, so this is deferred until a producer exists. C208/C209 (the
            // raw device-port read/write) stay unsupported for the same reason.
            0x07 => {
                // The far pointer's segment is the literal ES the guest passed (the
                // selector), not the derived physical base.
                let es = self.cpu.registers.segment(SegmentIndex::Es).selector;
                let bx = self.cpu.registers.ebx() as u16;
                let base = (u32::from(EBDA_SEGMENT) << 4) + EBDA_MOUSE_HANDLER_OFF;
                self.write_guest_block(base, &bx.to_le_bytes());
                self.write_guest_block(base + 2, &es.to_le_bytes());
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            // C208/C209 raw device-port read/write: no raw aux-port path is wired.
            // Report function-not-supported.
            _ => {
                self.set_eax_ah(0x86);
                self.set_int_frame_carry(true);
            }
        }
    }

    /// INT 15h AX=E801h (and the AX=E881h 32-bit variant). Reports extended memory in two
    /// pieces the way DOS extenders and HIMEM expect: the 1-16 MB range in KB (AX/CX,
    /// capped at 0x3C00 = 15 MB) and the memory above 16 MB in 64 KB blocks (BX/DX). E881h
    /// returns the same magnitudes in the full 32-bit registers.
    fn int15_e801(&mut self, wide: bool) {
        let ext_kib = u32::from(self.profile.memory_mib) * 1024;
        let ext_kib = ext_kib.saturating_sub(1024); // memory above the first 1 MB
        let below_16m = ext_kib.min(15 * 1024); // 1-16 MB range, max 0x3C00 KB
        let above_16m_blocks = ext_kib.saturating_sub(15 * 1024) / 64; // 64 KB blocks
        if wide {
            self.cpu.registers.set_eax(below_16m);
            self.cpu.registers.set_ebx(above_16m_blocks);
            self.cpu.registers.set_ecx(below_16m);
            self.cpu.registers.set_edx(above_16m_blocks);
        } else {
            self.set_ax(below_16m as u16);
            self.set_bx(above_16m_blocks as u16);
            self.set_cx(below_16m as u16);
            self.set_dx(above_16m_blocks as u16);
        }
        self.set_int_frame_carry(false);
    }

    /// The system memory map E820h enumerates: 640 KB of conventional RAM, the reserved
    /// video/ROM hole below 1 MB, and a single available region for everything above 1 MB.
    fn e820_regions(&self) -> Vec<(u64, u64, u32)> {
        let total = u64::from(self.profile.memory_mib) * 0x10_0000;
        let mut regions = vec![
            (0x0u64, 0x9_FC00u64, 1u32), // 639 KB conventional, available (below the EBDA)
            (0x9_FC00, 0x400, 2),        // 1 KB extended BIOS data area, reserved
            (0xA_0000, 0x6_0000, 2),     // video + ROM BIOS hole, reserved
        ];
        if total > 0x10_0000 {
            regions.push((0x10_0000, total - 0x10_0000, 1)); // extended RAM, available
        }
        regions
    }

    /// INT 15h AX=E820h. Walks the memory map one 20-byte descriptor per call: EDX must
    /// carry 'SMAP', EBX is the continuation index (0 to start), ES:DI is the buffer. Each
    /// call returns EAX='SMAP', ECX=20, the descriptor written, and EBX advanced to the
    /// next index or 0 once the last region has been returned.
    fn int15_e820(&mut self) {
        const SMAP: u32 = 0x534D_4150;
        if self.cpu.registers.edx() != SMAP || (self.cpu.registers.ecx() as u16) < 20 {
            self.set_int_frame_carry(true);
            return;
        }
        let regions = self.e820_regions();
        let index = self.cpu.registers.ebx() as usize;
        let Some(&(base, len, kind)) = regions.get(index) else {
            self.set_int_frame_carry(true);
            return;
        };
        let es = self.cpu.registers.segment(SegmentIndex::Es).base;
        let di = self.cpu.registers.edi() as u16;
        let addr = es.wrapping_add(u32::from(di));
        let mut desc = [0u8; 20];
        desc[0..8].copy_from_slice(&base.to_le_bytes());
        desc[8..16].copy_from_slice(&len.to_le_bytes());
        desc[16..20].copy_from_slice(&kind.to_le_bytes());
        self.write_guest_block(addr, &desc);
        self.cpu.registers.set_eax(SMAP);
        self.cpu.registers.set_ecx(20);
        let next = index + 1;
        let continuation = if next < regions.len() { next as u32 } else { 0 };
        self.cpu.registers.set_ebx(continuation);
        self.set_int_frame_carry(false);
    }

    /// INT 15h AH=87h. ES:SI points at a 48-byte GDT the caller built; the source
    /// descriptor is at +0x10 and the destination at +0x18. Each descriptor holds
    /// a 24-bit base across bytes 2,3,4 and the high 8 bits at byte 7. Copies CX
    /// words. This is the standard path HIMEM and DOS extenders use to reach
    /// extended memory from real mode.
    fn int15_block_move(&mut self) {
        let es = self.cpu.registers.segment(SegmentIndex::Es).base;
        let si = self.cpu.registers.esi() as u16;
        let gdt = es.wrapping_add(u32::from(si));
        let base_at = |s: &mut Self, desc: u32| -> u32 {
            u32::from(s.read_physical_u8(desc + 2))
                | (u32::from(s.read_physical_u8(desc + 3)) << 8)
                | (u32::from(s.read_physical_u8(desc + 4)) << 16)
                | (u32::from(s.read_physical_u8(desc + 7)) << 24)
        };
        let src = base_at(self, gdt + 0x10);
        let dst = base_at(self, gdt + 0x18);
        // CX is a word count capped at 0x8000 (64 KB); larger requests are clamped.
        let words = (self.cpu.registers.ecx() as u16).min(0x8000);
        let bytes = usize::from(words) * 2;
        let data = self.read_guest_block(src, bytes);
        self.write_guest_block(dst, &data);
        self.set_eax_ah(0x00);
        self.set_int_frame_carry(false);
    }

    /// Service INT 1Ah. AH=00h/01h read and set the BDA timer tick the ROM int08
    /// maintains; AH=02h/04h read the RTC time and date as BCD (the documented
    /// contract, converted from the binary CMOS). AH=03h/05h/06h/07h are accepted
    /// as no-ops with CF clear, since the host drives the clock.
    fn handle_int1a(&mut self) {
        let ah = (self.cpu.registers.eax() as u16 >> 8) as u8;
        match ah {
            // AH=00h/01h read and set the BIOS tick count; neither reports status
            // in CF, so leaving the carry flag untouched here is intentional.
            0x00 => {
                let ticks = self.read_guest_dword(0x46c);
                let rollover = self.read_physical_u8(0x470);
                let _ = self.memory.write_u8(0x470, 0);
                self.set_eax_al(rollover);
                self.set_cx((ticks >> 16) as u16);
                self.set_dx(ticks as u16);
            }
            0x01 => {
                let cx = self.cpu.registers.ecx() as u16;
                let dx = self.cpu.registers.edx() as u16;
                let _ = self.memory.write_u16(0x46c, dx);
                let _ = self.memory.write_u16(0x46e, cx);
                let _ = self.memory.write_u8(0x470, 0);
            }
            0x02 => {
                let (_, _, _, _, hour, minute, second) = self.rtc.clock();
                let cx = (u16::from(bin_to_bcd(hour)) << 8) | u16::from(bin_to_bcd(minute));
                let dx = u16::from(bin_to_bcd(second)) << 8; // DL = 0 (no DST)
                self.set_cx(cx);
                self.set_dx(dx);
                self.set_int_frame_carry(false);
            }
            0x04 => {
                let (year, month, day, ..) = self.rtc.clock();
                let century = bin_to_bcd(self.rtc.century());
                let yy = bin_to_bcd((year % 100) as u8);
                let cx = (u16::from(century) << 8) | u16::from(yy);
                let dx = (u16::from(bin_to_bcd(month)) << 8) | u16::from(bin_to_bcd(day));
                self.set_cx(cx);
                self.set_dx(dx);
                self.set_int_frame_carry(false);
            }
            // AH=03h set RTC time: CH/CL/DH are BCD hours/minutes/seconds (DL = DST flag,
            // not modeled). Re-seed the clock keeping the current date.
            0x03 => {
                let cx = self.cpu.registers.ecx() as u16;
                let dx = self.cpu.registers.edx() as u16;
                let hour = bcd_to_bin((cx >> 8) as u8);
                let minute = bcd_to_bin(cx as u8);
                let second = bcd_to_bin((dx >> 8) as u8);
                let (year, month, day, weekday, ..) = self.rtc.clock();
                self.rtc
                    .seed(year, month, day, weekday, hour, minute, second);
                self.set_int_frame_carry(false);
            }
            // AH=05h set RTC date: CH/CL are BCD century/year, DH/DL BCD month/day.
            // Re-seed keeping the current time.
            0x05 => {
                let cx = self.cpu.registers.ecx() as u16;
                let dx = self.cpu.registers.edx() as u16;
                let century = bcd_to_bin((cx >> 8) as u8);
                let yy = bcd_to_bin(cx as u8);
                let month = bcd_to_bin((dx >> 8) as u8);
                let day = bcd_to_bin(dx as u8);
                let year = u16::from(century) * 100 + u16::from(yy);
                let (_, _, _, weekday, hour, minute, second) = self.rtc.clock();
                self.rtc
                    .seed(year, month, day, weekday, hour, minute, second);
                // Persist the century to CMOS 0x32 so it survives an NVRAM reload.
                self.rtc.set_century(century);
                self.set_int_frame_carry(false);
            }
            // AH=0Ah read the system-timer day counter: CX = days since 1980-01-01,
            // derived from the host-authoritative RTC calendar. AL = 0 (no rollover).
            0x0A => {
                let (year, month, day, ..) = self.rtc.clock();
                self.set_cx(days_since_1980(year, month, day));
                self.set_eax_al(0);
                self.set_int_frame_carry(false);
            }
            // AH=0Bh set the system-timer day counter: store CX in the BDA scratch
            // word so a later read returns it. The RTC calendar stays authoritative
            // for AH=0Ah, so this is a write-through latch the BIOS keeps for the OS.
            0x0B => {
                let cx = self.cpu.registers.ecx() as u16;
                let _ = self.memory.write_u16(BDA_DAY_COUNT, cx);
                self.set_int_frame_carry(false);
            }
            // AH=06h/07h set/cancel alarm: no alarm hardware modeled, accept and ignore.
            // AH=08h/0Ch set power-on alarm/date, AH=0Dh reset, AH=0Fh initialize RTC: all
            // documented as succeeding, and the host-driven clock makes them no-ops.
            // ponytail: power-management and alarm hardware are not modeled; these return
            // success without persisting state. Read-back alarm calls (AH=09h/0Eh) keep the
            // default carry since there is no alarm to report.
            0x06 | 0x07 | 0x08 | 0x0C | 0x0D | 0x0F => self.set_int_frame_carry(false),
            _ => self.set_int_frame_carry(true),
        }
    }

    /// Point CS:IP at a real-mode far address. Used by the boot vectors to
    /// redirect execution instead of returning through the INT's IRET stub: the
    /// run loop steps the CPU from these registers on its next iteration, so the
    /// guest resumes at `seg:off` as if the BIOS had far-jumped there.
    fn set_cs_ip(&mut self, seg: u16, off: u16) {
        self.cpu
            .registers
            .set_segment(SegmentIndex::Cs, SegmentRegister::real(seg));
        self.cpu.registers.eip = u32::from(off);
    }

    /// Service INT 19h (BOOTSTRAP LOADER). Re-run the boot: load the boot sector of
    /// the default drive to 0000:7C00 and jump there. The default drive is A: when
    /// a floppy is mounted, otherwise C: (the Toka-DOS HLE boot). When neither is
    /// bootable, fall through to the INT 18h path. DL carries the drive the loaded
    /// code booted from (00h floppy, 80h fixed disk), the way a real BIOS leaves it.
    ///
    /// This mirrors the izarra-bios ROM's own INT 19h: a mounted floppy is treated
    /// as bootable and sector 0 is loaded with no 0xAA55 signature check, so a guest
    /// re-invoking INT 19h gets the same outcome the ROM gives at power-on.
    ///
    /// ponytail: the floppy boot copies sector 0 and jumps; it does not retry on a
    /// read error. C: boots the bundled Toka-DOS through the existing HLE record, not
    /// an arbitrary MBR. To lift this, read the C: partition table and load a
    /// guest-supplied MBR the same way the floppy path loads its boot sector.
    fn handle_int19(&mut self) {
        // A: floppy first. Copy its boot sector (CHS 0,0,1) to 0000:7C00 and jump
        // there. A mounted floppy is bootable (matching the ROM path); only an
        // unreadable sector 0 falls through.
        if let Some(sector) = self
            .floppy
            .as_ref()
            .and_then(|f| f.read_sector(0, 0, 1))
            .filter(|s| s.len() >= 512)
            .map(<[u8]>::to_vec)
        {
            self.write_guest_block(BOOT_SECTOR_ADDRESS as u32, &sector[..512]);
            self.cpu.registers.set_edx(0x00); // DL = 00h: booted from floppy A:
            self.set_cs_ip(0x0000, BOOT_SECTOR_ADDRESS as u16);
            return;
        }
        // No bootable floppy: try the C: Toka-DOS HLE boot. A zero status means the
        // boot record landed at 0x7C00 and the DOS base is set up; jump to it.
        if self.toka_load_boot_record() == 0 {
            self.cpu.registers.set_edx(0x80); // DL = 80h: booted from fixed disk C:
            self.set_cs_ip(0x0000, BOOT_SECTOR_ADDRESS as u16);
            return;
        }
        // Nothing bootable: hand off to the diskless/no-boot path.
        self.handle_int18();
    }

    /// Service INT 18h (DISKLESS BOOT HOOK). On a real PC this entered ROM BASIC;
    /// the Izarra 3000 has none, so it reports no bootable device and halts. The
    /// halt stub clears IF first, so the machine genuinely stops rather than
    /// spinning on the timer tick.
    fn handle_int18(&mut self) {
        // A real BIOS prints a "no bootable device" message here. The text screen
        // is the BIOS's, so write the line through the same teletype path the rest
        // of the BIOS uses, then jump to the CLI;HLT stub.
        for &byte in b"No bootable device\r\n" {
            self.teletype_char(byte);
        }
        self.set_cs_ip(0x0000, BIOS_HALT_STUB_ADDRESS as u16);
    }

    /// Replace the low 16 bits of EAX, leaving the upper 16 intact.
    fn set_ax(&mut self, ax: u16) {
        let eax = (self.cpu.registers.eax() & !0xFFFF) | u32::from(ax);
        self.cpu.registers.set_eax(eax);
    }

    /// Replace the low 16 bits of EBX.
    fn set_bx(&mut self, bx: u16) {
        let ebx = (self.cpu.registers.ebx() & !0xFFFF) | u32::from(bx);
        self.cpu.registers.set_ebx(ebx);
    }

    /// Replace the low 16 bits of ECX.
    fn set_cx(&mut self, cx: u16) {
        let ecx = (self.cpu.registers.ecx() & !0xFFFF) | u32::from(cx);
        self.cpu.registers.set_ecx(ecx);
    }

    /// Replace the low 16 bits of EDX.
    fn set_dx(&mut self, dx: u16) {
        let edx = (self.cpu.registers.edx() & !0xFFFF) | u32::from(dx);
        self.cpu.registers.set_edx(edx);
    }

    /// Service the INT 33h mouse driver (Microsoft API). The subset DOS games
    /// rely on: reset/detect, show/hide cursor, get position+buttons, set
    /// position, define horizontal/vertical ranges, and read the mickey motion
    /// counters. The PS/2 aux device is the hardware behind it; this HLE tracks
    /// the same position the host feeds through `inject_mouse`, so a game that
    /// polls INT 33h sees the pointer without writing its own IRQ12 ISR.
    /// Functions outside this subset return with the registers unchanged.
    fn handle_int33(&mut self) {
        let ax = self.cpu.registers.eax() as u16;
        let cx = self.cpu.registers.ecx() as u16;
        let dx = self.cpu.registers.edx() as u16;
        match ax {
            // AX=0000: reset driver and read status. Re-centre the cursor, hide
            // it, clear motion, and report "installed, 2 buttons".
            0x0000 => {
                self.mouse = MouseState::default();
                self.set_ax(0xFFFF); // driver installed
                self.set_bx(0x0002); // two-button mouse
            }
            // AX=0001: show cursor. The visibility counter counts up toward 0 and
            // saturates there, never going positive. From the reset value of -1 a single
            // Show reaches the visible state (0); the saturation keeps extra Shows from
            // banking credit, so N hides require exactly N shows to undo (RBIL INT 33h
            // AX=0002 note). `.min(0)` is the high-end cap, not a clamp to a maximum of 0
            // from below.
            0x0001 => {
                self.mouse.show_count = (self.mouse.show_count + 1).min(0);
            }
            // AX=0002: hide cursor. Decrement without a lower bound, so successive hides
            // stack and each needs a matching show to reverse.
            0x0002 => {
                self.mouse.show_count -= 1;
            }
            // AX=0003: return position and button status.
            0x0003 => {
                self.set_bx(u16::from(self.mouse.buttons));
                self.set_cx(self.mouse.x as u16);
                self.set_dx(self.mouse.y as u16);
            }
            // AX=0004: position the cursor at CX (column), DX (row), clamped.
            0x0004 => {
                self.mouse.x = i32::from(cx).clamp(self.mouse.min_x, self.mouse.max_x);
                self.mouse.y = i32::from(dx).clamp(self.mouse.min_y, self.mouse.max_y);
            }
            // AX=0007: define horizontal range (CX..DX). A reversed pair is
            // swapped, the way the driver normalizes the limits.
            0x0007 => {
                let (lo, hi) = order(i32::from(cx), i32::from(dx));
                self.mouse.min_x = lo.clamp(0, MOUSE_MAX_X);
                self.mouse.max_x = hi.clamp(0, MOUSE_MAX_X);
                self.mouse.x = self.mouse.x.clamp(self.mouse.min_x, self.mouse.max_x);
            }
            // AX=0008: define vertical range (CX..DX).
            0x0008 => {
                let (lo, hi) = order(i32::from(cx), i32::from(dx));
                self.mouse.min_y = lo.clamp(0, MOUSE_MAX_Y);
                self.mouse.max_y = hi.clamp(0, MOUSE_MAX_Y);
                self.mouse.y = self.mouse.y.clamp(self.mouse.min_y, self.mouse.max_y);
            }
            // AX=000B: read and clear the mickey motion counters. Returned as
            // 16-bit signed deltas; positive is right/down.
            0x000B => {
                self.set_cx(self.mouse.mickey_x as i16 as u16);
                self.set_dx(self.mouse.mickey_y as i16 as u16);
                self.mouse.mickey_x = 0;
                self.mouse.mickey_y = 0;
            }
            // Other functions are accepted as no-ops, leaving registers as-is.
            _ => {}
        }
    }

    /// Service the ICDEX functions of `INT 2Fh` (the multiplex interrupt) as an
    /// HLE bridge, so the guest sees a CD drive without a real driver loaded. The
    /// CD-ROM is exposed at the drive letter `CD_DRIVE_NUMBER` (0 = A:), which is
    /// D: by default. Only the query and device-driver-request functions are
    /// modeled; unrecognized AX values fall through unchanged so other INT 2Fh
    /// consumers are unaffected. Returns true if the call was an ICDEX function
    /// this bridge handled.
    fn handle_int2f(&mut self) -> bool {
        let ax = self.cpu.registers.eax() as u16;
        match ax {
            // Network-redirector / ICDEX installation check (RBIL INTERRUP.K,
            // INT 2F/AX=1100h). The caller pushes a DADAh marker, runs INT 2Fh,
            // and a present ICDEX returns AL=FFh and replaces the pushed word
            // with ADADh. A strict probe checks that the word changed, so we
            // rewrite it. The INT pushed IP, CS, FLAGS over the marker, so the
            // marker sits at SS:SP+6. Only the DADAh marker is the install check;
            // any other pushed value is some other 1100h subfunction and is left
            // unhandled rather than falsely reporting installed.
            0x1100 => {
                let ss = self.cpu.registers.segment(SegmentIndex::Ss).base;
                let sp = self.cpu.registers.esp() as u16;
                let marker_addr = ss + u32::from(sp.wrapping_add(6));
                if self.read_guest_word(marker_addr) == 0xDADA {
                    let _ = self.memory.write_u16(marker_addr as usize, 0xADAD);
                    self.set_eax_al(0xFF);
                    true
                } else {
                    false
                }
            }
            // CD-ROM installation check: BX = number of CD drives, CX = first
            // drive letter (0 = A:).
            0x1500 => {
                // One CD drive is always present (D:), even with no disc loaded:
                // a game maps the drive letter before inserting media.
                let bx = 1u16;
                let ebx = (self.cpu.registers.ebx() & !0xFFFF) | u32::from(bx);
                self.cpu.registers.set_ebx(ebx);
                let ecx = (self.cpu.registers.ecx() & !0xFFFF) | u32::from(CD_DRIVE_NUMBER);
                self.cpu.registers.set_ecx(ecx);
                true
            }
            // Get drive device list: ES:BX -> 5 bytes per drive (subunit + driver
            // header far pointer). We write one entry: subunit 0, a null header
            // pointer (the guest only needs the drive count/letter to map the
            // drive; the header is informational for our HLE path).
            0x1501 => {
                let es = self.cpu.registers.segment(SegmentIndex::Es).base;
                let bx = self.cpu.registers.ebx() as u16;
                let addr = es.wrapping_add(u32::from(bx));
                self.write_guest_block(addr, &[0u8; 5]); // subunit 0, header 0:0
                true
            }
            // Get CD-ROM drive letters: ES:BX -> one byte per drive letter, the
            // drive number (0 = A:). One CD drive.
            0x150D => {
                let es = self.cpu.registers.segment(SegmentIndex::Es).base;
                let bx = self.cpu.registers.ebx() as u16;
                let addr = es.wrapping_add(u32::from(bx));
                self.write_guest_block(addr, &[CD_DRIVE_NUMBER]);
                true
            }
            // Drive check: BX = ADADh signals ICDEX present; AX nonzero if the
            // drive in CX is a supported CD-ROM.
            0x150B => {
                let cx = self.cpu.registers.ecx() as u16;
                let supported = u16::from(cx == u16::from(CD_DRIVE_NUMBER));
                let eax = (self.cpu.registers.eax() & !0xFFFF) | u32::from(supported);
                self.cpu.registers.set_eax(eax);
                let ebx = (self.cpu.registers.ebx() & !0xFFFF) | 0xADAD;
                self.cpu.registers.set_ebx(ebx);
                true
            }
            // XMS install check (INT 2F/AX=4300h). The host XMS driver is present, so
            // report AL=80h. The guest then asks for the entry point with AX=4310h.
            0x4300 => {
                self.set_eax_al(0x80);
                true
            }
            // Get XMS driver entry point (AX=4310h): return ES:BX -> the INT 66h entry
            // stub in ROM. The guest FAR-CALLs it to reach the XMS functions.
            0x4310 => {
                self.cpu
                    .registers
                    .set_segment(SegmentIndex::Es, SegmentRegister::real(XMS_ENTRY_SEG));
                self.set_bx(XMS_ENTRY_OFF);
                true
            }
            // Get ICDEX version: BH = major, BL = minor. Report 2.23.
            0x150C => {
                let ebx = (self.cpu.registers.ebx() & !0xFFFF) | 0x0217; // 2.23
                self.cpu.registers.set_ebx(ebx);
                true
            }
            // Send device driver request: ES:BX -> a CD-ROM device driver request
            // header. CX = drive number. Dispatch it to the ATAPI device.
            0x1510 => {
                let cx = self.cpu.registers.ecx() as u16;
                if cx != u16::from(CD_DRIVE_NUMBER) {
                    // Invalid drive: CF set, AX = 000Fh.
                    let eax = (self.cpu.registers.eax() & !0xFFFF) | 0x000F;
                    self.cpu.registers.set_eax(eax);
                    self.set_int_frame_carry(true);
                    return true;
                }
                let es = self.cpu.registers.segment(SegmentIndex::Es).base;
                let bx = self.cpu.registers.ebx() as u16;
                let header = es.wrapping_add(u32::from(bx));
                self.icdex_device_request(header);
                self.set_int_frame_carry(false);
                true
            }
            _ => false,
        }
    }

    /// Service the XMS driver, reached when a guest FAR-CALLs the entry point
    /// (INT 2Fh AX=4310h) and the stub runs `INT 66h`. The function is in AH; on
    /// return AX=1 means success, AX=0 means failure with the error code in BL.
    /// Register conventions follow the XMS 3.0 specification. The pure allocator
    /// and HMA/A20 bookkeeping live in `xms.rs`; this method moves values between
    /// the guest registers/memory and that state, and routes A20 to the 8042.
    fn handle_xms(&mut self) {
        let ah = (self.cpu.registers.eax() as u16 >> 8) as u8;
        match ah {
            // 00h: get version. AX = XMS version (3.00), BX = driver revision,
            // DX = 1 if the HMA exists. This call never fails.
            0x00 => {
                self.set_ax(0x0300);
                self.set_bx(xms::DRIVER_REVISION);
                self.set_dx(u16::from(self.xms.hma_exists()));
            }
            // 01h request HMA, 02h release HMA. DX carries the space the caller
            // needs (0xFFFF means the whole HMA); request_hma gates it on /HMAMIN.
            0x01 => {
                let space_needed = self.cpu.registers.edx() as u16;
                let result = self.xms.request_hma(space_needed);
                self.xms_result(result);
            }
            0x02 => {
                let result = self.xms.release_hma();
                self.xms_result(result);
            }
            // 03h global enable A20, 04h global disable A20. Route to the single
            // A20 source of truth (the 8042 output port, shared with port 0x92).
            0x03 => {
                self.keyboard.set_a20(true);
                self.xms_success();
            }
            0x04 => {
                self.keyboard.set_a20(false);
                self.xms_success();
            }
            // 05h local enable A20, 06h local disable A20. Keep the nesting count
            // and drive the gate only on the 0<->1 transition.
            0x05 => {
                if self.xms.local_enable_a20() {
                    self.keyboard.set_a20(true);
                }
                self.xms_success();
            }
            0x06 => {
                if self.xms.local_disable_a20() {
                    self.keyboard.set_a20(false);
                }
                self.xms_success();
            }
            // 07h query A20: AX = 1 if enabled, else 0. BL = 0 (this is a query,
            // not an error path), per the XMS 3.0 spec.
            0x07 => {
                self.set_ax(u16::from(self.keyboard.a20_enabled()));
                self.set_eax_bl_ok();
            }
            // 08h query free extended memory: AX = largest free block KB,
            // DX = total free KB. BL = 0 on success.
            0x08 => {
                let (largest, total) = self.xms.query_free();
                self.set_ax(largest.min(0xFFFF) as u16);
                self.set_dx(total.min(0xFFFF) as u16);
                self.set_eax_bl_ok();
            }
            // 09h allocate EMB: DX = KB requested -> DX = handle. AX = 1 / BL = err.
            0x09 => {
                let kb = self.cpu.registers.edx() as u16;
                match self.xms.allocate(kb) {
                    Ok(handle) => {
                        self.set_dx(handle);
                        self.xms_success();
                    }
                    Err(code) => self.xms_failure(code),
                }
            }
            // 0Ah free EMB: DX = handle.
            0x0A => {
                let handle = self.cpu.registers.edx() as u16;
                let result = self.xms.free(handle);
                self.xms_result(result);
            }
            // 0Bh move EMB: DS:SI -> the move descriptor.
            0x0B => self.xms_move(),
            // 0Ch lock EMB: DX = handle -> DX:BX = 32-bit linear address, lock++.
            0x0C => {
                let handle = self.cpu.registers.edx() as u16;
                match self.xms.lock(handle) {
                    Ok(linear) => {
                        self.set_dx((linear >> 16) as u16);
                        self.set_bx(linear as u16);
                        self.xms_success();
                    }
                    Err(code) => self.xms_failure(code),
                }
            }
            // 0Dh unlock EMB: DX = handle.
            0x0D => {
                let handle = self.cpu.registers.edx() as u16;
                let result = self.xms.unlock(handle);
                self.xms_result(result);
            }
            // 0Eh get EMB handle info: DX = handle -> BH = lock count,
            // BL = free handles, DX = size KB.
            0x0E => {
                let handle = self.cpu.registers.edx() as u16;
                match self.xms.handle_info(handle) {
                    Ok((lock, free_handles, size_kb)) => {
                        self.set_bx((u16::from(lock) << 8) | u16::from(free_handles));
                        self.set_dx(size_kb);
                        // 0Eh signals success with AX=1 and leaves BL as the free
                        // handle count, so set AX without touching BL.
                        self.set_ax(0x0001);
                    }
                    Err(code) => self.xms_failure(code),
                }
            }
            // 0Fh resize EMB: BX = new KB, DX = handle.
            0x0F => {
                let kb = self.cpu.registers.ebx() as u16;
                let handle = self.cpu.registers.edx() as u16;
                let result = self.xms.resize(handle, kb);
                self.xms_result(result);
            }
            // 10h request UMB, 11h release UMB. No upper memory blocks are modeled,
            // so 10h fails "no UMBs available" (BL=B1h) with the largest available
            // block in DX=0; 11h fails "invalid segment" (BL=B2h). A guest reads
            // this as "no UMB" and falls back to conventional memory.
            // ponytail: no UMB region exists yet. The upgrade path is the memory
            // mapper backing a UMB window in the 0xC000-0xEFFF hole and answering
            // 10h/11h from it.
            0x10 => {
                self.set_dx(0x0000);
                self.xms_failure(0xB1);
            }
            0x11 => self.xms_failure(0xB2),
            // Unimplemented function: AX=0, BL=80h (not implemented).
            _ => self.xms_failure(xms::err::NOT_IMPLEMENTED),
        }
    }

    /// Report XMS success: AX = 1, BL = 0.
    fn xms_success(&mut self) {
        self.set_ax(0x0001);
        self.set_eax_bl_ok();
    }

    /// Report XMS failure: AX = 0, BL = error code.
    fn xms_failure(&mut self, code: u8) {
        self.set_ax(0x0000);
        let ebx = (self.cpu.registers.ebx() & !0xFF) | u32::from(code);
        self.cpu.registers.set_ebx(ebx);
    }

    /// Map a `Result<(), u8>` from the XMS state onto the AX/BL convention.
    fn xms_result(&mut self, result: Result<(), u8>) {
        match result {
            Ok(()) => self.xms_success(),
            Err(code) => self.xms_failure(code),
        }
    }

    /// Clear BL (the low byte of EBX), leaving the rest of EBX intact. Success
    /// returns BL = 0.
    fn set_eax_bl_ok(&mut self) {
        let ebx = self.cpu.registers.ebx() & !0xFF;
        self.cpu.registers.set_ebx(ebx);
    }

    /// XMS function 0Bh: move memory between EMBs and/or conventional memory using
    /// the move descriptor at DS:SI. The descriptor is, little-endian:
    ///   +0  DWORD length in bytes
    ///   +4  WORD  source handle (0 = real-mode pointer)
    ///   +6  DWORD source offset (or seg:off if handle 0)
    ///   +10 WORD  dest handle (0 = real-mode pointer)
    ///   +12 DWORD dest offset (or seg:off if handle 0)
    /// A zero length is a legal no-op success. Both endpoints are resolved to
    /// linear addresses and bounds-checked; the copy goes byte by byte through the
    /// flat address space, so overlapping ranges copy correctly when dest < src.
    fn xms_move(&mut self) {
        let ds = self.cpu.registers.segment(SegmentIndex::Ds).base;
        let si = self.cpu.registers.esi() as u16;
        let desc = ds.wrapping_add(u32::from(si));
        let length = self.read_guest_dword(desc);
        let src_handle = self.read_guest_word(desc + 4);
        let src_offset = self.read_guest_dword(desc + 6);
        let dst_handle = self.read_guest_word(desc + 10);
        let dst_offset = self.read_guest_dword(desc + 12);

        if length == 0 {
            self.xms_success();
            return;
        }
        // The XMS spec requires an even byte count; an odd length is error A7h.
        if length % 2 != 0 {
            self.xms_failure(xms::err::INVALID_LENGTH);
            return;
        }

        let src = match self.xms.move_endpoint(
            src_handle,
            src_offset,
            length,
            xms::err::INVALID_SOURCE_HANDLE,
            xms::err::INVALID_SOURCE_OFFSET,
        ) {
            Ok(addr) => addr,
            Err(code) => {
                self.xms_failure(code);
                return;
            }
        };
        let dst = match self.xms.move_endpoint(
            dst_handle,
            dst_offset,
            length,
            xms::err::INVALID_DEST_HANDLE,
            xms::err::INVALID_DEST_OFFSET,
        ) {
            Ok(addr) => addr,
            Err(code) => {
                self.xms_failure(code);
                return;
            }
        };

        // Copy through the flat space. Overlap-safe: when dest is below source,
        // copy ascending; otherwise descending. (The XMS spec leaves overlapping
        // behavior undefined, but a real HIMEM does the non-clobbering direction.)
        if dst <= src {
            for i in 0..length {
                let byte = self.read_physical_u8(src + i);
                self.write_physical_u8(dst + i, byte);
            }
        } else {
            for i in (0..length).rev() {
                let byte = self.read_physical_u8(src + i);
                self.write_physical_u8(dst + i, byte);
            }
        }
        self.xms_success();
    }

    /// Service INT 28h (DOS idle). DOS calls this from its keyboard-wait loop so a
    /// TSR can do background work. With no TSR installed it is a no-op return; the
    /// guest's FLAGS image is untouched, the way the default IRET stub left it.
    fn handle_int28(&mut self) {}

    /// Service INT 29h (DOS fast console output). The character is in AL; write it to
    /// the active page through the same teletype path INT 10h AH=0Eh uses, the way
    /// the DOS CON device's fast-output hook does.
    fn handle_int29(&mut self) {
        let al = self.cpu.registers.eax() as u8;
        self.teletype_char(al);
    }

    /// Execute one CD-ROM device driver request whose header begins at linear
    /// `header`. Decodes the command code and the per-command fields (see RBIL
    /// table 02597) and drives the ATAPI device, writing data back to the
    /// transfer address and the status word back into the header. Supports the
    /// CD commands a game uses: READ LONG (0x80), SEEK (0x83), PLAY AUDIO (0x84),
    /// STOP (0x85), RESUME (0x88), and IOCTL INPUT (0x03) device-status queries.
    fn icdex_device_request(&mut self, header: u32) {
        let command = self.read_physical_u8(header + 2);
        // Status word at offset 3: bit 8 = done, bit 15 = error, low byte = code.
        let mut status: u16 = 0x0100; // done
        match command {
            // READ LONG: read `count` sectors starting at the given sector into
            // the transfer address. Addressing mode 0 = HSG (LBA), 1 = Red Book.
            0x80 => {
                let addr_mode = self.read_physical_u8(header + 0x0D);
                let xfer = self.read_guest_dword(header + 0x0E);
                let count = self.read_guest_word(header + 0x12);
                let start = self.read_guest_dword(header + 0x14);
                let lba = self.driver_addr_to_lba(addr_mode, start);
                let mut ok = true;
                for i in 0..u32::from(count) {
                    match self
                        .ide
                        .device()
                        .image()
                        .and_then(|img| img.read_data_sector(lba + i))
                    {
                        Some(sector) => {
                            self.write_guest_block(
                                xfer.wrapping_add(i * cdimage::DATA_SECTOR as u32),
                                &sector,
                            );
                        }
                        None => {
                            ok = false;
                            break;
                        }
                    }
                }
                self.cd_accesses += 1;
                if !ok {
                    status = 0x8000 | 0x0100 | 0x000F; // error + done, sector not found
                }
            }
            // SEEK: advisory; accept it (the timing model does not need it).
            0x83 => {}
            // PLAY AUDIO: start playback at the given sector for `count` sectors.
            0x84 => {
                let addr_mode = self.read_physical_u8(header + 0x0D);
                let start = self.read_guest_dword(header + 0x0E);
                let count = self.read_guest_dword(header + 0x12);
                let lba = self.driver_addr_to_lba(addr_mode, start);
                let mut cdb = [0u8; 12];
                cdb[0] = 0x45; // PLAY AUDIO(10)
                cdb[2..6].copy_from_slice(&lba.to_be_bytes());
                let frames = count.min(u32::from(u16::MAX)) as u16;
                cdb[7..9].copy_from_slice(&frames.to_be_bytes());
                if matches!(self.ide.device_mut().execute(&cdb), atapi::CmdResult::Error) {
                    status = 0x8000 | 0x0100 | 0x000F;
                }
            }
            // STOP AUDIO.
            0x85 => {
                let mut cdb = [0u8; 12];
                cdb[0] = 0x4E;
                let _ = self.ide.device_mut().execute(&cdb);
            }
            // RESUME AUDIO.
            0x88 => {
                let mut cdb = [0u8; 12];
                cdb[0] = 0x4B;
                cdb[8] = 0x01; // resume bit
                let _ = self.ide.device_mut().execute(&cdb);
            }
            // IOCTL INPUT and any other command: report done with no data. A
            // real driver answers control-block queries here; a game that only
            // needs the data/audio path tolerates a benign success.
            _ => {}
        }
        // Write the status word back into the header (offset 3).
        let _ = self.memory.write_u16(header as usize + 3, status);
    }

    /// Convert a CD device-driver address (HSG LBA when `addr_mode` == 0, packed
    /// Red Book frame/second/minute when 1) to a logical LBA.
    fn driver_addr_to_lba(&self, addr_mode: u8, raw: u32) -> u32 {
        if addr_mode == 0 {
            raw // HSG = logical sector number = LBA
        } else {
            // Red Book packed as frame/second/minute/unused in the low bytes.
            let frame = raw as u8;
            let second = (raw >> 8) as u8;
            let minute = (raw >> 16) as u8;
            cdimage::msf_to_lba(minute, second, frame)
        }
    }

    fn read_guest_word(&mut self, addr: u32) -> u16 {
        let lo = self.read_physical_u8(addr);
        let hi = self.read_physical_u8(addr + 1);
        u16::from_le_bytes([lo, hi])
    }

    fn read_guest_dword(&mut self, addr: u32) -> u32 {
        let bytes = [
            self.read_physical_u8(addr),
            self.read_physical_u8(addr + 1),
            self.read_physical_u8(addr + 2),
            self.read_physical_u8(addr + 3),
        ];
        u32::from_le_bytes(bytes)
    }

    /// Consume `secs` of emulated time for a device operation that blocks the
    /// guest (a floppy seek/read). Advancing both the master clock and the devices
    /// by the same amount keeps timekeeping coupled, the way an instruction's own
    /// clocks do. The guest clock jumps forward; the GUI's realtime pacing then
    /// turns that jump into a visible wall-clock wait. `clock_hz` is the live mode
    /// rate so the cost scales with the active GSW speed.
    fn stall_for(&mut self, secs: f64) {
        if secs <= 0.0 {
            return;
        }
        // Jump the master clock so the GUI's realtime pacing turns the access into
        // a wall-clock wait. Keep the time-of-day RTC advancing (O(1)), but do NOT
        // step the PIT/speaker/sound devices per clock: pushing a multi-million-
        // clock jump through advance_devices is the O(n) spin the HLT wake path is
        // careful to clamp, and the guest runs no instructions during the stall, so
        // it cannot observe their intermediate state. They resume cleanly from the
        // next instruction's own advance.
        let extra = (secs * self.active_mode.clock_hz() as f64) as u64;
        self.elapsed_clocks += extra;
        self.io_stall_clocks += extra;
        self.rtc_seconds += secs;
        let whole = self.rtc_seconds.floor();
        if whole >= 1.0 {
            self.rtc.tick_seconds(whole as u64);
            self.rtc_seconds -= whole;
        }
    }

    /// Service the host side of an `INT 13h` disk request. Only floppy A: (DL=0)
    /// is backed, by the mounted image. CHS to LBA uses the mounted media
    /// geometry, so a 720 KB disk reads with 9 sectors per track and a 1.44 MB
    /// disk with 18. Status is returned through AH and the carry flag the way a
    /// real BIOS reports it: CF clear and AH=0 on success, CF set with an error
    /// code in AH on failure.
    fn handle_int13(&mut self) {
        let ax = self.cpu.registers.eax() as u16;
        let ah = (ax >> 8) as u8;
        let dx = self.cpu.registers.edx() as u16;
        let dl = dx as u8;

        // Fixed-disk path: DL bit 7 selects a hard drive (0x80 = C:). Serviced
        // before the floppy early-return so a guest with no floppy but a mounted
        // hard disk still boots. EDD AH=41h-48h dispatch here too. Only taken when
        // a hard disk is actually mounted: with no disk the call falls through to
        // the no-op return below, which the firmware boot suite relies on (it
        // places its second stage in memory directly and issues INT 13h AH=02 with
        // DL=0x80 and carry pre-cleared, expecting a no-op success).
        if dl >= 0x80 && self.ata.is_some() {
            self.int13_hdd(ah, dl);
            return;
        }

        // With no floppy image mounted there is no drive to service. Leave the
        // registers and the IRET FLAGS image untouched so the guest sees the same
        // result the bare IRET stub gave before this handler existed.
        if self.floppy.is_none() {
            return;
        }

        match ah {
            // AH=00 reset disk system: the heads recalibrate back to track 0,
            // which steps the drive and takes time.
            0x00 => {
                let secs = self
                    .floppy
                    .as_mut()
                    .map_or(0.0, |f| f.access_duration_secs(0, 0));
                self.stall_for(secs);
                self.set_eax_ah(0x00);
                self.set_disk_status(0x00);
                self.set_int_frame_carry(false);
            }
            // AH=01 get last disk status. The documented result register is AH; PS/2
            // BIOSes mirror the status into AL as well. CF reflects a nonzero (error)
            // status. The status byte itself lives in BDA 0040:0041.
            0x01 => {
                let status = self.read_physical_u8(0x441);
                self.set_eax_ah(status);
                self.set_eax_al(status);
                self.set_int_frame_carry(status != 0);
            }
            // AH=02 read sectors, AH=03 write sectors. AL = sector count, CH/CL
            // carry the cylinder and sector (CL bits 0-5 sector, bits 6-7 the
            // cylinder high bits), DH = head, DL = drive, ES:BX = buffer.
            0x02 | 0x03 => self.int13_transfer(ah, dl),
            // AH=04 verify sectors: read without copying, report sectors checked.
            0x04 => self.int13_verify(dl),
            // AH=05 format track: fill the addressed track with the format filler.
            0x05 => self.int13_format_track(dl),
            // AH=08 read drive parameters. Report the mounted media geometry.
            0x08 => self.int13_drive_parameters(dl),
            // AH=15 get DASD type for a floppy. A mounted floppy reports AH=01
            // (no change-line), an absent floppy AH=00 (no such drive). DL>=0x80
            // never reaches here: the fixed-disk path handled it above.
            0x15 => {
                let mounted = dl == 0x00 && self.floppy.is_some();
                self.set_eax_ah(if mounted { 0x01 } else { 0x00 });
                self.set_disk_status(0x00);
                self.set_int_frame_carry(false);
            }
            // Genuinely unknown subfunctions report invalid-function, the way a
            // real BIOS does, instead of a false success.
            _ => {
                self.set_eax_ah(0x01);
                self.set_disk_status(0x01);
                self.set_int_frame_carry(true);
            }
        }
    }

    /// Record the INT 13h result in BDA 0040:0041 (last disk status) so AH=01h can
    /// report it. 0x00 is success; any other value is the error code.
    fn set_disk_status(&mut self, status: u8) {
        let _ = self.memory.write_u8(0x441, status);
    }

    /// INT 25h ABSOLUTE DISK READ (DOS). AL=drive (0=A:), CX=sector count, DX=first
    /// logical (LBA) sector, DS:BX=buffer. Classic form only; the >32 MB packet form
    /// (CX=0xFFFF, DS:BX -> a parameter block) is out of scope. On success AX=0 and
    /// CF clear; on error CF set and AX=0x40xx. ponytail: the real INT 25h/26h leave
    /// the original FLAGS on the stack for the caller to discard with its own POPF,
    /// but this HLE returns through the standard IRET stub, so the result CF is
    /// written into the IRET FLAGS image (set_int_frame_carry) like every other
    /// host-serviced INT here.
    fn handle_int25(&mut self) {
        self.int25_26_transfer(false);
    }

    /// INT 26h ABSOLUTE DISK WRITE (DOS). Same register layout as INT 25h; the
    /// DS:BX buffer is the source. See handle_int25 for the FLAGS-frame note.
    fn handle_int26(&mut self) {
        self.int25_26_transfer(true);
    }

    /// Shared body of INT 25h/26h. Converts each logical sector to CHS through the
    /// mounted floppy geometry and reads or writes it against the DS:BX buffer.
    fn int25_26_transfer(&mut self, write: bool) {
        let al = self.cpu.registers.eax() as u8;
        let count = self.cpu.registers.ecx() as u16;
        let start_lba = self.cpu.registers.edx() as u16;
        let ds = self.cpu.registers.segment(SegmentIndex::Ds).base;
        let bx = self.cpu.registers.ebx() as u16;
        let buffer = ds.wrapping_add(u32::from(bx));

        // Drive C: (AL=2) is served by the synthesized FAT32 volume when one is
        // mounted. Every 16-bit logical sector falls in the first 32 MB of any
        // FAT32 volume (its sector count exceeds 0xFFFF), so the classic form
        // addresses a valid sector and never reports out of range here. The
        // packet form (CX=FFFFh / AH=7305h) for sectors past 32 MB is a follow-up.
        if al == 0x02 && self.fat32_c.is_some() {
            if write {
                // The volume is read-only: an absolute write is write-protected.
                // AL=00h is the write-protect error (INT 24h code); AH=03h is the
                // write-protected disk status.
                self.set_ax(0x0300);
                self.set_int_frame_carry(true);
                return;
            }
            // Stream one sector at a time: each read borrows the volume only until
            // the [u8;512] is returned by value, freeing self for the write, so a
            // 512-byte buffer suffices rather than materializing up to 32 MB.
            for i in 0..count {
                let sector = self
                    .fat32_c
                    .as_ref()
                    .unwrap()
                    .read_sector(u32::from(start_lba.wrapping_add(i)));
                let addr = buffer.wrapping_add(u32::from(i) * 512);
                self.write_guest_block(addr, &sector);
            }
            self.set_ax(0);
            self.set_int_frame_carry(false);
            return;
        }

        // Only floppy A: is backed. Any other drive, or no media, reports a
        // drive-not-ready error (AX low byte 0x02 = drive not ready, high byte 0x40
        // = seek failed, per the DOS error-byte convention).
        let Some(geom) = self.floppy.as_ref().map(|f| f.geometry()) else {
            self.set_ax(0x4002);
            self.set_int_frame_carry(true);
            return;
        };
        if al != 0x00 {
            self.set_ax(0x4002);
            self.set_int_frame_carry(true);
            return;
        }

        self.floppy_accesses += 1;
        let spt = u16::from(geom.sectors);
        let heads = u16::from(geom.heads);
        let mut last_cyl = 0u16;
        for i in 0..count {
            let lba = start_lba.wrapping_add(i);
            // LBA -> CHS for the mounted geometry. The floppy sector index is 1-based.
            let sector = (lba % spt) as u8 + 1;
            let head = ((lba / spt) % heads) as u8;
            let cyl = lba / spt / heads;
            last_cyl = cyl;
            let addr = buffer.wrapping_add(u32::from(i) * 512);
            if write {
                let bytes = self.read_guest_block(addr, 512);
                let ok = self
                    .floppy
                    .as_mut()
                    .map(|f| f.write_sector(cyl, head, sector, &bytes))
                    .unwrap_or(false);
                if !ok {
                    // Sector off the mounted media: sector-not-found (0x40 = seek
                    // failed, 0x08 = sector not found).
                    self.set_ax(0x4008);
                    self.set_int_frame_carry(true);
                    return;
                }
            } else {
                let data = self
                    .floppy
                    .as_ref()
                    .and_then(|f| f.read_sector(cyl, head, sector))
                    .map(<[u8]>::to_vec);
                match data {
                    Some(bytes) => self.write_guest_block(addr, &bytes),
                    None => {
                        self.set_ax(0x4008);
                        self.set_int_frame_carry(true);
                        return;
                    }
                }
            }
        }

        // Charge the drive's mechanical time for the access the way INT 13h does.
        if count > 0 {
            let bytes = usize::from(count) * 512;
            let secs = self
                .floppy
                .as_mut()
                .map_or(0.0, |f| f.access_duration_secs(last_cyl, bytes));
            self.stall_for(secs);
        }

        self.set_ax(0x0000);
        self.set_int_frame_carry(false);
    }

    /// AH=04h verify: confirm the requested sectors are readable without copying
    /// them into the caller buffer. AL returns the count verified.
    fn int13_verify(&mut self, dl: u8) {
        if dl != 0x00 || self.floppy.is_none() {
            self.set_eax_ah(0x80);
            self.set_disk_status(0x80);
            self.set_int_frame_carry(true);
            return;
        }
        let ax = self.cpu.registers.eax() as u16;
        let count = ax as u8;
        let cx = self.cpu.registers.ecx() as u16;
        let cl = cx as u8;
        let ch = (cx >> 8) as u8;
        let sector = cl & 0x3f;
        let cyl = u16::from(ch) | (u16::from(cl & 0xc0) << 2);
        let head = (self.cpu.registers.edx() as u16 >> 8) as u8;
        let mut done = 0u8;
        for i in 0..count {
            let present = self
                .floppy
                .as_ref()
                .and_then(|f| f.read_sector(cyl, head, sector + i))
                .is_some();
            if !present {
                break;
            }
            done += 1;
        }
        self.set_eax_al(done);
        if done == count {
            self.set_eax_ah(0x00);
            self.set_disk_status(0x00);
            self.set_int_frame_carry(false);
        } else {
            self.set_eax_ah(0x04);
            self.set_disk_status(0x04);
            self.set_int_frame_carry(true);
        }
    }

    /// AH=05h format track. AL = sectors per track to format, CH = cylinder, DH =
    /// head, ES:BX = a list of 4-byte address-field records (C,H,R,N). Only floppy
    /// A: is backed; the records describe the standard sequential layout this drive
    /// already uses, so the cylinder/head address is taken from CH/DH and every
    /// sector of that track is filled with the DOS format filler 0xF6. ponytail:
    /// the address-field records are not parsed for nonstandard interleave or sector
    /// sizes; the in-memory image is a fixed-geometry linear array, so a track is
    /// formatted by zero-fill of its sectors at the mounted geometry.
    fn int13_format_track(&mut self, dl: u8) {
        // No fixed-disk path: any hard-disk unit reports no such drive.
        if dl >= 0x80 {
            self.set_eax_ah(0x80);
            self.set_disk_status(0x80);
            self.set_int_frame_carry(true);
            return;
        }
        let Some(geom) = self.floppy.as_ref().map(|f| f.geometry()) else {
            self.set_eax_ah(0x80);
            self.set_disk_status(0x80);
            self.set_int_frame_carry(true);
            return;
        };
        // Only floppy A: is backed.
        if dl != 0x00 {
            self.set_eax_ah(0x80);
            self.set_disk_status(0x80);
            self.set_int_frame_carry(true);
            return;
        }
        let al = self.cpu.registers.eax() as u8;
        let cx = self.cpu.registers.ecx() as u16;
        let ch = (cx >> 8) as u8;
        let cl = cx as u8;
        let cyl = u16::from(ch) | (u16::from(cl & 0xc0) << 2);
        let head = (self.cpu.registers.edx() as u16 >> 8) as u8;
        // A track off the mounted media, or a sector count past the media's
        // sectors-per-track, is a bad-track request (AH=0Ch).
        if cyl >= geom.cylinders || head >= geom.heads || al > geom.sectors {
            self.set_eax_ah(0x0c);
            self.set_disk_status(0x0c);
            self.set_int_frame_carry(true);
            return;
        }
        self.floppy_accesses += 1;
        let ok = self
            .floppy
            .as_mut()
            .map(|f| f.format_track(cyl, head, 0xf6))
            .unwrap_or(false);
        // Charge the seek to the formatted track plus a full-track write.
        let bytes = usize::from(geom.sectors) * 512;
        let secs = self
            .floppy
            .as_mut()
            .map_or(0.0, |f| f.access_duration_secs(cyl, bytes));
        self.stall_for(secs);
        if ok {
            self.set_eax_ah(0x00);
            self.set_disk_status(0x00);
            self.set_int_frame_carry(false);
        } else {
            self.set_eax_ah(0x0c);
            self.set_disk_status(0x0c);
            self.set_int_frame_carry(true);
        }
    }

    /// Carry out the AH=02 read / AH=03 write half of INT 13h.
    fn int13_transfer(&mut self, ah: u8, dl: u8) {
        let Some(geom) = self.floppy.as_ref().map(|f| f.geometry()) else {
            // No media backs the request: report a timeout the way an empty
            // drive would.
            self.set_eax_ah(0x80);
            self.set_disk_status(0x80);
            self.set_int_frame_carry(true);
            return;
        };
        // Only floppy A: is backed.
        if dl != 0x00 {
            self.set_eax_ah(0x80);
            self.set_disk_status(0x80);
            self.set_int_frame_carry(true);
            return;
        }
        let _ = geom;

        let ax = self.cpu.registers.eax() as u16;
        let count = ax as u8;
        let cx = self.cpu.registers.ecx() as u16;
        let cl = cx as u8;
        let ch = (cx >> 8) as u8;
        let sector = cl & 0x3f;
        let cyl = u16::from(ch) | (u16::from(cl & 0xc0) << 2);
        let head = (self.cpu.registers.edx() as u16 >> 8) as u8;
        let es = self.cpu.registers.segment(SegmentIndex::Es).base;
        let bx = self.cpu.registers.ebx() as u16;
        let buffer = es.wrapping_add(u32::from(bx));

        // The drive is being serviced: flash the GUI access LED.
        self.floppy_accesses += 1;

        let mut done: u8 = 0;
        for i in 0..count {
            // Multi-sector transfers advance within the current track only. A
            // booter that crosses a track boundary in one call would need
            // cross-track wrap added here.
            let sec = sector + i;
            let addr = buffer.wrapping_add(u32::from(i) * 512);
            if ah == 0x02 {
                let data = self
                    .floppy
                    .as_ref()
                    .and_then(|f| f.read_sector(cyl, head, sec))
                    .map(<[u8]>::to_vec);
                match data {
                    Some(bytes) => self.write_guest_block(addr, &bytes),
                    None => break,
                }
            } else {
                let bytes = self.read_guest_block(addr, 512);
                let wrote = self
                    .floppy
                    .as_mut()
                    .map(|f| f.write_sector(cyl, head, sec, &bytes))
                    .unwrap_or(false);
                if !wrote {
                    break;
                }
            }
            done += 1;
        }

        // Charge the drive's mechanical time for the access: seek from the head's
        // tracked position, rotational latency, and the transfer of the sectors
        // moved. This is what makes a load take wall-clock time (see stall_clocks)
        // instead of completing instantly.
        if done > 0 {
            let bytes = usize::from(done) * 512;
            let secs = self
                .floppy
                .as_mut()
                .map_or(0.0, |f| f.access_duration_secs(cyl, bytes));
            self.stall_for(secs);
        }

        // AL returns the number of sectors actually transferred.
        self.set_eax_al(done);
        if done == count {
            self.set_eax_ah(0x00);
            self.set_disk_status(0x00);
            self.set_int_frame_carry(false);
        } else {
            // Sector not found / read error.
            self.set_eax_ah(0x04);
            self.set_disk_status(0x04);
            self.set_int_frame_carry(true);
        }
    }

    /// Carry out the AH=08 read-drive-parameters half of INT 13h.
    fn int13_drive_parameters(&mut self, dl: u8) {
        let Some(geom) = self.floppy.as_ref().map(|f| f.geometry()) else {
            self.set_eax_ah(0x80);
            self.set_disk_status(0x80);
            self.set_int_frame_carry(true);
            return;
        };
        if dl != 0x00 {
            self.set_eax_ah(0x80);
            self.set_disk_status(0x80);
            self.set_int_frame_carry(true);
            return;
        }
        let max_cyl = geom.cylinders.saturating_sub(1);
        // CL: sectors per track in bits 0-5, cylinder high bits in 6-7.
        let cl = (geom.sectors & 0x3f) | (((max_cyl >> 8) as u8 & 0x03) << 6);
        let ch = (max_cyl & 0xff) as u8;
        let cx = (u16::from(ch) << 8) | u16::from(cl);
        let ecx = (self.cpu.registers.ecx() & !0xFFFF) | u32::from(cx);
        self.cpu.registers.set_ecx(ecx);
        // DH = max head index, DL = number of floppy drives, read from the
        // equipment word so it tracks the mounted drives rather than a fixed 1.
        let dx = (u16::from(geom.heads.saturating_sub(1)) << 8) | u16::from(self.floppy_count());
        let edx = (self.cpu.registers.edx() & !0xFFFF) | u32::from(dx);
        self.cpu.registers.set_edx(edx);
        // BL = drive type (0x03 = 720 KB, 0x04 = 1.44 MB).
        let ebx = (self.cpu.registers.ebx() & !0xFF) | u32::from(geom.drive_type);
        self.cpu.registers.set_ebx(ebx);
        self.set_eax_ah(0x00);
        self.set_disk_status(0x00);
        self.set_int_frame_carry(false);
    }

    /// INT 13h fixed-disk path (DL>=0x80). Only the first drive (0x80 = C:) is
    /// backed; any other unit reports no-such-drive. Status follows the AT BIOS
    /// convention: AH = result code (0 success), CF set on error. EDD AH=41h-48h
    /// extends this for LBA access.
    fn int13_hdd(&mut self, ah: u8, dl: u8) {
        // Only unit 0x80 is wired. A higher unit, or no mounted disk, reports
        // invalid-drive (AH=0x01) with carry set, the way a BIOS does for an
        // absent fixed disk. The EDD install check (AH=41h) on an absent drive
        // also lands here through the default arm.
        if dl != 0x80 || self.ata.is_none() {
            self.set_eax_ah(0x01);
            self.set_disk_status(0x01);
            self.set_int_frame_carry(true);
            return;
        }
        match ah {
            // AH=00 reset disk system: a no-op success on this model (no real
            // recalibrate cost is charged for the hard disk).
            0x00 => self.int13_hdd_ok(),
            // AH=01 get last status from BDA 0040:0074 (the fixed-disk status byte).
            0x01 => {
                let status = self.read_physical_u8(0x474);
                self.set_eax_ah(status);
                self.set_int_frame_carry(status != 0);
            }
            // AH=02 read, AH=03 write sectors via CHS.
            0x02 | 0x03 => self.int13_hdd_transfer(ah),
            // AH=04 verify: confirm the run is in range without copying.
            0x04 => self.int13_hdd_verify(),
            // AH=08 get drive parameters: CHS geometry packed into CX/DH, fixed-
            // disk count in DL.
            0x08 => self.int13_hdd_parameters(),
            // AH=09 init drive pair, AH=0C seek, AH=0D alternate reset, AH=11
            // recalibrate: all succeed with no data movement on this model.
            0x09 | 0x0C | 0x0D | 0x11 => self.int13_hdd_ok(),
            // AH=10 test drive ready, AH=14 controller diagnostic: ready/OK.
            0x10 | 0x14 => self.int13_hdd_ok(),
            // AH=15 get DASD type: AH=03 (fixed disk), and the total sector count
            // in CX:DX.
            0x15 => self.int13_hdd_dasd(),
            // EDD install check.
            0x41 => self.int13_edd_install_check(),
            // EDD extended read/write via a Disk Address Packet at DS:SI.
            0x42 | 0x43 => self.int13_edd_transfer(ah),
            // EDD get extended drive parameters into a result buffer at DS:SI.
            0x48 => self.int13_edd_drive_params(),
            // Genuinely unknown subfunctions report invalid-function.
            _ => {
                self.set_eax_ah(0x01);
                self.set_fixed_disk_status(0x01);
                self.set_int_frame_carry(true);
            }
        }
    }

    /// Record the fixed-disk INT 13h result in BDA 0040:0074 so AH=01h can report
    /// it. Floppies use 0040:0041; the hard disk has its own status byte.
    fn set_fixed_disk_status(&mut self, status: u8) {
        let _ = self.memory.write_u8(0x474, status);
    }

    /// Common success for a fixed-disk control call: AH=0, status byte 0, CF clear.
    fn int13_hdd_ok(&mut self) {
        self.set_eax_ah(0x00);
        self.set_fixed_disk_status(0x00);
        self.set_int_frame_carry(false);
    }

    /// Read the CHS address out of the INT 13h register layout: CH = cylinder low
    /// 8 bits, CL bits 6-7 = cylinder high 2 bits, CL bits 0-5 = sector (1-based),
    /// DH = head.
    fn int13_chs(&self) -> (u32, u32, u32) {
        let cx = self.cpu.registers.ecx() as u16;
        let cl = cx as u8;
        let ch = (cx >> 8) as u8;
        let sector = u32::from(cl & 0x3f);
        let cyl = u32::from(ch) | (u32::from(cl & 0xc0) << 2);
        let head = u32::from((self.cpu.registers.edx() as u16 >> 8) as u8);
        (cyl, head, sector)
    }

    /// AH=02/03 CHS read/write against the mounted hard disk. ES:BX is the buffer;
    /// AL is the sector count. AL returns the count actually moved.
    fn int13_hdd_transfer(&mut self, ah: u8) {
        let count = self.cpu.registers.eax() as u8;
        let (cyl, head, sector) = self.int13_chs();
        let es = self.cpu.registers.segment(SegmentIndex::Es).base;
        let bx = self.cpu.registers.ebx() as u16;
        let buffer = es.wrapping_add(u32::from(bx));

        let Some(start_lba) = self
            .ata
            .as_ref()
            .and_then(|d| d.chs_to_lba(cyl, head, sector))
        else {
            // Address off the disk: sector-not-found (AH=0x04), CF set.
            self.set_eax_al(0);
            self.set_eax_ah(0x04);
            self.set_fixed_disk_status(0x04);
            self.set_int_frame_carry(true);
            return;
        };
        self.c_accesses += 1;
        let mut done: u8 = 0;
        for i in 0..count {
            let lba = start_lba + u32::from(i);
            let addr = buffer.wrapping_add(u32::from(i) * 512);
            if ah == 0x02 {
                let data = self
                    .ata
                    .as_ref()
                    .and_then(|d| d.read_lba(lba))
                    .map(<[u8]>::to_vec);
                match data {
                    Some(bytes) => self.write_guest_block(addr, &bytes),
                    None => break,
                }
            } else {
                let bytes = self.read_guest_block(addr, 512);
                let wrote = self
                    .ata
                    .as_mut()
                    .map(|d| d.write_lba(lba, &bytes))
                    .unwrap_or(false);
                if !wrote {
                    break;
                }
            }
            done += 1;
        }
        self.set_eax_al(done);
        if done == count {
            self.set_eax_ah(0x00);
            self.set_fixed_disk_status(0x00);
            self.set_int_frame_carry(false);
        } else {
            self.set_eax_ah(0x04);
            self.set_fixed_disk_status(0x04);
            self.set_int_frame_carry(true);
        }
    }

    /// AH=04 verify: confirm the run is readable without copying it. AL returns
    /// the count verified.
    fn int13_hdd_verify(&mut self) {
        let count = self.cpu.registers.eax() as u8;
        let (cyl, head, sector) = self.int13_chs();
        let Some(start_lba) = self
            .ata
            .as_ref()
            .and_then(|d| d.chs_to_lba(cyl, head, sector))
        else {
            self.set_eax_al(0);
            self.set_eax_ah(0x04);
            self.set_fixed_disk_status(0x04);
            self.set_int_frame_carry(true);
            return;
        };
        let total = self.ata.as_ref().map_or(0, |d| d.total_sectors());
        let mut done: u8 = 0;
        for i in 0..count {
            if start_lba + u32::from(i) >= total {
                break;
            }
            done += 1;
        }
        self.set_eax_al(done);
        if done == count {
            self.set_eax_ah(0x00);
            self.set_fixed_disk_status(0x00);
            self.set_int_frame_carry(false);
        } else {
            self.set_eax_ah(0x04);
            self.set_fixed_disk_status(0x04);
            self.set_int_frame_carry(true);
        }
    }

    /// AH=08 get drive parameters. CH = cylinder low 8 bits, CL bits 6-7 =
    /// cylinder high 2 bits, CL bits 0-5 = max sector; DH = max head index; DL =
    /// number of fixed disks; BL = drive type. The reported maximum cylinder is
    /// the count minus one, the way a BIOS hands back the last valid index.
    fn int13_hdd_parameters(&mut self) {
        let Some(disk) = self.ata.as_ref() else {
            self.set_eax_ah(0x01);
            self.set_int_frame_carry(true);
            return;
        };
        // BIOS caps the reported cylinders at 1024 (the 10-bit CHS field), so a
        // large disk's geometry stays addressable through the legacy path even
        // though the full capacity needs LBA.
        let max_cyl = disk.cylinders().min(1024).saturating_sub(1) as u16;
        let max_head = (disk.heads().saturating_sub(1)) as u8;
        let sectors = disk.sectors_per_track() as u8;
        let cl = (sectors & 0x3f) | (((max_cyl >> 8) as u8 & 0x03) << 6);
        let ch = (max_cyl & 0xff) as u8;
        let cx = (u16::from(ch) << 8) | u16::from(cl);
        let ecx = (self.cpu.registers.ecx() & !0xFFFF) | u32::from(cx);
        self.cpu.registers.set_ecx(ecx);
        // DH = max head index, DL = number of fixed disks (1). BL drive type 0 for
        // a fixed disk (the type byte is floppy-only; hard disks report 0).
        let dx = (u16::from(max_head) << 8) | 0x0001;
        let edx = (self.cpu.registers.edx() & !0xFFFF) | u32::from(dx);
        self.cpu.registers.set_edx(edx);
        let ebx = self.cpu.registers.ebx() & !0xFF;
        self.cpu.registers.set_ebx(ebx);
        self.set_eax_ah(0x00);
        self.set_fixed_disk_status(0x00);
        self.set_int_frame_carry(false);
    }

    /// AH=15 get DASD type: AH=03 marks a fixed disk, and CX:DX carries the total
    /// sector count (CX high word, DX low word). CF clear.
    fn int13_hdd_dasd(&mut self) {
        let total = self.ata.as_ref().map_or(0, |d| d.total_sectors());
        let cx = (total >> 16) as u16;
        let dx = total as u16;
        self.set_cx(cx);
        self.set_dx(dx);
        self.set_eax_ah(0x03); // fixed disk present
        self.set_int_frame_carry(false);
    }

    /// EDD AH=41h install check. On a present drive: carry clear, BX=0xAA55, AH=
    /// 0x30 (EDD version 3.0), CX bit 0 set (extended disk access supported). The
    /// 0xAA55 in BX is the magic a caller checks to confirm the extensions exist.
    fn int13_edd_install_check(&mut self) {
        self.set_bx(0xAA55);
        self.set_eax_ah(0x30); // version 3.0
        self.set_cx(0x0001); // extended disk access support
        self.set_int_frame_carry(false);
    }

    /// EDD AH=42h/43h extended read/write. The Disk Address Packet at DS:SI holds
    /// the block count and the 64-bit starting LBA; the transfer buffer is a
    /// seg:off far pointer inside the packet. Only the low 32 bits of the LBA are
    /// honored. ponytail: the 64-bit-flat-buffer form (DAP bytes 16-23 when the
    /// seg:off is 0xFFFF:0xFFFF) is not decoded; lift by reading the wide pointer.
    fn int13_edd_transfer(&mut self, ah: u8) {
        let ds = self.cpu.registers.segment(SegmentIndex::Ds).base;
        let si = self.cpu.registers.esi() as u16;
        let dap = ds.wrapping_add(u32::from(si));
        let packet = self.read_guest_block(dap, 16);
        // Byte 0 = packet size (16 or 24). Byte 2 = block count. Bytes 4-7 = the
        // transfer buffer as offset (4-5) then segment (6-7). Bytes 8-15 = the
        // starting LBA, little-endian.
        let count = u16::from_le_bytes([packet[2], packet[3]]);
        let buf_off = u16::from_le_bytes([packet[4], packet[5]]);
        let buf_seg = u16::from_le_bytes([packet[6], packet[7]]);
        let lba = u32::from_le_bytes([packet[8], packet[9], packet[10], packet[11]]);
        let buffer = (u32::from(buf_seg) << 4).wrapping_add(u32::from(buf_off));

        let total = self.ata.as_ref().map_or(0, |d| d.total_sectors());
        if lba.saturating_add(u32::from(count)) > total {
            // Out of range: AH=0x04 (sector not found), CF set, and the DAP block
            // count is rewritten to the number actually transferred (zero here).
            self.set_dap_blocks(dap, 0);
            self.set_eax_ah(0x04);
            self.set_fixed_disk_status(0x04);
            self.set_int_frame_carry(true);
            return;
        }
        self.c_accesses += 1;
        let mut done: u16 = 0;
        for i in 0..count {
            let l = lba + u32::from(i);
            let addr = buffer.wrapping_add(u32::from(i) * 512);
            if ah == 0x42 {
                let data = self
                    .ata
                    .as_ref()
                    .and_then(|d| d.read_lba(l))
                    .map(<[u8]>::to_vec);
                match data {
                    Some(bytes) => self.write_guest_block(addr, &bytes),
                    None => break,
                }
            } else {
                let bytes = self.read_guest_block(addr, 512);
                let wrote = self
                    .ata
                    .as_mut()
                    .map(|d| d.write_lba(l, &bytes))
                    .unwrap_or(false);
                if !wrote {
                    break;
                }
            }
            done += 1;
        }
        // EDD writes the count actually moved back into the DAP block-count field.
        self.set_dap_blocks(dap, done);
        if done == count {
            self.set_eax_ah(0x00);
            self.set_fixed_disk_status(0x00);
            self.set_int_frame_carry(false);
        } else {
            self.set_eax_ah(0x04);
            self.set_fixed_disk_status(0x04);
            self.set_int_frame_carry(true);
        }
    }

    /// Rewrite the Disk Address Packet block-count field (offset 2) with the
    /// sectors actually transferred, the way EDD reports partial completion.
    fn set_dap_blocks(&mut self, dap: u32, blocks: u16) {
        let bytes = blocks.to_le_bytes();
        self.write_guest_block(dap + 2, &bytes);
    }

    /// EDD AH=48h get extended drive parameters. The result buffer at DS:SI takes
    /// the EDD 1.x layout: word 0 = buffer size, word 2 = info flags, dwords for
    /// the CHS geometry, qword 16 = total sectors, word 24 = bytes per sector.
    fn int13_edd_drive_params(&mut self) {
        let Some(disk) = self.ata.as_ref() else {
            self.set_eax_ah(0x01);
            self.set_int_frame_carry(true);
            return;
        };
        let total = u64::from(disk.total_sectors());
        let cylinders = disk.cylinders();
        let heads = disk.heads();
        let spt = disk.sectors_per_track();

        let ds = self.cpu.registers.segment(SegmentIndex::Ds).base;
        let si = self.cpu.registers.esi() as u16;
        let dst = ds.wrapping_add(u32::from(si));

        let mut buf = [0u8; 26];
        buf[0..2].copy_from_slice(&26u16.to_le_bytes()); // buffer size (EDD 1.x)
        buf[2..4].copy_from_slice(&0x0002u16.to_le_bytes()); // info: geometry valid
        buf[4..8].copy_from_slice(&cylinders.to_le_bytes()); // physical cylinders
        buf[8..12].copy_from_slice(&heads.to_le_bytes()); // physical heads
        buf[12..16].copy_from_slice(&spt.to_le_bytes()); // sectors per track
        buf[16..24].copy_from_slice(&total.to_le_bytes()); // total sectors (qword)
        buf[24..26].copy_from_slice(&(ata::SECTOR as u16).to_le_bytes()); // bytes/sector
        self.write_guest_block(dst, &buf);
        self.set_eax_ah(0x00);
        self.set_fixed_disk_status(0x00);
        self.set_int_frame_carry(false);
    }

    /// Number of floppy drives the BDA equipment word advertises (0040:0010): bit 0
    /// is the floppy-installed flag, bits 7-6 are the drive count minus one. INT 13h
    /// AH=08h reports this in DL so it tracks the mounted drives.
    fn floppy_count(&self) -> u8 {
        let word = self.memory.read_u16(0x410).unwrap_or(BIOS_EQUIPMENT_WORD);
        if word & 0x0001 == 0 {
            0
        } else {
            ((word >> 6) & 0x03) as u8 + 1
        }
    }

    /// Replace AH in EAX, leaving AL and the upper 16 bits intact.
    fn set_eax_ah(&mut self, ah: u8) {
        let eax = (self.cpu.registers.eax() & !0xFF00) | (u32::from(ah) << 8);
        self.cpu.registers.set_eax(eax);
    }

    /// Replace AL in EAX, leaving AH and the upper 16 bits intact.
    fn set_eax_al(&mut self, al: u8) {
        let eax = (self.cpu.registers.eax() & !0xFF) | u32::from(al);
        self.cpu.registers.set_eax(eax);
    }

    /// Set or clear CF in the FLAGS image the pending IRET stub will pop (SS:SP+4
    /// after a real-mode INT pushed IP, CS, FLAGS). Host-serviced INTs that report
    /// status through carry use this so the guest sees the right flag on return.
    fn set_int_frame_carry(&mut self, carry: bool) {
        let ss = self.cpu.registers.segment(SegmentIndex::Ss).base;
        let sp = self.cpu.registers.esp() as u16;
        let flags_addr = (ss + u32::from(sp.wrapping_add(4))) as usize;
        if let Ok(mut flags) = self.memory.read_u16(flags_addr) {
            if carry {
                flags |= 0x0001;
            } else {
                flags &= !0x0001;
            }
            let _ = self.memory.write_u16(flags_addr, flags);
        }
    }

    /// INT 10h AH=10h: set/get the ATC palette registers and the DAC. Covers the
    /// set/get forms for the attribute palette (00/01/02/07/08/09) and the DAC
    /// (10/12/13/15/17/1A/1B). Register conventions per RBIL (INT 10/AH=10h).
    /// ponytail: the attribute-controller mode bits behind a few sub-functions
    /// (AL=03 blink/intensity toggle, AL=13h color-page select) have no public
    /// setter on the video core, so they are accepted as no-ops with CF clear; the
    /// DAC paging state (AL=1Ah) reports the power-up default (mode 0, page 0).
    fn handle_int10_palette(&mut self, al: u8) {
        let bx = self.cpu.registers.ebx() as u16;
        let bl = bx as u8;
        let bh = (bx >> 8) as u8;
        let cx = self.cpu.registers.ecx() as u16;
        let ch = (cx >> 8) as u8;
        let cl = cx as u8;
        let dx = self.cpu.registers.edx() as u16;
        let dh = (dx >> 8) as u8;
        let es_base = self.cpu.registers.segment(SegmentIndex::Es).base;
        let es_dx = es_base + u32::from(dx);
        match al {
            // AL=00: set individual palette register. BL=index (0-15), BH=value.
            0x00 => self.video.set_attr_palette_reg(bl, bh),
            // AL=01: set overscan/border color. BH=value (overlap with AH=0Bh).
            0x01 => self.video.set_overscan(bh),
            // AL=02: set all 16 palette registers and overscan from ES:DX (17 bytes).
            0x02 => {
                let block = self.read_guest_block(es_dx, 17);
                for i in 0..16u8 {
                    self.video.set_attr_palette_reg(i, block[i as usize]);
                }
                self.video.set_overscan(block[16]);
            }
            // AL=03: toggle intensify/blink (BL bit0). ponytail: the attribute mode
            // control bit 3 has no public setter on the video core, so this is a
            // no-op; CF stays clear so the caller sees the call succeed.
            0x03 => {}
            // AL=07: get individual palette register. BL=index -> BH.
            0x07 => {
                let value = self.video.attr_palette_reg(bl);
                let ebx = (self.cpu.registers.ebx() & !0xFF00) | (u32::from(value) << 8);
                self.cpu.registers.set_ebx(ebx);
            }
            // AL=08: read overscan/border color -> BH.
            0x08 => {
                let value = self.video.overscan();
                let ebx = (self.cpu.registers.ebx() & !0xFF00) | (u32::from(value) << 8);
                self.cpu.registers.set_ebx(ebx);
            }
            // AL=09: read all 16 palette registers + overscan into ES:DX (17 bytes).
            0x09 => {
                let mut block = [0u8; 17];
                for (i, slot) in block.iter_mut().take(16).enumerate() {
                    *slot = self.video.attr_palette_reg(i as u8);
                }
                block[16] = self.video.overscan();
                self.write_guest_block(es_dx, &block);
            }
            // AL=10: set individual DAC register. BX=index, DH=R, CH=G, CL=B.
            0x10 => self.video.set_dac_entry(bx as u8, dh, ch, cl),
            // AL=12: set a block of DAC registers. BX=start, CX=count, ES:DX -> RGB triples.
            0x12 => {
                let bytes = self.read_guest_block(es_dx, cx as usize * 3);
                let entries: Vec<[u8; 3]> =
                    bytes.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
                self.video.set_dac_block(bx as u8, &entries);
            }
            // AL=13: select color page / paging mode (BL=0 select mode in BH, BL=1
            // select page in BH). ponytail: the attribute color-select datapath has
            // no public setter, so this is a no-op; CF stays clear.
            0x13 => {}
            // AL=15: get individual DAC register. BX=index -> DH=R, CH=G, CL=B.
            0x15 => {
                let [r, g, b] = self.video.dac_entry(bx as u8);
                let edx = (self.cpu.registers.edx() & !0xFF00) | (u32::from(r) << 8);
                self.cpu.registers.set_edx(edx);
                let ecx_new =
                    (self.cpu.registers.ecx() & !0xFFFF) | (u32::from(g) << 8) | u32::from(b);
                self.cpu.registers.set_ecx(ecx_new);
            }
            // AL=17: get a block of DAC registers. BX=start, CX=count -> ES:DX.
            0x17 => {
                let bytes = self.video.dac_block_bytes(bx as u8, cx);
                self.write_guest_block(es_dx, &bytes);
            }
            // AL=1A: read DAC page state -> BL=paging mode, BH=current page.
            // ponytail: color paging is not modeled, so the power-up default is
            // reported (mode 0 = four pages of 64, page 0).
            0x1A => {
                let ebx = self.cpu.registers.ebx() & !0xFFFF; // BL=0, BH=0
                self.cpu.registers.set_ebx(ebx);
            }
            // AL=1B: sum a block of DAC registers to gray scale. BX=start, CX=count.
            // The NTSC luma weights (30% R, 59% G, 11% B) collapse each entry to a
            // single gray level, the way the BIOS gray-scale-summing routine does.
            0x1B => {
                let start = bx as u8;
                for offset in 0..cx {
                    let index = start.wrapping_add(offset as u8);
                    let [r, g, b] = self.video.dac_entry(index);
                    let gray =
                        ((u16::from(r) * 77 + u16::from(g) * 151 + u16::from(b) * 28) >> 8) as u8;
                    self.video.set_dac_entry(index, gray, gray, gray);
                }
            }
            _ => {}
        }
    }

    /// INT 10h AH=11h: the character-generator font services (RBIL). AL=00/10
    /// loads a user font at ES:BP (CX glyphs, DX first char, BH bytes/char, BL
    /// block); AL=01/11, 02/12, 04/14 load the ROM 8x14, 8x8, 8x16 fonts (BL
    /// block); AL=03 sets the block specifier (BL -> Sequencer index 3). The 1x
    /// variants also reprogram the CRTC character height. AL=30 (get font info)
    /// and the graphics-mode text services are deferred. Register conventions
    /// verified against the LGPL VGABios `biosfn_load_text_*`.
    fn handle_int10_font(&mut self, al: u8) {
        let bx = self.cpu.registers.ebx() as u16;
        let bl = bx as u8;
        let bh = (bx >> 8) as u8;
        let cx = self.cpu.registers.ecx() as u16;
        let dx = self.cpu.registers.edx() as u16;
        let table = self.video.char_map_table(bl);
        match al {
            0x00 | 0x10 => {
                let bp = self.cpu.registers.ebp() as u16;
                let es_base = self.cpu.registers.segment(SegmentIndex::Es).base;
                // load_font_table folds character codes modulo 256, so any
                // glyphs beyond the first 256 only rewrite earlier codes. Cap
                // the read there to keep a pathological CX (a u16 up to 65535)
                // from stalling the emulator with up to ~16 million
                // byte-at-a-time bus reads plus a multi-megabyte allocation.
                let count = (cx as usize).min(256);
                let bytes = self.read_guest_block(es_base + u32::from(bp), count * bh as usize);
                self.video.load_font_table(table, dx, bh, &bytes);
                if al >= 0x10 {
                    self.video.set_char_height(bh);
                }
            }
            0x01 | 0x11 => {
                self.video.load_rom_font(table, 14);
                if al >= 0x10 {
                    self.video.set_char_height(14);
                }
            }
            0x02 | 0x12 => {
                self.video.load_rom_font(table, 8);
                if al >= 0x10 {
                    self.video.set_char_height(8);
                }
            }
            0x04 | 0x14 => {
                self.video.load_rom_font(table, 16);
                if al >= 0x10 {
                    self.video.set_char_height(16);
                }
            }
            0x03 => self.video.set_char_map_select(bl),
            _ => {}
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
        // C: is the only mounted host drive, so a DOS file-I/O call is a C:
        // access for the GUI LED: open/create/close/read/write/seek/find.
        if vector == 0x21 {
            let ah = (regs.ax >> 8) as u8;
            if matches!(ah, 0x3C | 0x3D | 0x3E | 0x3F | 0x40 | 0x42 | 0x4E | 0x4F) {
                self.c_accesses += 1;
            }
        }
        let action = self.dos.dispatch(vector, &mut regs, &mut self.memory)?;
        if matches!(action, izarravm_dos::DosAction::WaitForKey) {
            // Blocking read with an empty ring. Rewind the stacked return IP by 2
            // so the IRET stub re-enters the INT 21h (CD 21), and set IF in the
            // stacked FLAGS so IRQ1 can run the keyboard ISR before the retry.
            let ss = self.cpu.registers.segment(SegmentIndex::Ss).base;
            let sp = self.cpu.registers.esp() as u16;
            let ip_addr = (ss + u32::from(sp)) as usize;
            let flags_addr = (ss + u32::from(sp.wrapping_add(4))) as usize;
            let ret_ip = self.memory.read_u16(ip_addr)?;
            self.memory.write_u16(ip_addr, ret_ip.wrapping_sub(2))?;
            let mut flags = self.memory.read_u16(flags_addr)?;
            flags |= 0x0200; // IF
            self.memory.write_u16(flags_addr, flags)?;
            return Ok(None);
        }
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
            izarravm_dos::DosAction::Exec { entry, child_ax } => {
                // Snapshot the parent and switch to the child. The kernel has
                // already saved its per-program state; we save the CPU side.
                self.program_frames.push(ProgramFrame {
                    registers: self.cpu.registers.clone(),
                });
                self.apply_program_entry(entry);
                // Only AX is defined on child entry (FCB drive validity); the
                // other GPRs are undefined, matching real DOS (marked).
                self.cpu.registers.set_eax(u32::from(child_ax));
                None // keep looping; the CPU now runs the child
            }
            // Handled above with an early return; kept so the match stays exhaustive.
            izarravm_dos::DosAction::WaitForKey => None,
        })
    }

    /// The bytes the DOS kernel has written to standard output (INT 21h AH=09h and
    /// the character-output calls). Captured host-side for headless runs; not yet
    /// rendered to the VGA text mode.
    pub fn dos_output(&self) -> &[u8] {
        self.dos.stdout()
    }

    /// Seed the BDA keyboard ring with input bytes for the character-input calls.
    /// An empty ring blocks the reads (AH=01h/08h) until the keyboard ISR refills
    /// it; AH=06h reports an empty ring through ZF. Holds up to 15 bytes.
    pub fn set_dos_stdin(&mut self, bytes: &[u8]) {
        let _ = izarravm_dos::seed_keyboard_ring(&mut self.memory, bytes);
    }

    /// Mount a host directory as the guest C: drive for INT 21h file calls.
    pub fn mount_c_drive(&mut self, drive: izarravm_dos::HostDrive) {
        self.dos.mount_c(drive);
    }

    /// Tell the machine where the host C: drive lives so the BIOS Repair and
    /// Format service-port commands can lay Toka-DOS down on it.
    pub fn set_toka_c_root(&mut self, root: std::path::PathBuf) {
        self.toka_c_root = Some(root);
    }

    /// Perform a Toka-DOS service requested through Lotura port 0xE3, recording
    /// the status the BIOS reads back. Called from the run loop after the cycle
    /// that issued the OUT, since the work needs host filesystem and memory.
    fn perform_toka_service(&mut self, command: u8) {
        self.toka_service_status = match command {
            0x01 => self.toka_install_files(izarravm_dos::InstallMode::Repair),
            0x02 => self.toka_install_files(izarravm_dos::InstallMode::Format),
            0x10 => self.toka_load_boot_record(),
            _ => 0xff,
        };
    }

    fn toka_install_files(&mut self, mode: izarravm_dos::InstallMode) -> u8 {
        let Some(root) = self.toka_c_root.clone() else {
            return 1; // no C: root known
        };
        let files = izarravm_firmware::toka_dos_system_files();
        match izarravm_dos::toka_dos_install(&root, &files, mode) {
            Ok(()) => 0,
            Err(_) => 0xfe,
        }
    }

    /// Place the Toka-DOS boot record (TOKABOOT) at 0x7C00 and set up the DOS
    /// base context so the boot record's EXEC of ICOMMAND.COM works. The BIOS
    /// then jumps to 0x7C00 like a real INT 19h boot.
    fn toka_load_boot_record(&mut self) -> u8 {
        // Toka-DOS is bootable only when it is installed on C: (ICOMMAND.COM
        // present). The boot record always lives in the ROM, so without this
        // check the machine would "boot" a drive that carries no OS.
        let installed = self
            .toka_c_root
            .as_ref()
            .is_some_and(|root| root.join("ICOMMAND.COM").exists());
        if !installed {
            return 1; // not installed: the BIOS reports and idles
        }
        let Some(boot) = izarravm_firmware::toka_boot_record() else {
            return 1; // ROM carries no boot record
        };
        let boot = boot.to_vec();
        for (offset, &byte) in boot.iter().enumerate() {
            if self
                .memory
                .write_u8(BOOT_SECTOR_ADDRESS + offset, byte)
                .is_err()
            {
                return 0xfe;
            }
        }
        if self.setup_toka_dos_base().is_err() {
            return 0xfe;
        }
        0
    }

    /// Stand up the DOS base context for a Toka-DOS boot: point the INT 20h/21h
    /// vectors at the RAM IRET stub the HLE kernel returns through (the real BIOS
    /// does not install these), then build a system PSP, arena, and base
    /// environment so the boot record's EXEC has a parent to inherit from. This
    /// is the SYSINIT-equivalent for the HLE kernel.
    fn setup_toka_dos_base(&mut self) -> Result<(), MachineError> {
        self.memory.write_u8(BIOS_IRET_STUB_ADDRESS, 0xcf)?;
        for vector in [0x20usize, 0x21] {
            self.memory
                .write_u16(vector * 4, BIOS_IRET_STUB_ADDRESS as u16)?;
            self.memory.write_u16(vector * 4 + 2, 0)?;
        }
        let env: [(&str, &str); 3] = [
            ("COMSPEC", "C:\\ICOMMAND.COM"),
            ("PATH", "C:\\;C:\\DOS"),
            ("PROMPT", "$p$g"),
        ];
        let Machine { dos, memory, .. } = self;
        dos.init_shell_base(memory, DOS_LOAD_SEGMENT, &env)?;
        Ok(())
    }

    /// Mirror any DOS console output produced since the last call onto the VGA
    /// text screen. DOS programs write CON through INT 21h, which the kernel
    /// buffers; real DOS renders that to the screen via the BIOS teletype. We do
    /// the same here so a Toka-DOS session is visible on the framebuffer, sharing
    /// the BDA cursor at 0040:0050 with the BIOS.
    fn flush_dos_console_to_screen(&mut self) {
        let total = self.dos_output().len();
        if self.dos_screen_shown >= total {
            return;
        }
        let pending: Vec<u8> = self.dos_output()[self.dos_screen_shown..].to_vec();
        self.dos_screen_shown = total;
        for byte in pending {
            self.teletype_char(byte);
        }
    }

    /// Write one character to the VGA text screen at the BDA cursor, advancing it
    /// with CR, LF, backspace, tab, and bottom-of-screen scroll, the way the BIOS
    /// teletype (INT 10h AH=0Eh) does. Attribute 0x07 is light grey on black.
    fn teletype_char(&mut self, byte: u8) {
        let cursor = self.memory.read_u16(0x450).unwrap_or(0);
        let mut col = usize::from(cursor & 0x00ff);
        let mut row = usize::from(cursor >> 8);
        match byte {
            b'\r' => col = 0,
            b'\n' => row += 1,
            0x08 => col = col.saturating_sub(1), // backspace
            0x07 => {}                           // bell: no visible effect
            b'\t' => {
                col = (col + 8) & !7;
                if col >= 80 {
                    col = 0;
                    row += 1;
                }
            }
            _ => {
                let offset = (row * 80 + col) * 2;
                let _ = self.video.write_u8(offset, byte);
                let _ = self.video.write_u8(offset + 1, 0x07);
                col += 1;
                if col >= 80 {
                    col = 0;
                    row += 1;
                }
            }
        }
        while row >= 25 {
            self.scroll_text_up();
            row -= 1;
        }
        let _ = self
            .memory
            .write_u16(0x450, ((row as u16) << 8) | col as u16);
        // Track the visible hardware cursor (CRTC 0E/0Fh) with the BDA cursor, the
        // way the BIOS teletype does, so it sits where the next char lands.
        self.video.set_cursor_offset((row * 80 + col) as u16);
    }

    /// Scroll the 80x25 text screen up one line, clearing the bottom row to
    /// spaces with the normal attribute.
    fn scroll_text_up(&mut self) {
        const ROW_BYTES: usize = 80 * 2;
        for offset in 0..(24 * ROW_BYTES) {
            let byte = self.video.read_u8(offset + ROW_BYTES).unwrap_or(b' ');
            let _ = self.video.write_u8(offset, byte);
        }
        let last = 24 * ROW_BYTES;
        for col in 0..80 {
            let _ = self.video.write_u8(last + col * 2, b' ');
            let _ = self.video.write_u8(last + col * 2 + 1, 0x07);
        }
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

    fn read_guest_block(&mut self, addr: u32, len: usize) -> Vec<u8> {
        (0..len)
            .map(|index| self.read_physical_u8(addr + index as u32))
            .collect()
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
        // Every VGA mode (text, planar, mode X, mode 13h) now presents a raster
        // through the core; Margo's linear framebuffer is the only other path.
        if self.margo_active {
            ActiveDisplay::MargoLfb
        } else {
            ActiveDisplay::VgaRaster
        }
    }

    /// Emulated vertical refresh of the active display, in Hz. The host uses
    /// this to pace repaints to the guest's frame rate (mode 13h is ~70 Hz,
    /// mode 12h ~60 Hz). Clamped to a sane range so a CRTC reprogram caught
    /// mid-mode-set (a zero or absurd frame size) can't yield a degenerate
    /// repaint interval. Margo's linear framebuffer has no beam model, so it
    /// reports a plain 60 Hz.
    pub fn display_refresh_hz(&self) -> f64 {
        let hz = match self.active_display() {
            ActiveDisplay::VgaRaster => match self.video.frame_dots() {
                0 => 60.0,
                dots => VGA_DOT_HZ as f64 / dots as f64,
            },
            ActiveDisplay::MargoLfb => 60.0,
        };
        hz.clamp(50.0, 120.0)
    }

    pub fn vga_raster(&mut self) -> Option<VgaRaster> {
        self.video.last_presented().cloned()
    }

    pub fn palette_argb(&self) -> [u32; DAC_ENTRIES] {
        self.video.palette_argb()
    }

    /// The active display as native-resolution `0x00RRGGBB` words plus
    /// `(width, height)`. Mirrors the GUI's scanout so the unit tester's CRC and
    /// snapshot see exactly what is presented on screen.
    pub fn frame_argb(&mut self) -> (Vec<u32>, usize, usize) {
        let palette = self.palette_argb();
        match self.active_display() {
            ActiveDisplay::VgaRaster => match self.vga_raster() {
                Some(raster) => {
                    let words = raster
                        .pixels
                        .iter()
                        .map(|&index| palette[usize::from(index)])
                        .collect();
                    (words, raster.width as usize, raster.height as usize)
                }
                None => (vec![0], 1, 1),
            },
            ActiveDisplay::MargoLfb => {
                let display = self.margo.display();
                let (width, height) = (display.width as usize, display.height as usize);
                (self.margo.scanout_argb(&palette), width, height)
            }
        }
    }

    /// zlib/IEEE CRC-32 of a framebuffer rectangle, each pixel hashed as its four
    /// `0x00RRGGBB` bytes (little-endian). The rectangle is clamped to the frame;
    /// one fully outside it hashes nothing (CRC of empty input, 0). This is the
    /// value the unit tester returns at `REG_CRC`, and a handy Rust-side check
    /// for the boot suite.
    pub fn screen_crc32(&mut self, x: u16, y: u16, w: u16, h: u16) -> u32 {
        let (words, frame_w, frame_h) = self.frame_argb();
        let x = usize::from(x);
        let y = usize::from(y);
        let x_end = x.saturating_add(usize::from(w)).min(frame_w);
        let y_end = y.saturating_add(usize::from(h)).min(frame_h);
        let mut bytes = Vec::new();
        for row in y..y_end {
            for col in x..x_end {
                bytes.extend_from_slice(&words[row * frame_w + col].to_le_bytes());
            }
        }
        unittester::crc32(&bytes)
    }

    /// Set where the unit tester's Snapshot command writes PPM frames. `None`
    /// (the default) makes Snapshot a no-op. Each Snapshot overwrites this path.
    // ponytail: single path, overwrite. Add an index suffix if a test ever needs
    // to capture multiple frames in one run.
    pub fn set_test_snapshot_path(&mut self, path: Option<std::path::PathBuf>) {
        self.test_snapshot_path = path;
    }

    /// Set the HMA minimum-request threshold in KB, the `/HMAMIN=` HIMEM
    /// parameter. XMS function 01h then refuses a request below this size so the
    /// HMA stays free for a larger claimant (such as DOS=HIGH). The config wiring
    /// that parses the CONFIG.SYS HIMEM line will drive this.
    pub fn set_hma_min_kb(&mut self, kb: u16) {
        self.xms.set_hma_min_kb(kb);
    }

    /// Execute a unit-tester command deferred from a 0xE6 write. Returns the exit
    /// code for `CMD_EXIT` so the run loop can stop; `None` otherwise.
    fn perform_unittester(&mut self, cmd: u8) -> Option<u8> {
        match cmd {
            unittester::CMD_CRC => {
                let (x, y, w, h) = self.unittester.rect();
                let crc = self.screen_crc32(x, y, w, h);
                self.unittester.set_crc(crc);
                None
            }
            unittester::CMD_SNAPSHOT => {
                if let Some(path) = self.test_snapshot_path.clone() {
                    if let Err(err) = self.write_snapshot_ppm(&path) {
                        eprintln!("unit tester: snapshot to {} failed: {err}", path.display());
                    }
                }
                None
            }
            unittester::CMD_EXIT => Some(self.unittester.exit_code()),
            _ => None, // unknown command: ignore, like an unused port write
        }
    }

    /// Write the current frame to `path` as a binary PPM (P6). PPM keeps a PNG
    /// encoder out of the dependency tree for a baseline-capture convenience; any
    /// image viewer or `pnmtopng` opens it.
    fn write_snapshot_ppm(&mut self, path: &std::path::Path) -> std::io::Result<()> {
        use std::io::Write;
        let (words, width, height) = self.frame_argb();
        let mut out = Vec::with_capacity(width * height * 3 + 32);
        write!(out, "P6\n{width} {height}\n255\n")?;
        for &word in &words {
            out.push((word >> 16) as u8); // R
            out.push((word >> 8) as u8); // G
            out.push(word as u8); // B
        }
        std::fs::write(path, out)
    }

    pub fn bus_trace(&self) -> &BusTrace {
        &self.trace
    }

    pub fn elapsed_clocks(&self) -> u64 {
        self.elapsed_clocks
    }

    /// Cumulative guest clocks spent blocked on device I/O (floppy, later ATA)
    /// rather than executing instructions. A realtime host subtracts these from
    /// the clocks run when it gauges emulation speed, so a drive grind does not
    /// read as the emulator running fast.
    pub fn io_stall_clocks(&self) -> u64 {
        self.io_stall_clocks
    }

    /// Switch the active compatibility mode live, recomputing the timing factors
    /// for the new clock and lowering the CPU's guest-facing instruction-set level
    /// to match. Called from the Lotura mode write (port 0xE1). The CPU level gate
    /// is guest-facing only: firmware POST never reaches this path, so it always
    /// runs at the full ISA the core resets to.
    pub fn set_mode(&mut self, mode: GswMode) {
        self.active_mode = mode;
        self.timing = TimingFactors::for_clock(mode.clock_hz());
        self.cpu.set_level(cpu_level_for_mode(mode));
    }

    /// The reported (L1 KB, L2 KB) cache for the live mode. Cosmetic: it models a
    /// motherboard L2 cache module and feeds the BIOS setup and GUI readout only,
    /// with no timing effect. Driven from the live CPU level so it tracks a Lotura
    /// mode switch.
    pub fn cache_config(&self) -> (u16, u16) {
        self.cpu.cache_kb()
    }

    /// The live compatibility mode (set at boot, changed by a Lotura mode write).
    pub fn active_mode(&self) -> GswMode {
        self.active_mode
    }

    /// Advance time-based devices by `clocks` of CPU time, carrying fractional
    /// remainders forward for the OPL timers (microseconds), the PIT counters,
    /// and the Margo blit engine (nanoseconds).
    fn advance_devices(&mut self, clocks: u64) {
        self.opl_micros += clocks as f64 * self.timing.micros_per_clock;
        let whole = self.opl_micros.floor();
        self.opl.advance_micros(whole as u64);
        self.opl_micros -= whole;

        // The DSP reset-settle countdown advances with emulated time so a
        // detection routine's delay loop sees 0xAA become available.
        self.dsp_micros += clocks as f64 * self.timing.micros_per_clock;
        let whole = self.dsp_micros.floor();
        self.dsp.advance_micros(whole);
        self.dsp_micros -= whole;

        // DMA playback is clock-driven: accrue DSP sample phases per CPU clock
        // and, for each whole sample, advance the block and buffer the rendered
        // stereo frame onto the DSP ring. The half/end-buffer IRQ that
        // render_frame edges is forwarded to the PIC here, so playback timing and
        // IRQ5 no longer depend on the host frontend pulling audio. The host path
        // (render_dsp_audio) only drains what the clock already produced.
        // The mixer's SB Pro stereo bit (0x0E bit1) selects 8-bit byte
        // interleaving, which halves the per-channel frame rate; sample it before
        // computing the rate the DSP frames at.
        self.dsp.set_sbpro_stereo(self.mixer.sbpro_stereo());
        let rate = self.dsp.output_frame_rate();
        // The mixer selects the IRQ line and DMA channels (registers 0x80/0x81);
        // read them before the borrow-splitting loop below so the loop's
        // `let Machine { dsp, dma, memory, .. } = self;` shape is untouched.
        let irq_line = self.mixer.selected_irq();
        let dma8 = self.mixer.selected_dma_8();
        let dma16 = self.mixer.selected_dma_16();
        if self.dsp.is_playing() && rate > 0 {
            self.dsp_sample_phase += clocks as f64 * rate as f64 * self.timing.inv_clock;
            while self.dsp_sample_phase >= 1.0 {
                self.dsp_sample_phase -= 1.0;
                // Borrow dsp/dma/memory together for one sample tick (same shape
                // as render_dsp_audio). Only the fetcher matching the armed mode
                // is wired to the DMA channel, so a single &mut dma/&mut memory
                // borrow feeds the tick.
                let Machine {
                    dsp, dma, memory, ..
                } = self;
                if dsp.is_16bit() {
                    dsp.tick_sample(|| None, || dma.read_word(dma16, memory));
                } else {
                    dsp.tick_sample(|| dma.read_byte(dma8, memory), || None);
                }
            }
            if self.dsp.take_irq() {
                let is_16bit = self.dsp.is_16bit();
                self.mixer.set_irq_status(is_16bit);
                self.pic.request(irq_line);
            }
        }

        // AD1848 / Windows Sound System playback, clock-driven exactly like the
        // SB16 DSP above but on the codec's own base/IRQ/DMA -- no cross-talk with
        // the SB16's mixer-selected IRQ/DMA. The codec pulls 1/2/4 byte-wide DMA
        // reads per output frame internally (8/16-bit, mono/stereo), so a single
        // byte fetcher feeds tick_sample. advance_autocal retires the post-MCE ACI
        // window one output period per frame, and the terminal-count IRQ forwards
        // to the configured PIC line. Gated entirely on wss_enabled.
        if self.wss_enabled {
            let programmed_rate = self.wss.output_frame_rate();
            let autocal_active = self.wss.autocal_active();
            // The output sample clock paces both the DMA render and the autocal
            // (ACI) countdown. On real hardware the autocal converter clock retires
            // its ~128-sample window regardless of the *programmed* sample rate, so
            // when ACI is draining while I8 selects one of the two unsupported XTAL1
            // selects (rate_hz()==0) we fall back to the lowest documented WSS rate
            // (8000 Hz) just to clock the ACI countdown -- otherwise a guest that
            // clears MCE under an invalid rate would leave ACI asserted forever.
            // DMA render is still gated on a *valid* programmed rate below, so no
            // audio is produced at the fallback cadence.
            let wss_rate = if programmed_rate > 0 {
                programmed_rate
            } else if autocal_active {
                WSS_AUTOCAL_FALLBACK_HZ
            } else {
                0
            };
            let wss_dma = self.wss_dma;
            let wss_irq = self.wss_irq;
            // Run the sample clock whenever there is actual per-frame work pending:
            // either playback is armed (and the rate is valid), or the post-MCE ACI
            // window is still retiring (a driver clears MCE and polls ACI before
            // setting PEN). Gating on work mirrors the DSP path's `is_playing()`
            // check so an idle codec -- the default state on every machine at
            // power-on (rate 8000 Hz, not playing, no autocal) -- skips the
            // accumulation entirely instead of spinning ~8000 times/sec.
            let playing_at_valid_rate = programmed_rate > 0 && self.wss.is_playing();
            if wss_rate > 0 && (playing_at_valid_rate || autocal_active) {
                self.wss_sample_phase += clocks as f64 * wss_rate as f64 * self.timing.inv_clock;
                while self.wss_sample_phase >= 1.0 {
                    self.wss_sample_phase -= 1.0;
                    let Machine {
                        wss, dma, memory, ..
                    } = self;
                    // The codec pulls the 1/2/4 bytes a frame needs internally; an
                    // idle (unarmed) codec, or one running only to retire ACI under
                    // an invalid rate, ignores the fetcher and renders nothing.
                    if playing_at_valid_rate {
                        wss.tick_sample(|| dma.read_byte(wss_dma, memory));
                    }
                    wss.advance_autocal();
                }
                if self.wss.take_irq() {
                    self.pic.request(wss_irq);
                }
            }
        }

        self.pit_clocks += clocks as f64 * self.timing.pit_per_clock;
        let whole = self.pit_clocks.floor();
        self.pit_clocks -= whole;
        let edges = self.pit.tick(whole as u64);
        for _ in 0..edges {
            self.pic.request(0); // channel 0 OUT rising edge is IRQ0
        }

        // PC speaker: sample (ch2 OUT AND data enable) into the speaker ring at
        // the DAC rate. `clocks` is small (per instruction), so a tone is sampled
        // finely enough to form a square wave.
        self.speaker
            .accumulate(clocks, self.timing.inv_clock, self.pit.channel_out(2));

        if self.keyboard.take_irq() {
            self.pic.request(1); // IRQ1: keyboard output buffer has a scancode
        }
        if self.serial.take_irq() {
            self.pic.request(4); // IRQ4: COM1 (0x3F8) has a pending UART interrupt
        }
        if self.serial2.take_irq() {
            self.pic.request(3); // IRQ3: COM2 (0x2F8) has a pending UART interrupt
        }
        if self.keyboard.take_irq12() {
            self.pic.request(12); // IRQ12: mouse output buffer has an aux byte
        }
        if self.lpt.take_irq() {
            // IRQ7: LPT1 -ACK after a strobed byte. The Sound Blaster DSP can also
            // route to IRQ7, so this line is shared; the LPT only requests it on a
            // real strobed byte with control bit 4 set.
            self.pic.request(7);
        }
        if self.lpt2.take_irq() {
            self.pic.request(5); // IRQ5: LPT2 (0x278) -ACK after a strobed byte
        }

        // The floppy disk controller raises IRQ6 on command completion and seek
        // end. The DOR DMA/IRQ gate is honored inside take_irq, so a guest that
        // polls the FDC with the gate off does not get a spurious line.
        if self.fdc.take_irq() {
            self.pic.request(6);
        }

        // ATAPI command completion forwards IRQ15 (the secondary channel) to the
        // PIC, the way a real drive interrupts the host when a packet finishes.
        if self.ide.take_irq() {
            self.pic.request(ide::SECONDARY_IRQ);
        }
        // ATA hard-disk completion forwards IRQ14 (the primary channel) the same
        // way. The access-byte count flashes the C: LED through c_accesses.
        if let Some(disk) = self.ata.as_mut() {
            if disk.take_irq() {
                self.pic.request(ata::PRIMARY_IRQ);
            }
            if disk.take_access_bytes() > 0 {
                self.c_accesses += 1;
            }
        }
        // Flash the GUI CD LED for any data the drive just served.
        if self.ide.take_access_bytes() > 0 {
            self.cd_accesses += 1;
        }

        // Advance the RTC: inv_clock is 1/clock_hz, so clocks * inv_clock is
        // elapsed seconds. Fold whole seconds into the clock and carry the rest.
        self.rtc_seconds += clocks as f64 * self.timing.inv_clock;
        let whole_secs = self.rtc_seconds.floor();
        if whole_secs >= 1.0 {
            let secs = whole_secs as u64;
            self.rtc.tick_seconds(secs);
            // Advance the clock first, then evaluate the RTC interrupt sources so
            // an enabled alarm compares against the new time. tick_interrupts
            // returns true only on the rising edge of IRQF (a guest that has not
            // read Register C to ack keeps the line asserted without a new edge).
            if self.rtc.tick_interrupts(secs) {
                self.pic.request(8); // IRQ8: RTC periodic/alarm/update interrupt
            }
            self.rtc_seconds -= whole_secs;
        }

        self.margo_ns += clocks as f64 * self.timing.margo_ns_per_clock;
        let whole_ns = self.margo_ns.floor();
        self.margo.advance_busy(whole_ns as u64);
        self.margo_ns -= whole_ns;

        self.vga_dots += clocks as f64 * self.timing.vga_dots_per_clock;
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

    /// Render `native_samples` of DSP DMA output as stereo frames by draining
    /// the rendered-frame ring the per-CPU-clock producer (in `advance_devices`)
    /// fills. The block counter and the half/end-buffer IRQ now advance with CPU
    /// time, independent of this call; this path only reads back frames for the
    /// DAC. Each drained frame is attenuated by the CT1745 voice volume
    /// (`0x32`/`0x33`) so a mid-buffer guest volume change applies immediately. A
    /// silent (idle) DSP drains nothing, so the OPL passes through.
    pub fn render_dsp_audio(&mut self, native_samples: usize) -> Vec<(i16, i16)> {
        let (voice_l, voice_r) = self.mixer.voice_gain();
        let mut out = Vec::with_capacity(native_samples);
        for _ in 0..native_samples {
            match self.dsp.drain_frame() {
                Some((l, r)) => {
                    let l = clamp_i16((i32::from(l) as f32 * voice_l) as i32);
                    let r = clamp_i16((i32::from(r) as f32 * voice_r) as i32);
                    out.push((l, r));
                }
                None => break,
            }
        }
        out
    }

    /// Render `native_samples` of AD1848 / WSS DMA output as stereo frames by
    /// draining the codec's rendered-frame ring (filled by the clock-driven
    /// producer in advance_devices). The codec already applies its own I6/I7 DAC
    /// attenuation inside drain_frame's source path, and it is independent of the
    /// CT1745 mixer, so NO SB16 voice/master gain is applied here. An idle codec
    /// drains nothing, so it contributes silence (the OPL/DSP pass through).
    pub fn render_wss_audio(&mut self, native_samples: usize) -> Vec<(i16, i16)> {
        let mut out = Vec::with_capacity(native_samples);
        for _ in 0..native_samples {
            match self.wss.drain_frame() {
                Some(frame) => out.push(frame),
                None => break,
            }
        }
        out
    }

    /// Drive the internal per-clock device advance (PIT, OPL, DSP reset-settle,
    /// and the clock-driven DMA playback producer). Exposed so a host test or a
    /// frontend can flush device time without running the CPU, and so the DMA
    /// host goldens can advance the clock that now paces playback.
    pub fn advance_devices_clocks(&mut self, clocks: u64) {
        self.advance_devices(clocks);
    }

    /// Rebuild the DSP resampler when the programmed sample rate changes, so it
    /// always runs rate_hz -> 44100.
    fn sync_dsp_resampler(&mut self) {
        let rate = self.dsp.output_frame_rate().max(1);
        if rate != self.dsp_rate_hz {
            self.dsp_resampler = Resampler::new(rate, DAC_HZ);
            self.dsp_rate_hz = rate;
        }
    }

    /// Rebuild the WSS resampler when the codec's programmed sample rate changes,
    /// so it always runs output_frame_rate -> 44100. Mirrors sync_dsp_resampler;
    /// `.max(1)` guards the two unsupported XTAL1 clock selects that decode to 0.
    fn sync_wss_resampler(&mut self) {
        let rate = self.wss.output_frame_rate().max(1);
        if rate != self.wss_rate_hz {
            self.wss_resampler = Resampler::new(rate, DAC_HZ);
            self.wss_rate_hz = rate;
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
        let dsp_native_count = (native_samples as f64 * self.dsp.output_frame_rate() as f64
            / OPL_NATIVE_HZ as f64)
            .round() as usize;
        // The DSP already produces stereo frames; widen to i32 and resample.
        let dsp_stereo: Vec<(i32, i32)> = self
            .render_dsp_audio(dsp_native_count)
            .iter()
            .map(|&(l, r)| (i32::from(l), i32::from(r)))
            .collect();
        let dsp_out = self.dsp_resampler.process(&dsp_stereo);

        // AD1848 / WSS: the same wall-clock window's worth of codec frames,
        // resampled to the DAC rate. The codec is independent of the CT1745 mixer
        // (its I6/I7 DAC attenuation is already applied inside the frames), so it
        // is summed directly below WITHOUT the SB16 master/voice/outgain scaling.
        let wss_out = if self.wss_enabled {
            self.sync_wss_resampler();
            let wss_native_count = (native_samples as f64 * self.wss.output_frame_rate() as f64
                / OPL_NATIVE_HZ as f64)
                .round() as usize;
            let wss_stereo: Vec<(i32, i32)> = self
                .render_wss_audio(wss_native_count)
                .iter()
                .map(|&(l, r)| (i32::from(l), i32::from(r)))
                .collect();
            self.wss_resampler.process(&wss_stereo)
        } else {
            Vec::new()
        };

        // Apply master + output gain (0x30/0x31, 0x41/0x42) once to the summed
        // pair. The DSP frames already carry the voice gain from render_dsp_audio,
        // so this single scaling pass gives DSP·voice·master·outgain and
        // OPL·master·outgain. A silent (idle) DSP yields no frames, so the OPL
        // passes through (attenuated only by master/outgain) when no DMA is armed.
        // The WSS stream is summed in raw afterward (independent of the mixer).
        let (master_l, master_r) = self.mixer.master_gain();
        let (outgain_l, outgain_r) = self.mixer.outgain_gain();
        let len = opl_out.len().max(dsp_out.len()).max(wss_out.len());
        let spk = self.speaker.drain(len);
        // CD-Audio: pull the matching count of Red Book samples (44.1 kHz, the
        // DAC rate, so no resample) and attenuate by the CT1745 CD volume. A drive
        // that is not playing returns silence, so this is a no-op when no PLAY
        // AUDIO is active. This realizes CD audio through the ReSonique 2 DAC.
        let (cd_l_gain, cd_r_gain) = self.mixer.cd_gain();
        let cd = self.pull_cd_audio_samples(len);
        (0..len)
            .map(|i| {
                let (ol, or) = opl_out.get(i).copied().unwrap_or((0, 0));
                let (dl, dr) = dsp_out.get(i).copied().unwrap_or((0, 0));
                let (wl, wr) = wss_out.get(i).copied().unwrap_or((0, 0));
                let s = i32::from(spk[i]);
                let (cl, cr) = cd.get(i).copied().unwrap_or((0, 0));
                let cl = (cl as f32 * cd_l_gain) as i32;
                let cr = (cr as f32 * cd_r_gain) as i32;
                // OPL + SB16 DSP take the CT1745 master/outgain; the WSS codec is
                // summed in raw (its own attenuation already applied), like the
                // speaker and CD streams that bypass the SB16 mixer.
                let l = ((ol + dl) as f32 * (master_l * outgain_l)) as i32 + wl + s + cl;
                let r = ((or + dr) as f32 * (master_r * outgain_r)) as i32 + wr + s + cr;
                (clamp_i16(l), clamp_i16(r))
            })
            .collect()
    }

    /// Pull `count` stereo CD-audio samples (44.1 kHz, the DAC rate) from the
    /// ATAPI drive's active PLAY AUDIO, advancing the playback position. Each Red
    /// Book frame (one CD sector) holds 588 stereo 16-bit samples; the helper
    /// reads frames on demand and tracks the fractional frame consumed so the
    /// stream is continuous across calls. Returns silence when no audio is
    /// playing.
    fn pull_cd_audio_samples(&mut self, count: usize) -> Vec<(i32, i32)> {
        const SAMPLES_PER_FRAME: usize = crate::cdimage::RAW_SECTOR / 4; // 588
        let mut out = Vec::with_capacity(count);
        if !self.ide.device().playback().playing {
            self.cd_audio_frac = 0.0;
            return out;
        }
        // cd_audio_frac is the next sample index within the current frame, carried
        // across render calls so the stream stays continuous. Peek the current
        // frame, drain its remaining samples, then step to the next frame.
        let mut sample_in_frame = self.cd_audio_frac as usize;
        while out.len() < count {
            let Some(buf) = self.ide.device().peek_audio_frame() else {
                break; // playback reached its end mid-window
            };
            while sample_in_frame < SAMPLES_PER_FRAME && out.len() < count {
                let base = sample_in_frame * 4;
                let l = i16::from_le_bytes([buf[base], buf[base + 1]]);
                let r = i16::from_le_bytes([buf[base + 2], buf[base + 3]]);
                out.push((i32::from(l), i32::from(r)));
                sample_in_frame += 1;
            }
            if sample_in_frame >= SAMPLES_PER_FRAME {
                // Consumed the whole frame: step the play position forward.
                self.ide.device_mut().advance_play(1);
                sample_in_frame = 0;
            }
        }
        self.cd_audio_frac = sample_in_frame as f64;
        out
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

    /// CPU clocks to advance while halted so the next wake-capable IRQ lands, or
    /// None if nothing can wake the CPU (so HLT is a genuine halt). A halted guest
    /// is woken by any of three sources: IRQ0 (PIT channel 0 OUT edge), IRQ5 (the
    /// SB16 DSP half/end-buffer edge, clock-driven), or the AD1848/WSS codec's
    /// terminal-count edge on its own (config) IRQ line. Each is considered only
    /// when unmasked/deliverable; the result is the soonest of the applicable
    /// wakes, clamped to the deadline and to at least one clock so the run loop
    /// always makes progress.
    fn next_timer_wake(&self, deadline: u64) -> Option<u64> {
        if !self.cpu.interrupts_enabled() {
            return None;
        }
        let remaining = deadline.saturating_sub(self.elapsed_clocks);
        if remaining == 0 {
            return None;
        }
        let pit_wake = if self.pic.irq0_unmasked() {
            self.clocks_until_timer0_irq().map(|pit_delta| {
                ((u128::from(pit_delta) * u128::from(self.active_mode.clock_hz()))
                    .div_ceil(u128::from(PIT_INPUT_HZ))) as u64
            })
        } else {
            None
        };
        let dsp_wake = if self.pic.deliverable(self.mixer.selected_irq()) {
            // clocks_until_next_irq reasons in block-counter units (bytes for
            // 8-bit, words for 16-bit), so it must be fed the rate at which that
            // counter drains -- the raw byte/word rate -- not the per-channel
            // output frame rate. In SB Pro 8-bit stereo the counter ticks two
            // bytes per frame at the full byte rate (rate_hz), so passing
            // output_frame_rate() (= rate_hz/2) would over-estimate the wake by
            // 2x. rate_hz() is exact for every 8-bit path and keeps the
            // documented conservative estimate for 16-bit stereo (counter in
            // words, drained at 2x the per-channel frame rate).
            self.dsp
                .clocks_until_next_irq(self.dsp.rate_hz(), self.active_mode.clock_hz())
        } else {
            None
        };
        // The AD1848 / WSS terminal-count wake, on the codec's own (config) IRQ
        // line. The codec drains one Current Count per output frame, so its IRQ
        // estimator is fed the frame rate directly (no byte/word-counter scaling
        // like the SB16's). Considered only when that line can actually deliver
        // (`deliverable` also requires the master IR2 cascade pin for a slave line
        // 9/10/11) and the codec is enabled; clocks_until_next_irq also returns
        // None when IEN is clear (the underflow then sets only the sticky Status
        // bit, no pin edge).
        let wss_wake = if self.wss_enabled && self.pic.deliverable(self.wss_irq) {
            self.wss
                .clocks_until_next_irq(self.wss.rate_hz(), self.active_mode.clock_hz())
        } else {
            None
        };
        // The sooner of whichever wakes apply; None only when none can fire.
        let wake = [pit_wake, dsp_wake, wss_wake].into_iter().flatten().min()?;
        Some(wake.max(1).min(remaining))
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
            self.io_touched = false;
            let trace_before = self.trace.elapsed_clocks();
            // Run a batch of straight-line instructions against one MachineBus,
            // then service devices once. The cap holds the batch to at most one
            // DAC sample of CPU time so the per-clock fine-samplers stay exact; a
            // port access, an HLE INT, a HLT, or a fault ends it sooner. This is
            // the global-TSC / event-batched model (research item 2.3): it drops
            // the per-instruction bus rebuild + 14-device fan-out that dominated
            // the old loop, and is the prerequisite for the recompiler (item 2.2).
            let cap = self
                .timing
                .clocks_per_audio_sample
                .min(deadline - self.elapsed_clocks);
            let outcome = {
                let Machine {
                    profile,
                    active_mode,
                    pending_mode,
                    cpu,
                    memory,
                    video,
                    margo,
                    rom,
                    serial,
                    serial2,
                    lpt,
                    lpt2,
                    device_ports,
                    pic,
                    pit,
                    keyboard,
                    speaker,
                    rtc,
                    dma,
                    fdc,
                    floppy,
                    opl,
                    dsp,
                    mixer,
                    wss,
                    wss_base,
                    wss_enabled,
                    ide,
                    ata,
                    trace,
                    pending_soft_int,
                    fast_post,
                    booter_inert,
                    pending_toka_service,
                    toka_service_status,
                    unittester,
                    io_touched,
                    ..
                } = self;
                let mut bus = MachineBus {
                    memory,
                    video,
                    margo,
                    rom,
                    serial,
                    serial2,
                    lpt,
                    lpt2,
                    device_ports,
                    pic,
                    pit,
                    keyboard,
                    speaker,
                    rtc,
                    dma,
                    fdc,
                    floppy,
                    opl,
                    dsp,
                    mixer,
                    wss,
                    wss_base: *wss_base,
                    wss_enabled: *wss_enabled,
                    ide,
                    ata,
                    trace,
                    pending_soft_int,
                    active_mode: *active_mode,
                    pending_mode,
                    fast_post: *fast_post,
                    booter_inert: *booter_inert,
                    pending_toka_service,
                    toka_service_status: *toka_service_status,
                    unittester,
                    wait_states: profile.wait_states,
                    io_touched,
                };
                // Collapse the batch into one CycleOutcome so every downstream
                // service step (device advance, CD stall, pending INT/mode/Toka/
                // unittester, console flush, HLT fast-forward) is unchanged:
                // core_clocks is the batch sum, halted is set iff the batch ended
                // on a HLT. core_clocks can't overflow u32 (cap is ~one audio
                // sample, a few thousand clocks at most).
                let mut batch_core = 0u32;
                let mut halted = false;
                let mut fault = None;
                loop {
                    match cpu.cycle(&mut bus) {
                        Ok(o) => {
                            batch_core = batch_core.saturating_add(o.core_clocks);
                            if o.halted {
                                halted = true;
                                break;
                            }
                            // A port access read or changed time-dependent device
                            // state; an HLE INT (pending_soft_int) needs &mut self.
                            // Stop so the run loop services them at this instant.
                            if *bus.io_touched || bus.pending_soft_int.is_some() {
                                break;
                            }
                            if u64::from(batch_core) >= cap {
                                break;
                            }
                        }
                        Err(e) => {
                            fault = Some(e);
                            break;
                        }
                    }
                }
                match fault {
                    Some(e) => Err(e),
                    None => Ok(CycleOutcome {
                        core_clocks: batch_core,
                        halted,
                    }),
                }
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
                    // Charge the CD-ROM's seek + transfer time for a read the
                    // instruction just issued, the way the floppy stalls. The
                    // guest clock jumps; the GUI's realtime pacing turns that into
                    // a visible wait.
                    let cd_secs = self.ide.take_stall_secs();
                    if cd_secs > 0.0 {
                        self.stall_for(cd_secs);
                    }
                    if let Some(mode) = self.pending_mode.take() {
                        self.set_mode(mode); // live Lotura switch takes effect next instruction
                    }
                    if let Some(cmd) = self.pending_toka_service.take() {
                        self.perform_toka_service(cmd); // Repair/Format/LoadBootRecord
                    }
                    if let Some(cmd) = self.unittester.take_pending() {
                        if let Some(code) = self.perform_unittester(cmd) {
                            return Ok(StopReason::TestExit { code });
                        }
                    }
                    if let Some(vector) = self.pending_soft_int {
                        match vector {
                            0x10 => self.handle_int10(),
                            0x11 => self.handle_int11(),
                            0x12 => self.handle_int12(),
                            0x13 => self.handle_int13(),
                            0x14 => self.handle_int14(),
                            0x15 => self.handle_int15(),
                            0x17 => self.handle_int17(),
                            0x18 => self.handle_int18(),
                            0x19 => self.handle_int19(),
                            0x1A => self.handle_int1a(),
                            0x25 => self.handle_int25(),
                            0x26 => self.handle_int26(),
                            0x28 => self.handle_int28(),
                            0x29 => self.handle_int29(),
                            0x33 => self.handle_int33(),
                            0x2F => {
                                self.handle_int2f();
                            }
                            0x66 => self.handle_xms(),
                            0x20 | 0x21 => match self.handle_dos_int(vector) {
                                Ok(Some(code)) => {
                                    if let Some(frame) = self.program_frames.pop() {
                                        // A child exited; resume the parent.
                                        self.cpu.registers = frame.registers;
                                        self.dos.finish_exec(code);
                                        // EXEC success: AX=0, CF=0 in the parent's
                                        // INT-return FLAGS image (SS:SP+4).
                                        let ss = self.cpu.registers.segment(SegmentIndex::Ss).base;
                                        let sp = self.cpu.registers.esp() as u16;
                                        let flags_addr =
                                            (ss + u32::from(sp.wrapping_add(4))) as usize;
                                        let mut flags = self.memory.read_u16(flags_addr)?;
                                        flags &= !0x0001; // CF=0
                                        self.memory.write_u16(flags_addr, flags)?;
                                        self.cpu.registers.set_eax(0);
                                        // fall through: the loop continues, the
                                        // IRET stub returns to the parent.
                                    } else {
                                        return Ok(StopReason::DosExit { code });
                                    }
                                }
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
                    // Mirror any DOS console output onto the VGA text screen.
                    self.flush_dos_console_to_screen();
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
    serial: &'a mut uart::Uart16450,
    serial2: &'a mut uart::Uart16450,
    lpt: &'a mut lpt::Lpt,
    lpt2: &'a mut lpt::Lpt,
    device_ports: &'a mut DevicePorts,
    pic: &'a mut pic::Pic8259Pair,
    pit: &'a mut pit::Pit,
    keyboard: &'a mut keyboard::Keyboard8042,
    speaker: &'a mut speaker::Speaker,
    rtc: &'a mut rtc::Rtc,
    dma: &'a mut dma::DmaController,
    fdc: &'a mut fdc::Fdc,
    // The mounted A: image the FDC transfers against. The borrowed bus needs it
    // alongside `dma` and `memory` so a READ/WRITE DATA port write can run the
    // floppy + DMA datapath in one place.
    floppy: &'a mut Option<floppy::Floppy>,
    opl: &'a mut OplChip,
    dsp: &'a mut SbDsp,
    mixer: &'a mut SbMixer,
    // The AD1848 codec and its config-region base. The port decode routes the 8
    // ports in [wss_base, wss_base+8) to read_port/write_port when enabled; the
    // DMA/IRQ feed lives on the owning Machine in advance_devices, not here.
    wss: &'a mut Ad1848,
    wss_base: u16,
    wss_enabled: bool,
    ide: &'a mut ide::IdeChannel,
    ata: &'a mut Option<ata::AtaDisk>,
    trace: &'a mut BusTrace,
    pending_soft_int: &'a mut Option<u8>,
    active_mode: GswMode,                       // a copy, for the 0xE1 read
    pending_mode: &'a mut Option<GswMode>,      // a 0xE1 write records the request here
    fast_post: bool,                            // a copy, for the 0xE2 POST-pacing read
    booter_inert: bool,                         // a copy, stands the HLE down at INT-ack
    pending_toka_service: &'a mut Option<u8>,   // a 0xE3 write records the command
    toka_service_status: u8,                    // a copy, for the 0xE3 status read
    unittester: &'a mut unittester::UnitTester, // Lotura ports 0xE4-0xE6
    wait_states: WaitStateProfile,
    // Set true by any port I/O this batch. The run loop batches straight-line
    // instructions and services devices once per batch; a port access (a PIT
    // latch read, 0x3DA retrace poll, RTC read, a PIT/PIC/DSP/mode write) reads
    // or changes time-dependent device state, so it ends the batch to keep that
    // state exact. Memory/MMIO (framebuffer blits, the hot path) does not set it.
    io_touched: &'a mut bool,
}

/// The A20 gate clears address line 20 when it is closed. With the gate off, any
/// physical address with bit 20 set folds down by 0x100000, so a real-mode
/// program reaching 0x100000-0x10FFEF (the most a seg:off pair can address) wraps
/// back to 0x0-0xFFEF, the classic 1 MiB wraparound the HMA depends on. The
/// effect is intentionally global, matching A20M# on real hardware: bit 20 is
/// cleared on every physical address, so high ROM (0xFFFF0000) and the upper half
/// of the Margo LFB alias down too when the gate is closed. That is unreachable
/// in normal use, since A20 powers on enabled and stays so unless a guest
/// deliberately closes it.
const A20_MASK: u32 = !(1 << 20);

impl CpuBus for MachineBus<'_> {
    fn read_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<u32, BusError> {
        let address = self.apply_a20(address);
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
        let address = self.apply_a20(address);
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
        *self.io_touched = true;
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
        if let Some(value) = self.serial2.read_port(port) {
            return Ok(u32::from(value));
        }
        if let Some(value) = self.lpt.read_port(port) {
            return Ok(u32::from(value));
        }
        if let Some(value) = self.lpt2.read_port(port) {
            return Ok(u32::from(value));
        }
        if let Some(value) = self.video.read_port(port) {
            return Ok(u32::from(value));
        }
        if let Some(opl_port) = opl_port(port) {
            // The chip drives only the status byte on reads; data ports read open-bus.
            return Ok(u32::from(self.opl.read_port(opl_port).unwrap_or(0xff)));
        }
        if let Some(value) = self.mixer.read_port(port) {
            return Ok(u32::from(value));
        }
        // AD1848 / Windows Sound System: 4 config-region ports at wss_base plus
        // the 4 codec ports at wss_base+4. read_port takes the in-region offset
        // and returns a u8, so the range MUST be checked before the call. The
        // region (default 0x530-0x537) never overlaps the SB16 (0x220-0x22F),
        // CT1745 mixer (0x224/5), or OPL (0x388/9) ports.
        if let Some(offset) = self.wss_offset(port) {
            return Ok(u32::from(self.wss.read_port(offset)));
        }
        if ide::IdeChannel::owns_port(port) {
            return Ok(u32::from(self.ide.read_port(port).unwrap_or(0xff)));
        }
        if ata::AtaDisk::owns_port(port) {
            // The primary channel: a mounted disk drives the task file; an empty
            // channel reads open-bus (0xFF), so a probe sees no device.
            let value = self
                .ata
                .as_mut()
                .and_then(|d| d.read_port(port))
                .unwrap_or(0xff);
            return Ok(u32::from(value));
        }
        if fdc::Fdc::owns_port(port) {
            return Ok(u32::from(self.fdc.read_port(port).unwrap_or(0xff)));
        }
        if let Some(value) = self.dsp.read_port(port) {
            // A guest ISR acknowledges the DSP interrupt by reading 0x22E (8-bit)
            // or 0x22F (16-bit); that read also clears the mixer's 0x82 source bit.
            if port == 0x22E || port == 0x22F {
                self.mixer.clear_irq_status();
            }
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
        if port == 0x61 {
            // Bit 4 is the DRAM-refresh heartbeat: PIT channel 1 OUT (the AT
            // refresh timer, mode 2), not the speaker's standalone toggle. The PIT
            // seeds channel 1 at power-on so this pulses without guest programming.
            let value = (self.speaker.control_bits() & 0x03)
                | (u8::from(self.pit.channel_out(1)) << 4)
                | (u8::from(self.pit.channel_out(2)) << 5);
            return Ok(u32::from(value));
        }
        if port == 0x00e0 {
            return Ok(u32::from(LOTURA_ID_VALUE));
        }
        if port == 0x00e1 {
            return Ok(u32::from(gsw_mode_code(self.active_mode)));
        }
        if port == 0x00e2 {
            // Lotura POST-pacing flag: 1 = fast (skip cosmetic delays), 0 = full.
            return Ok(u32::from(u8::from(self.fast_post)));
        }
        if port == 0x00e3 {
            // Toka-DOS service status: 0 ok, 1 absent, other = error.
            return Ok(u32::from(self.toka_service_status));
        }
        if port == 0x0092 {
            // System control port A: bit 1 mirrors the A20 gate (the 8042 output
            // port is the single source of truth). Other bits read 0.
            return Ok(u32::from(u8::from(self.keyboard.a20_enabled()) << 1));
        }
        if let Some(value) = self.unittester.read_port(port) {
            return Ok(u32::from(value));
        }
        if let Some(value) = self.rtc.read_port(port) {
            return Ok(u32::from(value));
        }
        if let Some(value) = self.keyboard.read_port(port) {
            return Ok(u32::from(value));
        }
        self.device_ports
            .read_port(port)
            .map(u32::from)
            .ok_or(BusError::UnsupportedPort { port })
    }

    fn write_io(&mut self, port: u16, width: BusWidth, value: u32) -> Result<(), BusError> {
        *self.io_touched = true;
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
        if self.mixer.write_port(port, value as u8) {
            return Ok(());
        }
        // AD1848 / Windows Sound System write path. write_port takes the in-region
        // offset and returns (), so the range is checked first (mirrors read_io).
        if let Some(offset) = self.wss_offset(port) {
            self.wss.write_port(offset, value as u8);
            return Ok(());
        }
        if ide::IdeChannel::owns_port(port) {
            self.ide.write_port(port, value as u8);
            return Ok(());
        }
        if ata::AtaDisk::owns_port(port) {
            // Writes to an empty primary channel are dropped; a probe of a bare
            // channel must not fault. A mounted disk takes the task-file write.
            if let Some(disk) = self.ata.as_mut() {
                disk.write_port(port, value as u8);
            }
            return Ok(());
        }
        if fdc::Fdc::owns_port(port) {
            self.fdc.write_port(port, value as u8);
            // A READ/WRITE DATA command stages an execution-phase transfer the
            // chip cannot run on its own; the bus owns the floppy image and the
            // DMA channel, so run it here and feed the result phase back.
            if let Some(req) = self.fdc.take_transfer() {
                self.run_fdc_transfer(req);
            }
            return Ok(());
        }
        if self.dsp.write_port(port, value as u8) {
            return Ok(());
        }
        if self.dma.write_port(port, value as u8) {
            // The 8237A runs a memory-to-memory block transfer when the guest
            // arms a software DREQ on channel 0 (a write to the request register,
            // port 0x09) with mem-to-mem enabled in the command register. The
            // write above recorded the request; fire the block copy here.
            if port == 0x09 && self.dma.mem_to_mem_request_armed() {
                self.dma.mem_to_mem(self.memory);
            }
            return Ok(());
        }
        if port == 0x61 {
            self.speaker.write_control(value as u8);
            self.pit.set_gate(2, value & 1 != 0);
            return Ok(());
        }
        if port == 0x0092 {
            // Fast A20 gate: bit 1 drives A20, routed through the 8042 so every A20
            // method agrees. Bit 0 (fast CPU reset) is not modeled.
            self.keyboard.set_a20(value & 0x02 != 0);
            return Ok(());
        }
        if port == 0x00e1 {
            if let Some(mode) = gsw_mode_from_code(value as u8) {
                *self.pending_mode = Some(mode);
            }
            return Ok(());
        }
        if port == 0x00e3 {
            // Toka-DOS service command: 1 Repair, 2 Format, 0x10 LoadBootRecord.
            // The run loop performs it after this cycle (it needs &mut self).
            *self.pending_toka_service = Some(value as u8);
            return Ok(());
        }
        if self.unittester.write_port(port, value as u8) {
            return Ok(());
        }
        if self.rtc.write_port(port, value as u8) {
            return Ok(());
        }
        if self.serial.write_port(port, value as u8)
            || self.serial2.write_port(port, value as u8)
            || self.lpt.write_port(port, value as u8)
            || self.lpt2.write_port(port, value as u8)
            || self.video.write_port(port, value as u8)
            || self.pit.write_port(port, value as u8)
            || self.pic.write_port(port, value as u8)
            || self.keyboard.write_port(port, value as u8)
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
        //
        // In booter-inert mode the DOS kernel, absolute-disk, multiplex, and XMS
        // vectors stand down so a self-booting disk owns them through the IVT; the
        // BIOS hardware services (0x10-0x1A) and the mouse stay intercepted.
        let dos_or_iemm = matches!(
            vector,
            0x20 | 0x21 | 0x25 | 0x26 | 0x28 | 0x29 | 0x2F | 0x66
        );
        let intercepted = matches!(
            vector,
            0x10 | 0x11
                | 0x12
                | 0x13
                | 0x14
                | 0x15
                | 0x17
                | 0x18
                | 0x19
                | 0x1A
                | 0x20
                | 0x21
                | 0x25
                | 0x26
                | 0x28
                | 0x29
                | 0x2F
                | 0x33
                | 0x66
        );
        if intercepted && !(self.booter_inert && dos_or_iemm) {
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
        0x0062..=0x0063, // system control port B (speaker now owns 0x61)
        0x0080..=0x008f, // DMA page registers
        0x00c0..=0x00df, // DMA controller 2
        0x0220..=0x022f, // Sound Blaster base
        0x0388..=0x038b, // OPL2/OPL3 (intercepted by the chip, kept as a fallback)
        0x03b0..=0x03df, // MDA/CGA/EGA/VGA registers
    ];
    ranges.into_iter().flatten()
}

fn gsw_mode_from_code(code: u8) -> Option<GswMode> {
    match code {
        0 => Some(GswMode::Gsw386),
        1 => Some(GswMode::Gsw486),
        2 => Some(GswMode::Gsw586),
        3 => Some(GswMode::Gsw286),
        _ => None,
    }
}

fn gsw_mode_code(mode: GswMode) -> u8 {
    match mode {
        GswMode::Gsw386 => 0,
        GswMode::Gsw486 => 1,
        GswMode::Gsw586 => 2,
        // 286 (Super Slow) takes code 3 so the original 386/486/586 codes keep their
        // values and old guests that write 0/1/2 are unaffected.
        GswMode::Gsw286 => 3,
    }
}

/// Map a GSW compatibility mode to the CPU instruction-set level it presents to the
/// guest. The 586 native default keeps the full ISA; a lower mode lowers the level
/// so the core raises #UD for instructions that part lacked.
fn cpu_level_for_mode(mode: GswMode) -> CpuLevel {
    match mode {
        GswMode::Gsw286 => CpuLevel::I286,
        GswMode::Gsw386 => CpuLevel::I386,
        GswMode::Gsw486 => CpuLevel::I486,
        GswMode::Gsw586 => CpuLevel::I586,
    }
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

/// Convert a binary value 0..=99 to packed BCD. Values above 99 saturate the
/// high nibble, which is enough for the clock fields INT 1Ah returns.
fn bin_to_bcd(n: u8) -> u8 {
    ((n / 10) << 4) | (n % 10)
}

/// Convert packed BCD back to binary. The inverse of `bin_to_bcd`, used when a guest
/// sets the clock through INT 1Ah AH=03h/05h with BCD register fields.
fn bcd_to_bin(n: u8) -> u8 {
    (n >> 4) * 10 + (n & 0x0f)
}

/// Days elapsed from 1980-01-01 to the given calendar date, the count INT 1Ah AH=0Ah
/// reports. Gregorian leap years; the date is assumed valid (the RTC clamps it).
fn days_since_1980(year: u16, month: u8, day: u8) -> u16 {
    const MONTH_DAYS: [u16; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let is_leap = |y: u16| (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let mut days = 0u32;
    for y in 1980..year {
        days += if is_leap(y) { 366 } else { 365 };
    }
    for m in 1..u16::from(month) {
        days += u32::from(MONTH_DAYS[(m - 1) as usize]);
        if m == 2 && is_leap(year) {
            days += 1;
        }
    }
    days += u32::from(day.saturating_sub(1));
    days as u16
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

/// BIOS equipment word reported by INT 11h (BDA 0040:0010). Bit 0 set with
/// bits 7-6 clear means one floppy drive; bits 5-4 = 10b is the 80x25 color
/// initial video mode; bits 11-9 = 010b advertises two serial ports (COM1 and
/// COM2 are emulated); bits 15-14 = 10b advertises two parallel printer ports
/// (LPT1 and LPT2 are emulated). Bit 1 (80x87 coprocessor) stays clear: the
/// Izarra 3000 ships no 387, so software that probes the equipment word skips
/// its FPU path. See RBIL INT 11h equipment bitfield
/// (dev_docs/reference/rbil/INTERRUP.B).
const BIOS_EQUIPMENT_WORD: u16 = 0x8421;

/// Conventional memory size in KiB reported by INT 12h (BDA 0040:0013). A PC
/// caps usable low memory at 640 KiB no matter how much RAM is installed; the
/// rest is extended memory above 1 MiB (reported by INT 15h AH=88h).
const BIOS_BASE_MEMORY_KIB: u16 = 640;

/// BDA scratch word INT 1Ah AH=0Bh latches the system-timer day count into, for a
/// later read. It sits in the inter-application scratch area at 0040:00F0, which no
/// other field here uses.
const BDA_DAY_COUNT: usize = 0x4f0;

/// Segment of the ROM-resident IRET the BIOS keeps at ROM offset 0xF000, i.e.
/// FF00:0000. The host intercepts the BIOS service interrupts by vector number,
/// so their IVT targets only need a valid IRET to return on. Pointing them at
/// the ROM stub instead of the RAM stub at 0x600 keeps them working after a
/// booter wipes low memory, the way real BIOS handlers (which live in ROM) do.
const BIOS_ROM_IRET_SEG: u16 = 0xff00;

/// RAM address of the default INT 70h (IRQ8) handler, a few bytes past the IRET
/// stub at 0x600 in the free BIOS scratch below the .COM load segment (0x1000).
/// Unlike the host-serviced service INTs, the RTC interrupt arrives as a real
/// hardware IRQ, so its ISR is genuine guest code: it acknowledges Register C
/// and sends EOI to both 8259s before IRET.
const BIOS_RTC_ISR_ADDRESS: usize = 0x0610;

/// RAM address of the INT 18h "no bootable device" stub: CLI then HLT. Clearing
/// IF makes the HLT a genuine stop (the run loop will not wake a CPU whose
/// interrupts are masked), matching a real BIOS that gives up and halts.
const BIOS_HALT_STUB_ADDRESS: usize = 0x0620;

/// EBDA offset of the far pointer to the user pointing-device (mouse) handler the
/// guest installs with INT 15h AX=C207h. The IBM PS/2 BIOS keeps it here as
/// offset word then segment word; INT 15h AX=C208h reads it back.
const EBDA_MOUSE_HANDLER_OFF: u32 = 0x0022;

/// Soft-INT vector the XMS driver entry stub triggers to trap into the host. INT
/// 66h sits in the user-reserved range (RBIL: INT 60h-66h are free for
/// application use, and EMS is the next vector, 67h), so claiming it does not
/// collide with any DOS/BIOS service or with EMS. It is dispatched only as a
/// software INT from the entry stub; the CPU never faults with this vector.
const XMS_INT_VECTOR: u8 = 0x66;

/// ROM offset of the XMS driver entry stub. The ROM maps at LOW_BIOS_BASE
/// (0xF0000), so offset 0xF010 is physical 0xFF010, i.e. segment:offset
/// FF00:0010. That clears the ROM IRET at 0xF000 and the reset vector at 0xFFF0.
const XMS_ENTRY_ROM_OFFSET: usize = 0xf010;

/// Real-mode segment:offset of the XMS entry stub, handed back in ES:BX by INT
/// 2Fh AX=4310h.
const XMS_ENTRY_SEG: u16 = 0xff00;
const XMS_ENTRY_OFF: u16 = 0x0010;

/// The XMS entry stub bytes: `INT 66h` then `RETF`. A guest FAR-CALLs the entry,
/// the INT traps into the host XMS handler (which sets AX/BX/DX/etc. from the
/// function in AH), and the RETF returns to the caller.
const XMS_ENTRY_STUB: [u8; 3] = [0xcd, XMS_INT_VECTOR, 0xcb];

/// Real-mode segment of the 1 KB extended BIOS data area (EBDA), reserved at the
/// top of conventional memory. Segment 0x9FC0 is physical 0x9FC00, so the EBDA
/// runs 0x9FC00-0x9FFFF and the conventional-memory word at 0040:0013 drops from
/// 640 to 639 KB. INT 15h AH=C1h returns this segment in ES.
const EBDA_SEGMENT: u16 = 0x9FC0;

/// Physical base of the INT 15h AH=C0h system-configuration table. It lives inside
/// the reserved EBDA (after the size byte at offset 0), so it is consistent with
/// the lowered conventional-memory size and out of the BDA's way.
const BIOS_CONFIG_TABLE_ADDR: u32 = 0x9FC10;

fn install_boot_bios_stubs(memory: &mut Memory) -> Result<(), BusError> {
    // BIOS service interrupts the host intercepts by vector. Their IVT targets
    // point at the ROM IRET so they survive a guest low-memory wipe. INT 33h is
    // the mouse driver and INT 2Fh is the ICDEX CD bridge; INT 28h/29h are the DOS
    // idle and fast-console hooks; INT 25h/26h are the DOS absolute disk read/write:
    // the same stub shape the HLE handler returns through. INT 18h/19h are the
    // host-serviced boot and diskless vectors (the run loop services them and
    // redirects CS:IP itself, so the IRET target is only a fallback). INT 1Bh is
    // the Ctrl-Break hook: no host handler, just a default IRET so a guest that
    // hooks it or calls it through the vector has a valid target. INT 66h is the
    // XMS driver entry trap, host-intercepted the same way.
    for vector in [
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x25, 0x26, 0x28, 0x29,
        0x2F, 0x33, 0x66,
    ] {
        let address = vector * 4;
        memory.write_u16(address, 0)?;
        memory.write_u16(address + 2, BIOS_ROM_IRET_SEG)?;
    }
    // INT 70h (IRQ8) is a real hardware interrupt, not a host-serviced INT, so its
    // vector points at the RAM ISR stub that acks Register C and EOIs both PICs.
    memory.write_u16(0x70 * 4, BIOS_RTC_ISR_ADDRESS as u16)?;
    memory.write_u16(0x70 * 4 + 2, 0)?;
    install_rtc_isr_stub(memory)?;
    // INT 18h's halt target: CLI;HLT in low RAM. The run loop's INT 18h handler
    // points CS:IP here when no device is bootable.
    memory.write_u8(BIOS_HALT_STUB_ADDRESS, 0xfa)?; // CLI
    memory.write_u8(BIOS_HALT_STUB_ADDRESS + 1, 0xf4)?; // HLT
    // The DOS kernel vectors keep the RAM stub: the INT 21h blocking path rewinds
    // EIP onto the CD 21 the RAM stub returns to, and the DOS path owns its memory.
    for vector in [0x20, 0x21] {
        let address = vector * 4;
        memory.write_u16(address, BIOS_IRET_STUB_ADDRESS as u16)?;
        memory.write_u16(address + 2, 0)?;
    }
    // Seed the BDA words INT 11h and INT 12h hand back, like a real BIOS. The 1 KB
    // EBDA reserved below 640 KB lowers the conventional-memory word by 1 (to 639),
    // so INT 12h and the EBDA stay consistent.
    memory.write_u16(0x410, BIOS_EQUIPMENT_WORD)?;
    memory.write_u16(0x413, BIOS_BASE_MEMORY_KIB - 1)?;
    // Reserve the 1 KB EBDA at 0x9FC00 and write its size byte (1 = 1 KB) at offset
    // 0, the way a real BIOS POST does. INT 15h AH=C1h returns its segment.
    memory.write_u8((usize::from(EBDA_SEGMENT)) << 4, 1)?;
    seed_bios_config_table(memory)?;
    // Serial and parallel port base address tables POST detected (0040:0000 COM1-4,
    // 0040:0008 LPT1-4). COM1 (0x03F8) + COM2 (0x02F8) and LPT1 (0x0378) + LPT2
    // (0x0278) are wired, matching the equipment word; the rest read 0 (absent).
    // INT 14h/17h drive the ports, and software that reads a base straight from the
    // BDA finds it here.
    memory.write_u16(0x400, 0x03f8)?; // COM1 base
    memory.write_u16(0x402, 0x02f8)?; // COM2 base
    memory.write_u16(0x404, 0)?; // COM3 absent
    memory.write_u16(0x406, 0)?; // COM4 absent
    memory.write_u16(0x408, 0x0378)?; // LPT1 base
    memory.write_u16(0x40a, 0x0278)?; // LPT2 base
    memory.write_u16(0x40c, 0)?; // LPT3 absent
    memory.write_u16(0x40e, 0)?; // LPT4 absent
    // Per-port timeout tables: serial 0040:007C-007F, printer 0040:0078-007B. The
    // BIOS defaults a serial timeout of 0x01 and a printer timeout of 0x14.
    for offset in 0x47c..=0x47f {
        memory.write_u8(offset, 0x01)?; // COM1-4 timeouts
    }
    for offset in 0x478..=0x47b {
        memory.write_u8(offset, 0x14)?; // LPT1-4 timeouts
    }
    // Seed the BDA video state to text 80x25 (mode 03h) like a real BIOS POST.
    memory.write_u8(0x449, 0x03)?; // current video mode
    memory.write_u16(0x44a, 80)?; // columns on screen
    memory.write_u16(0x44c, 0x1000)?; // regen (page) size in bytes
    memory.write_u16(0x44e, 0)?; // active page start in regen buffer
    memory.write_u8(0x462, 0)?; // active display page
    memory.write_u16(0x463, 0x03d4)?; // CRTC base port
    memory.write_u8(0x484, 24)?; // rows on screen minus one
    memory.write_u8(0x485, 16)?; // character cell height in scan lines
    memory.write_u8(0x487, 0x60)?; // EGA/VGA control: 350-line, no cursor emulation
    memory.write_u8(0x488, 0xf9)?; // EGA/VGA switches / feature bits
    memory.write_u8(0x489, 0x00)?; // VGA flags (mode-set control)
    // Fixed-disk count: zero at construction, before any image is mounted.
    // Machine::mount_hdd bumps it to 1 when a hard disk attaches, so the count
    // tracks the real device rather than a fixed value. Ctrl-Break flag clear.
    // Warm-boot magic 0x1234 tells the BIOS to skip the memory test on reset.
    memory.write_u8(0x475, 0)?; // number of fixed disks
    memory.write_u8(0x471, 0)?; // Ctrl-Break flag
    memory.write_u16(0x472, 0x1234)?; // warm-boot magic
    // Keyboard data area. The two shift-flag bytes start clear (no key held). The
    // 32-byte INT 16h ring runs 0040:001E-003D; head and tail both point at its
    // start (empty), and the start/end pointers (0040:0080/0082) bracket it. A
    // guest that reads the BDA ring directly, or an INT 16h ROM that walks these
    // pointers, finds the standard empty-buffer layout.
    memory.write_u8(0x417, 0)?; // shift flags 1
    memory.write_u8(0x418, 0)?; // shift flags 2
    memory.write_u16(0x41a, 0x001e)?; // buffer head pointer (offset into segment 0040)
    memory.write_u16(0x41c, 0x001e)?; // buffer tail pointer (head == tail: empty)
    memory.write_u16(0x480, 0x001e)?; // buffer start
    memory.write_u16(0x482, 0x003e)?; // buffer end (32 bytes -> 16 key slots)
    memory.write_u8(0x496, 0)?; // keyboard mode/type flags
    memory.write_u8(0x497, 0)?; // keyboard LED flags
    // Disk status bytes start clear (no error). 0040:0041 is the floppy/INT 13h
    // last status (AH=01h reads it); 0040:0074 is the fixed-disk last status.
    memory.write_u8(0x43e, 0)?; // floppy recalibrate/seek status
    memory.write_u8(0x441, 0)?; // last floppy disk status
    memory.write_u8(0x474, 0)?; // last fixed-disk status
    memory.write_u8(BIOS_IRET_STUB_ADDRESS, 0xcf)
}

/// Write the default INT 70h (IRQ8) handler into low RAM: acknowledge the RTC by
/// reading Register C (which clears its flags and de-asserts the line) and send
/// end-of-interrupt to both 8259 PICs, then IRET. This is the minimum a real BIOS
/// INT 70h does before chaining to any user routine. A guest that masks IRQ8 or
/// installs its own handler simply overwrites this vector.
///
/// ponytail: the real BIOS INT 70h also tests the RTC wait flag (0040:00A0) and
/// signals the INT 15h AH=83h/86h event-wait completion at 0040:0098. No wait
/// flag is modeled here, so the stub only acks and EOIs; wire those BDA bytes and
/// an INT 15h AH=83h path to lift it.
fn install_rtc_isr_stub(memory: &mut Memory) -> Result<(), BusError> {
    // push ax; mov al,0Ch; out 70h,al; in al,71h; (ack Register C)
    // mov al,20h; out A0h,al; out 20h,al; (EOI slave then master)
    // pop ax; iret
    const STUB: [u8; 14] = [
        0x50, // push ax
        0xb0, 0x0c, // mov al,0Ch (select Register C)
        0xe6, 0x70, // out 70h,al
        0xe4, 0x71, // in al,71h (read clears the flags)
        0xb0, 0x20, // mov al,20h (non-specific EOI)
        0xe6, 0xa0, // out A0h,al (slave PIC)
        0xe6, 0x20, // out 20h,al (master PIC)
        0x58, // pop ax
    ];
    for (offset, &byte) in STUB.iter().enumerate() {
        memory.write_u8(BIOS_RTC_ISR_ADDRESS + offset, byte)?;
    }
    memory.write_u8(BIOS_RTC_ISR_ADDRESS + STUB.len(), 0xcf) // iret
}

/// Seed the INT 15h AH=C0h system-configuration table at BIOS_CONFIG_TABLE_ADDR.
/// The layout is the AT-class table the BIOS hands back in ES:BX: a WORD byte
/// count, then model/submodel/revision and the five feature bytes. Only feature
/// byte 1 carries set bits, and each is set only when the matching service is
/// actually present, per the honest-reporting rule.
fn seed_bios_config_table(memory: &mut Memory) -> Result<(), BusError> {
    // Feature byte 1 (RBIL INTERRUP.B, AH=C0h):
    //   bit6 second 8259 PIC present (the AT has IRQ8-15) -> set
    //   bit5 RTC present (INT 1Ah / CMOS clock)           -> set
    //   bit4 INT 15h/AH=4Fh keyboard-intercept issued     -> clear (no AH=4Fh callout)
    //   bit3 wait-for-external-event (AH=41h) supported    -> clear (not implemented)
    //   bit2 extended BIOS data area allocated             -> set (AH=C1h present)
    //   bit1 Micro Channel bus                             -> clear (ISA)
    const FEATURE_1: u8 = 0x40 | 0x20 | 0x04; // 0x64
    let base = BIOS_CONFIG_TABLE_ADDR as usize;
    let table: [u8; 10] = [
        0x08, 0x00, // WORD length: 8 bytes follow
        0xFC, // model: AT-class
        0x00, // submodel
        0x00, // BIOS revision
        FEATURE_1, 0x00, 0x00, 0x00, 0x00, // feature bytes 1-5
    ];
    for (i, &byte) in table.iter().enumerate() {
        memory.write_u8(base + i, byte)?;
    }
    Ok(())
}

impl MachineBus<'_> {
    /// In-region offset (0..=7) of `port` within the AD1848 / WSS port window
    /// `[wss_base, wss_base + 8)`, or `None` when the codec is disabled or the
    /// port lies outside the window. The codec's read_port/write_port take this
    /// offset; the caller dispatches to them only on `Some`.
    fn wss_offset(&self, port: u16) -> Option<u16> {
        if !self.wss_enabled {
            return None;
        }
        port.checked_sub(self.wss_base).filter(|&off| off < 8)
    }

    /// Apply the A20 gate to a physical address before it reaches memory. The gate
    /// is the single 8042 output-port bit (shared with fast-A20 port 0x92); when
    /// it is closed, address line 20 is forced low. This is the motherboard-level
    /// effect, so it sits at the one CPU bus seam and covers fetches and data
    /// alike. Host-side pokes (write_physical_u8 and friends) deliberately bypass
    /// it: they address exact physical cells, not the guest's gated bus.
    fn apply_a20(&self, address: u32) -> u32 {
        if self.keyboard.a20_enabled() {
            address
        } else {
            address & A20_MASK
        }
    }

    /// The plane-window offset for an access that the guest-selected GC06 graphics
    /// aperture redirects, or None when the access is not in a moved graphics
    /// window. Only graphics modes consult the aperture; text and CGA keep the
    /// fixed B8000 decode. A power-on / default (128 KB) aperture is left to the
    /// fixed A0000 routing so every mode's default behavior is unchanged.
    fn vga_gfx_offset(&self, address: u32, width: usize) -> Option<usize> {
        match self.video.active_mode() {
            VideoMode::Planar | VideoMode::ModeX | VideoMode::Mode13h => {
                let ap = self.video.gfx_aperture();
                vga_gfx_aperture_offset(ap.base, ap.length, address, width)
            }
            VideoMode::Text | VideoMode::Cga => None,
        }
    }

    fn read_memory_bytes(&mut self, address: u32, width: usize) -> Result<Vec<u8>, BusError> {
        if let Some(offset) = rom_offset(address, width) {
            return Ok(self.rom[offset..offset + width].to_vec());
        }

        // A guest that moves the graphics aperture through GC06 (memory map select)
        // redirects the framebuffer window. When the active mode is a graphics mode
        // and GC06 points at a moved window, route the access through the planar /
        // chain-4 datapath before the fixed text/CGA window decode below.
        if let Some(offset) = self.vga_gfx_offset(address, width) {
            return Ok(match self.video.active_mode() {
                VideoMode::Mode13h => (0..width)
                    .map(|i| self.video.cpu_read_chain4(offset + i))
                    .collect(),
                _ => (0..width)
                    .map(|i| self.video.cpu_read(offset + i))
                    .collect(),
            });
        }

        if let Some(offset) = video_text_offset(address, width) {
            // In a CGA graphics mode the B800 aperture is the 16 KiB CGA
            // framebuffer; in text mode it is the character/attribute buffer.
            if self.video.active_mode() == VideoMode::Cga {
                return Ok((0..width)
                    .map(|i| self.video.cga_read(offset + i))
                    .collect());
            }
            return (0..width)
                .map(|index| {
                    self.video
                        .read_u8(offset + index)
                        .map_err(|_| BusError::UnmappedMemory { address })
                })
                .collect();
        }

        // The 64 KB A0000 window serves all three graphics modes. Unchained (mode
        // X) and 16-color planar route through the planar datapath (cpu_read loads
        // the VGA latches as a side effect, so it needs &mut self); chained mode
        // 13h routes through the chain-4 decode.
        if let Some(offset) = vga_planar_offset(address, width) {
            match self.video.active_mode() {
                VideoMode::Planar | VideoMode::ModeX => {
                    return Ok((0..width)
                        .map(|i| self.video.cpu_read(offset + i))
                        .collect());
                }
                VideoMode::Mode13h => {
                    return Ok((0..width)
                        .map(|i| self.video.cpu_read_chain4(offset + i))
                        .collect());
                }
                // Text and CGA do not decode the A0000 window; fall through.
                VideoMode::Text | VideoMode::Cga => {}
            }
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

        // A guest that moves the graphics aperture through GC06 redirects the
        // framebuffer window; route through the planar / chain-4 write datapath
        // before the fixed text/CGA window decode below.
        if let Some(offset) = self.vga_gfx_offset(address, 1) {
            match self.video.active_mode() {
                VideoMode::Mode13h => self.video.cpu_write_chain4(offset, value),
                _ => self.video.cpu_write(offset, value),
            }
            return Ok(());
        }

        if let Some(offset) = video_text_offset(address, 1) {
            // In a CGA graphics mode the B800 aperture is the 16 KiB CGA
            // framebuffer; in text mode it is the character/attribute buffer.
            if self.video.active_mode() == VideoMode::Cga {
                self.video.cga_write(offset, value);
                return Ok(());
            }
            return self
                .video
                .write_u8(offset, value)
                .map_err(|_| BusError::UnmappedMemory { address });
        }

        // The 64 KB A0000 window serves all three graphics modes. Unchained (mode
        // X) and 16-color planar route A0000 through the planar datapath (map mask,
        // write mode, bit mask, latches); chained mode 13h routes through the
        // chain-4 decode.
        if let Some(offset) = vga_planar_offset(address, 1) {
            match self.video.active_mode() {
                VideoMode::Planar | VideoMode::ModeX => {
                    self.video.cpu_write(offset, value);
                    return Ok(());
                }
                VideoMode::Mode13h => {
                    self.video.cpu_write_chain4(offset, value);
                    return Ok(());
                }
                // Text and CGA do not decode the A0000 window; fall through.
                VideoMode::Text | VideoMode::Cga => {}
            }
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

    /// Run a floppy READ/WRITE DATA execution phase the FDC staged: move sector
    /// bytes between the mounted image and guest memory over DMA channel 2, then
    /// hand the result phase back to the chip.
    ///
    /// The transfer walks sectors from the start id up to EOT on the addressed
    /// track, but the DMA terminal count is the real limit: the channel's
    /// programmed byte count decides how much actually moves, exactly as on
    /// hardware where the FDC streams until the DMAC asserts /TC. A read with no
    /// disk, an off-media address, or a masked/misprogrammed channel terminates
    /// abnormally.
    fn run_fdc_transfer(&mut self, req: fdc::TransferRequest) {
        const FDC_DMA_CHANNEL: usize = 2;
        let Some(geom) = self.floppy.as_ref().map(|f| f.geometry()) else {
            // No media: abnormal termination at the requested address.
            self.fdc
                .complete_transfer(req, req.cylinder, req.head, req.sector, false);
            return;
        };

        let cyl = u16::from(req.cylinder);
        let mut sector = req.sector;
        let mut last_sector = req.sector;
        let mut moved_any = false;
        let mut ok = true;

        // Walk sectors up to EOT, stopping early at DMA terminal count. EOT bounds
        // the track; the spec's sector ids are 1-based.
        while sector <= req.end_sector && sector <= geom.sectors {
            if self.dma.at_terminal_count(FDC_DMA_CHANNEL) {
                break;
            }
            if req.read {
                // Disk -> memory: copy the sector out of the image first (an
                // immutable borrow), then push the bytes through DMA channel 2.
                let Some(data) = self
                    .floppy
                    .as_ref()
                    .and_then(|f| f.read_sector(cyl, req.head, sector))
                    .map(|s| s.to_vec())
                else {
                    ok = false;
                    break;
                };
                let mut pushed = 0usize;
                for &byte in &data {
                    if self
                        .dma
                        .write_byte(FDC_DMA_CHANNEL, self.memory, byte)
                        .is_none()
                    {
                        // DMA reached terminal count (or the channel is not
                        // programmed for a write transfer): stop streaming.
                        break;
                    }
                    pushed += 1;
                }
                if pushed == 0 {
                    // A masked or unprogrammed channel moved no bytes: abnormal
                    // termination, not a clean completion (matches the write path).
                    break;
                }
            } else {
                // Memory -> disk: pull a sector's worth out of the DMA channel,
                // then commit it to the image.
                let mut data = vec![0u8; usize::from(req.bytes_per_sec)];
                let mut filled = 0usize;
                for slot in data.iter_mut() {
                    match self.dma.pull_byte(FDC_DMA_CHANNEL, self.memory) {
                        Some(byte) => {
                            *slot = byte;
                            filled += 1;
                        }
                        None => break,
                    }
                }
                if filled == 0 {
                    break; // nothing left to write
                }
                let wrote = self
                    .floppy
                    .as_mut()
                    .map(|f| f.write_sector(cyl, req.head, sector, &data))
                    .unwrap_or(false);
                if !wrote {
                    ok = false;
                    break;
                }
            }
            moved_any = true;
            last_sector = sector;
            sector += 1;
        }

        // Success means at least one sector moved without an off-media fault.
        let success = ok && moved_any;
        self.fdc
            .complete_transfer(req, req.cylinder, req.head, last_sector, success);
    }

    fn memory_wait_states(&self, address: u32) -> u8 {
        if rom_offset(address, 1).is_some() {
            self.wait_states.rom
        } else if self.vga_gfx_offset(address, 1).is_some()
            || video_text_offset(address, 1).is_some()
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

/// The A0000 window for chained mode 13h and unchained / 16-color planar access:
/// the full 64 KB the hardware decodes.
fn vga_planar_offset(address: u32, width: usize) -> Option<usize> {
    let end = VGA_MODE13H_BASE + VGA_PLANAR_WINDOW_SIZE;
    if (VGA_MODE13H_BASE..end).contains(&address) && address + width as u32 <= end {
        Some((address - VGA_MODE13H_BASE) as usize)
    } else {
        None
    }
}

/// The graphics-mode CPU window the guest selected through Graphics Controller
/// register 06h (memory map select), as a plane-window offset for the VGA
/// datapath. Returns Some(offset) when the access falls in a guest-moved aperture
/// that the fixed A0000 window does not already cover.
///
/// The aperture's `base`/`length` come from `Vga::gfx_aperture`. The plane window
/// is 64 KB, so the offset is `address - base` capped to that window; a 128 KB
/// aperture is clamped to the low 64 KB the datapath addresses.
// ponytail: the power-on / map-select-00 aperture (A0000, 128 KB) is left to the
// fixed A0000 64 KB routing, so the default behavior of every mode is byte-for-
// byte unchanged. Only a guest that programs a smaller, moved window (64 KB at
// A0000, or 32 KB at B0000 / B8000) is routed here. Power-on and an explicit
// 128 KB selection are indistinguishable (both read misc bits 3-2 as 00), so the
// rarely used full-128 KB decode stays on the legacy path rather than extending
// graphics routing across B0000-BFFFF.
fn vga_gfx_aperture_offset(base: u32, length: u32, address: u32, width: usize) -> Option<usize> {
    // A 128 KB window is the default/legacy decode; do not reroute it here.
    if length >= VGA_PLANAR_WINDOW_SIZE * 2 {
        return None;
    }
    let end = base + length;
    if (base..end).contains(&address) && address + width as u32 <= end {
        let offset = (address - base) as usize;
        // The planar/chain-4 datapath indexes a 64 KB plane window.
        (offset + width <= VGA_PLANAR_WINDOW_SIZE as usize).then_some(offset)
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
    use izarravm_core::{SbDma8, SbDma16, SbIrq};
    use izarravm_firmware::I386DX25_TEST_ROM;

    #[test]
    fn slow_post_paces_without_null_vector_runaway() {
        // Under slow POST the BIOS drives PIT channel 0 to pace the chime and the
        // RAM count-up. Those OUT edges raise IRQ0 with IF set; before INT 08h was
        // installed the timer vectored through the zeroed IVT[08h] (CS=0000) and ran
        // away through low memory. Run a slice that covers the chime and the start of
        // the count-up, then confirm the CPU never left the BIOS region and the INT
        // 08h handler advanced the BDA tick count.
        let mut machine = Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            izarravm_firmware::izarra_bios(),
        )
        .unwrap();
        machine.set_fast_post(false);
        let mut max_ticks = 0u32;
        for _ in 0..400 {
            let _ = machine.run_until_halt_or_cycles(50_000).unwrap();
            let cs = machine.cpu().registers.cs().selector;
            assert_ne!(cs, 0, "CPU vectored to CS=0000 (null IVT runaway)");
            let lo = u32::from(machine.read_physical_u8(0x46c));
            let hi = u32::from(machine.read_physical_u8(0x46d));
            max_ticks = max_ticks.max(lo | (hi << 8));
        }
        assert!(
            max_ticks > 3,
            "INT 08h did not advance the BDA tick (got {max_ticks})"
        );
    }

    fn test_machine() -> Machine {
        Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            I386DX25_TEST_ROM,
        )
        .unwrap()
    }

    fn int15_machine(mem_mib: u16) -> Machine {
        Machine::new(
            MachineProfile::gsw_386(mem_mib, VideoCard::Et4000Ax),
            vec![0u8; BIOS_ROM_SIZE],
        )
        .unwrap()
    }

    #[test]
    fn int15_8a_reports_extended_memory_as_dx_ax() {
        let mut m = int15_machine(24);
        m.cpu.registers.set_eax(0x8A00);
        m.handle_int15();
        // 23 MB above the first 1 MB = 23552 KB = 0x5C00 (fits in AX, DX = 0).
        assert_eq!(m.cpu.registers.eax() as u16, 0x5C00);
        assert_eq!(m.cpu.registers.edx() as u16, 0x0000);
    }

    #[test]
    fn int15_e801_splits_memory_at_16m() {
        let mut m = int15_machine(24);
        m.cpu.registers.set_eax(0xE801);
        m.handle_int15();
        // 1-16 MB capped at 0x3C00 KB; 8 MB above 16 MB = 128 64KB-blocks = 0x80.
        assert_eq!(m.cpu.registers.eax() as u16, 0x3C00);
        assert_eq!(m.cpu.registers.ebx() as u16, 0x80);
        assert_eq!(m.cpu.registers.ecx() as u16, 0x3C00);
        assert_eq!(m.cpu.registers.edx() as u16, 0x80);
    }

    #[test]
    fn int15_e820_walks_the_memory_map() {
        let mut m = int15_machine(24);
        // ES = 0, DI = 0: the descriptor lands at physical 0 in test RAM.
        let mut ebx = 0u32;
        let mut regions = Vec::new();
        loop {
            m.cpu.registers.set_eax(0xE820);
            m.cpu.registers.set_edx(0x534D_4150);
            m.cpu.registers.set_ecx(20);
            m.cpu.registers.set_ebx(ebx);
            m.handle_int15();
            assert_eq!(m.cpu.registers.eax(), 0x534D_4150);
            assert_eq!(m.cpu.registers.ecx(), 20);
            let base = m.read_guest_dword(0);
            let len = m.read_guest_dword(8);
            let kind = m.read_guest_dword(16);
            regions.push((base, len, kind));
            ebx = m.cpu.registers.ebx();
            if ebx == 0 {
                break;
            }
        }
        assert_eq!(regions.len(), 4);
        assert_eq!(regions[0], (0x0, 0x9_FC00, 1)); // 639 KB conventional (below EBDA)
        assert_eq!(regions[1], (0x9_FC00, 0x400, 2)); // 1 KB EBDA, reserved
        assert_eq!(regions[2], (0xA_0000, 0x6_0000, 2)); // reserved hole
        assert_eq!(regions[3], (0x10_0000, 23 * 0x10_0000, 1)); // extended RAM
    }

    #[test]
    fn int15_c201_reset_reports_present_standard_mouse() {
        // C201 resets the PS/2 mouse: BH=0x00 (standard device id), BL=0xAA (the
        // reset-complete signature drivers probe for), AH=0x00, CF clear.
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0xC201);
        m.cpu.registers.set_ebx(0xFFFF);
        m.handle_int15();
        assert_eq!(m.cpu.registers.ebx() as u16, 0x00AA, "BH=00 BL=AA");
        assert_eq!((m.cpu.registers.eax() as u16 >> 8) as u8, 0x00, "AH=00");
    }

    #[test]
    fn int15_c204_reports_standard_device_type() {
        // C204 get device type: BH=0x00 (standard PS/2 mouse), AH=0x00.
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0xC204);
        m.cpu.registers.set_ebx(0xFF00);
        m.handle_int15();
        assert_eq!((m.cpu.registers.ebx() as u16 >> 8) as u8, 0x00, "BH=00");
        assert_eq!((m.cpu.registers.eax() as u16 >> 8) as u8, 0x00, "AH=00");
    }

    #[test]
    fn int15_c206_status_describes_an_enabled_mouse() {
        // C206 BH=00 returns the three status bytes. BL bit5 = mouse enabled.
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0xC206);
        m.cpu.registers.set_ebx(0x0000); // BH=00
        m.handle_int15();
        assert_eq!(m.cpu.registers.ebx() as u8 & 0x20, 0x20, "BL bit5 enabled");
    }

    #[test]
    fn int15_c207_set_handler_stores_pointer_and_succeeds() {
        // C207 (set device handler) now registers the ES:BX far pointer in the EBDA
        // and returns success (AH=0, CF clear). The pointer is stored but not yet
        // called (no mouse-packet producer is wired).
        let mut m = int15_machine(16);
        m.cpu
            .registers
            .set_segment(SegmentIndex::Es, SegmentRegister::real(0xABCD));
        m.cpu.registers.set_ebx(0x0042);
        m.cpu.registers.set_eax(0xC207);
        m.handle_int15();
        assert_eq!(
            (m.cpu.registers.eax() as u16 >> 8) as u8,
            0x00,
            "AH=0 success"
        );
        let base = (u32::from(EBDA_SEGMENT) << 4) + EBDA_MOUSE_HANDLER_OFF;
        assert_eq!(read_u16(&mut m, base), 0x0042);
        assert_eq!(read_u16(&mut m, base + 2), 0xABCD);
    }

    #[test]
    fn int15_c208_still_reports_unsupported() {
        // C208 (read raw device port) has no wired path: AH=0x86 unsupported.
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0xC208);
        m.handle_int15();
        assert_eq!(
            (m.cpu.registers.eax() as u16 >> 8) as u8,
            0x86,
            "AH=86 unsupported"
        );
    }

    #[test]
    fn int15_e820_rejects_a_bad_smap_signature() {
        let mut m = int15_machine(24);
        m.cpu.registers.set_eax(0xE820);
        m.cpu.registers.set_edx(0); // not 'SMAP'
        m.cpu.registers.set_ecx(20);
        m.handle_int15();
        // EAX must not be rewritten to 'SMAP' when the call is rejected.
        assert_ne!(m.cpu.registers.eax(), 0x534D_4150);
    }

    #[test]
    fn int14_status_reports_uart_registers() {
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0x0300); // AH=03h read status
        m.cpu.registers.set_edx(0); // COM1
        m.handle_int14();
        // LSR reads 0x60 (THRE|TEMT) on the idle UART; MSR reads 0x00.
        assert_eq!(
            (m.cpu.registers.eax() >> 8) as u8,
            0x60,
            "line status in AH"
        );
        assert_eq!(m.cpu.registers.eax() as u8, 0x00, "modem status in AL");
    }

    #[test]
    fn int14_send_writes_a_byte_to_the_uart() {
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0x0158); // AH=01h send AL='X'
        m.cpu.registers.set_edx(0);
        m.handle_int14();
        assert_eq!(
            m.serial.output(),
            b"X",
            "byte reached the UART capture sink"
        );
        // THRE is always set, so the send succeeds with bit7 clear.
        assert_eq!((m.cpu.registers.eax() >> 8) as u8 & 0x80, 0, "no timeout");
    }

    #[test]
    fn int14_unwired_port_times_out() {
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0x0300);
        // INT 14h only services COM1 (DX=0); the COM2 hardware exists but the
        // BIOS service does not drive it, so DX=1 reads as a timeout.
        m.cpu.registers.set_edx(1);
        m.handle_int14();
        assert_eq!(
            (m.cpu.registers.eax() >> 8) as u8 & 0x80,
            0x80,
            "timeout bit set"
        );
    }

    #[test]
    fn int17_print_captures_and_reports_ready() {
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0x0050); // AH=00h print AL='P'
        m.cpu.registers.set_edx(0); // LPT1
        m.handle_int17();
        assert_eq!(m.lpt_output(), b"P", "byte reached the LPT capture sink");
        // An always-ready printer reports 0x90: not busy, selected, no error/timeout.
        assert_eq!(
            (m.cpu.registers.eax() >> 8) as u8,
            0x90,
            "ready status in AH"
        );
    }

    #[test]
    fn int17_status_reports_ready_printer() {
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0x0200); // AH=02h read status
        m.cpu.registers.set_edx(0);
        m.handle_int17();
        assert_eq!(
            (m.cpu.registers.eax() >> 8) as u8,
            0x90,
            "ready status in AH"
        );
        assert!(m.lpt_output().is_empty(), "status query prints nothing");
    }

    #[test]
    fn bda_seeds_serial_and_parallel_port_bases() {
        let m = int15_machine(16);
        assert_eq!(
            m.memory.read_u16(0x400).unwrap(),
            0x03f8,
            "COM1 base at 0040:0000"
        );
        assert_eq!(
            m.memory.read_u16(0x408).unwrap(),
            0x0378,
            "LPT1 base at 0040:0008"
        );
    }

    #[test]
    fn int15_a20_status_enable_and_disable() {
        let mut m = int15_machine(16);
        // The 8042 output port defaults to A20 on, so status reads enabled.
        m.cpu.registers.set_eax(0x2402);
        m.handle_int15();
        assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=0 success");
        assert_eq!(m.cpu.registers.eax() as u8, 0x01, "A20 enabled by default");
        // AH=2400h disable.
        m.cpu.registers.set_eax(0x2400);
        m.handle_int15();
        assert!(
            !m.keyboard.a20_enabled(),
            "8042 A20 state off after disable"
        );
        m.cpu.registers.set_eax(0x2402);
        m.handle_int15();
        assert_eq!(m.cpu.registers.eax() as u8, 0x00, "status reports disabled");
        // AH=2401h enable.
        m.cpu.registers.set_eax(0x2401);
        m.handle_int15();
        assert!(m.keyboard.a20_enabled(), "8042 A20 state on after enable");
    }

    #[test]
    fn int15_a20_query_support_reports_both_methods() {
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0x2403);
        m.handle_int15();
        assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=0 success");
        // Bit 0 keyboard controller, bit 1 port 0x92.
        assert_eq!(
            m.cpu.registers.ebx() as u16,
            0x0003,
            "both A20 methods supported"
        );
    }

    #[test]
    fn port_92_and_int15_a20_stay_coherent() {
        let mut m = int15_machine(16);
        // Disable A20 through the fast-A20 port; it reads back off.
        {
            let mut bus = m.make_bus();
            bus.write_io(0x0092, BusWidth::Byte, 0x00).unwrap();
            assert_eq!(
                bus.read_io(0x0092, BusWidth::Byte).unwrap(),
                0x00,
                "port 0x92 A20 off"
            );
        }
        assert!(!m.keyboard.a20_enabled(), "8042 agrees A20 is off");
        m.cpu.registers.set_eax(0x2402);
        m.handle_int15();
        assert_eq!(
            m.cpu.registers.eax() as u8,
            0x00,
            "INT 15h status agrees A20 is off"
        );
        // Enable through the port again; bit 1 reads back set.
        {
            let mut bus = m.make_bus();
            bus.write_io(0x0092, BusWidth::Byte, 0x02).unwrap();
            assert_eq!(
                bus.read_io(0x0092, BusWidth::Byte).unwrap(),
                0x02,
                "port 0x92 A20 on"
            );
        }
        assert!(m.keyboard.a20_enabled(), "8042 agrees A20 is on");
    }

    #[test]
    fn a20_off_folds_the_hma_onto_low_memory() {
        let mut m = int15_machine(16);
        // A20 is on by default, so 0x0 and 0x100000 are distinct cells.
        {
            let mut bus = m.make_bus();
            bus.write_memory(0x0, BusWidth::Byte, 0xAA, BusAccessKind::DataWrite)
                .unwrap();
            bus.write_memory(0x10_0000, BusWidth::Byte, 0xBB, BusAccessKind::DataWrite)
                .unwrap();
            assert_eq!(
                bus.read_memory(0x10_0000, BusWidth::Byte, BusAccessKind::DataRead)
                    .unwrap(),
                0xBB,
                "a distinct extended cell with A20 on"
            );
        }
        // Close the gate: a write to 0x100000 now folds onto 0x0.
        m.keyboard.set_a20(false);
        {
            let mut bus = m.make_bus();
            bus.write_memory(0x10_0000, BusWidth::Byte, 0xCC, BusAccessKind::DataWrite)
                .unwrap();
            assert_eq!(
                bus.read_memory(0x0, BusWidth::Byte, BusAccessKind::DataRead)
                    .unwrap(),
                0xCC,
                "the HMA write reached 0x0 through the closed gate"
            );
        }
        // Reopen the gate: the real extended cell was never touched (still 0xBB).
        m.keyboard.set_a20(true);
        {
            let mut bus = m.make_bus();
            assert_eq!(
                bus.read_memory(0x10_0000, BusWidth::Byte, BusAccessKind::DataRead)
                    .unwrap(),
                0xBB,
                "the aliased write left the extended cell alone"
            );
        }
    }

    #[test]
    fn a20_off_folds_a_split_word_in_the_hma() {
        let mut m = int15_machine(16);
        m.keyboard.set_a20(false);
        let mut bus = m.make_bus();
        // 0x100001 is odd, so the word splits; with the gate closed each byte
        // folds down by 0x100000, landing the pair at 0x1 and 0x2. (The byte just
        // below 1 MiB, 0xFFFFF, is BIOS ROM, so the genuinely straddling write is
        // not observable there; the odd HMA word proves the same split masking.)
        bus.write_memory(0x10_0001, BusWidth::Word, 0xBEEF, BusAccessKind::DataWrite)
            .unwrap();
        assert_eq!(
            bus.read_memory(0x1, BusWidth::Byte, BusAccessKind::DataRead)
                .unwrap(),
            0xEF,
            "low byte folded to 0x1"
        );
        assert_eq!(
            bus.read_memory(0x2, BusWidth::Byte, BusAccessKind::DataRead)
                .unwrap(),
            0xBE,
            "high byte folded to 0x2"
        );
        assert_eq!(
            bus.read_memory(0x10_0001, BusWidth::Word, BusAccessKind::DataRead)
                .unwrap(),
            0xBEEF,
            "the folded word reads back through the HMA alias"
        );
    }

    #[test]
    fn a20_off_folds_a_split_dword_and_reads_back() {
        let mut m = int15_machine(16);
        m.keyboard.set_a20(false);
        let mut bus = m.make_bus();
        // 0x100001 is not 4-aligned, so the dword splits into four bytes, each
        // folding down by 0x100000 to 0x1..0x4.
        bus.write_memory(
            0x10_0001,
            BusWidth::Dword,
            0xDEAD_BEEF,
            BusAccessKind::DataWrite,
        )
        .unwrap();
        // The read side folds too: the dword reads back through the alias.
        assert_eq!(
            bus.read_memory(0x10_0001, BusWidth::Dword, BusAccessKind::DataRead)
                .unwrap(),
            0xDEAD_BEEF,
            "the dword reads back through the HMA alias"
        );
        // The low-memory bytes hold the little-endian image.
        assert_eq!(
            bus.read_memory(0x1, BusWidth::Byte, BusAccessKind::DataRead)
                .unwrap(),
            0xEF,
            "byte 0 folded to 0x1"
        );
        assert_eq!(
            bus.read_memory(0x4, BusWidth::Byte, BusAccessKind::DataRead)
                .unwrap(),
            0xDE,
            "byte 3 folded to 0x4"
        );
    }

    #[test]
    fn a20_on_keeps_a_split_word_in_the_hma() {
        let mut m = int15_machine(16); // A20 on by default
        let mut bus = m.make_bus();
        bus.write_memory(0x10_0001, BusWidth::Word, 0xBEEF, BusAccessKind::DataWrite)
            .unwrap();
        // Low memory is untouched; the word stays at the real HMA cells.
        assert_eq!(
            bus.read_memory(0x1, BusWidth::Byte, BusAccessKind::DataRead)
                .unwrap(),
            0x00,
            "0x1 untouched with A20 on"
        );
        assert_eq!(
            bus.read_memory(0x10_0001, BusWidth::Word, BusAccessKind::DataRead)
                .unwrap(),
            0xBEEF,
            "the word stayed in the HMA"
        );
    }

    #[test]
    fn hma_is_claimable_and_a20_gates_its_visibility() {
        let mut m = int15_machine(16);
        // Claim the HMA through XMS function 01h (DX=0xFFFF: the whole HMA).
        m.cpu.registers.set_eax(0x0100);
        m.cpu.registers.set_edx(0xFFFF);
        m.handle_xms();
        assert_eq!(
            m.cpu.registers.eax() as u16,
            0x0001,
            "AH=01h claim succeeds"
        );

        // With A20 on (default), the HMA is real RAM: write FFFF:0010 (0x100000)
        // and read it back.
        {
            let mut bus = m.make_bus();
            bus.write_memory(0x10_0000, BusWidth::Word, 0x1234, BusAccessKind::DataWrite)
                .unwrap();
            assert_eq!(
                bus.read_memory(0x10_0000, BusWidth::Word, BusAccessKind::DataRead)
                    .unwrap(),
                0x1234,
                "the HMA holds the write with A20 open"
            );
        }

        // A second claim is refused while the HMA is held.
        m.cpu.registers.set_eax(0x0100);
        m.cpu.registers.set_edx(0xFFFF);
        m.handle_xms();
        assert_eq!(m.cpu.registers.eax() as u16, 0x0000, "second claim fails");
        assert_eq!(
            m.cpu.registers.ebx() as u8,
            xms::err::HMA_IN_USE,
            "BL = HMA already in use"
        );

        // Closing A20 hides the HMA: 0x100000 folds down to low memory (0x0,
        // still zero), so the previously written value is no longer visible.
        m.keyboard.set_a20(false);
        {
            let mut bus = m.make_bus();
            assert_eq!(
                bus.read_memory(0x10_0000, BusWidth::Word, BusAccessKind::DataRead)
                    .unwrap(),
                0x0000,
                "with A20 closed the HMA folds away and 0x100000 reads low memory"
            );
        }
    }

    #[test]
    fn xms_install_check_reports_present_and_entry_point() {
        let mut m = int15_machine(16);
        // INT 2Fh AX=4300h: AL = 80h means XMS present.
        m.cpu.registers.set_eax(0x4300);
        assert!(m.handle_int2f(), "4300h handled");
        assert_eq!(m.cpu.registers.eax() as u8, 0x80, "AL=80h XMS present");
        // INT 2Fh AX=4310h: ES:BX -> the entry stub. The stub bytes are INT 66h; RETF.
        m.cpu.registers.set_eax(0x4310);
        assert!(m.handle_int2f(), "4310h handled");
        let es = m.cpu.registers.segment(SegmentIndex::Es).selector;
        let bx = m.cpu.registers.ebx() as u16;
        assert_eq!(es, XMS_ENTRY_SEG);
        assert_eq!(bx, XMS_ENTRY_OFF);
        let linear = (u32::from(es) << 4) + u32::from(bx);
        assert_eq!(
            m.read_physical_u8(linear),
            0xCD,
            "stub starts with the INT opcode"
        );
        assert_eq!(m.read_physical_u8(linear + 1), XMS_INT_VECTOR, "INT 66h");
        assert_eq!(m.read_physical_u8(linear + 2), 0xCB, "RETF");
    }

    #[test]
    fn xms_version_reports_three_oh_oh() {
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0x0000); // AH=00h get version
        m.handle_xms();
        assert_eq!(m.cpu.registers.eax() as u16, 0x0300, "AX=3.00");
        assert_eq!(m.cpu.registers.ebx() as u16, xms::DRIVER_REVISION, "BX rev");
        assert_eq!(m.cpu.registers.edx() as u16, 0x0001, "DX=1 HMA exists");
    }

    #[test]
    fn xms_allocate_lock_unlock_free_round_trip() {
        let mut m = int15_machine(16);
        // Query free first (AH=08h): record the total.
        m.cpu.registers.set_eax(0x0800);
        m.handle_xms();
        let total_before = m.cpu.registers.edx() as u16;
        assert!(
            total_before > 0,
            "extended memory is free on a 16 MB machine"
        );

        // Allocate 64 KB (AH=09h, DX=KB).
        m.cpu.registers.set_eax(0x0900);
        m.cpu.registers.set_edx(64);
        m.handle_xms();
        assert_eq!(m.cpu.registers.eax() as u16, 0x0001, "alloc success");
        let handle = m.cpu.registers.edx() as u16;
        assert_ne!(handle, 0);

        // Lock (AH=0Ch, DX=handle) -> DX:BX linear address.
        m.cpu.registers.set_eax(0x0C00);
        m.cpu.registers.set_edx(handle.into());
        m.handle_xms();
        assert_eq!(m.cpu.registers.eax() as u16, 0x0001, "lock success");
        let linear = (u32::from(m.cpu.registers.edx() as u16) << 16)
            | u32::from(m.cpu.registers.ebx() as u16);
        assert!(
            linear >= 0x10_0000,
            "locked EMB linear address is above 1 MB, got {linear:#x}"
        );

        // Query free now shows 64 KB less.
        m.cpu.registers.set_eax(0x0800);
        m.handle_xms();
        let total_locked = m.cpu.registers.edx() as u16;
        assert_eq!(total_before - total_locked, 64, "alloc consumed 64 KB");

        // A locked block cannot be freed (AH=0Ah): AX=0, BL=ABh.
        m.cpu.registers.set_eax(0x0A00);
        m.cpu.registers.set_edx(handle.into());
        m.handle_xms();
        assert_eq!(m.cpu.registers.eax() as u16, 0x0000, "free of locked fails");
        assert_eq!(m.cpu.registers.ebx() as u8, xms::err::BLOCK_LOCKED);

        // Unlock (AH=0Dh) then free (AH=0Ah) succeed.
        m.cpu.registers.set_eax(0x0D00);
        m.cpu.registers.set_edx(handle.into());
        m.handle_xms();
        assert_eq!(m.cpu.registers.eax() as u16, 0x0001, "unlock success");
        m.cpu.registers.set_eax(0x0A00);
        m.cpu.registers.set_edx(handle.into());
        m.handle_xms();
        assert_eq!(m.cpu.registers.eax() as u16, 0x0001, "free success");

        // The KB came back.
        m.cpu.registers.set_eax(0x0800);
        m.handle_xms();
        assert_eq!(m.cpu.registers.edx() as u16, total_before, "KB returned");
    }

    #[test]
    fn xms_global_enable_a20_is_visible_at_port_92() {
        let mut m = int15_machine(16);
        // Disable A20 first so the enable is a real transition.
        m.cpu.registers.set_eax(0x0400); // AH=04h global disable A20
        m.handle_xms();
        assert!(!m.keyboard.a20_enabled(), "A20 off after global disable");

        // Global enable (AH=03h).
        m.cpu.registers.set_eax(0x0300);
        m.handle_xms();
        assert_eq!(m.cpu.registers.eax() as u16, 0x0001, "enable success");

        // Query A20 (AH=07h) reports enabled.
        m.cpu.registers.set_eax(0x0700);
        m.handle_xms();
        assert_eq!(m.cpu.registers.eax() as u16, 0x0001, "A20 query enabled");

        // And it is visible at port 0x92 bit 1, the single source of truth.
        let mut bus = m.make_bus();
        assert_eq!(
            bus.read_io(0x0092, BusWidth::Byte).unwrap() & 0x02,
            0x02,
            "port 0x92 bit1 set"
        );
    }

    #[test]
    fn xms_move_copies_between_conventional_and_emb() {
        let mut m = int15_machine(16);
        // Allocate a 64 KB EMB and lock it to learn its linear base.
        m.cpu.registers.set_eax(0x0900);
        m.cpu.registers.set_edx(64);
        m.handle_xms();
        let handle = m.cpu.registers.edx() as u16;
        m.cpu.registers.set_eax(0x0C00);
        m.cpu.registers.set_edx(handle.into());
        m.handle_xms();
        let emb_base = (u32::from(m.cpu.registers.edx() as u16) << 16)
            | u32::from(m.cpu.registers.ebx() as u16);

        // Put a known 8-byte pattern in conventional memory at 0x2000:0000.
        let src_seg = 0x2000u16;
        let src_lin = u32::from(src_seg) << 4;
        let pattern = [0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03, 0x04];
        m.write_guest_block(src_lin, &pattern);

        // Build the move descriptor at 0x3000:0000: len=8, src handle 0 / seg:off
        // 2000:0000, dst handle / offset into the EMB.
        let desc_lin = 0x3000u32 << 4;
        m.write_guest_block(desc_lin, &8u32.to_le_bytes()); // length
        m.write_guest_block(desc_lin + 4, &0u16.to_le_bytes()); // src handle 0
        let src_ptr = u32::from(src_seg) << 16; // seg:off real pointer (offset 0)
        m.write_guest_block(desc_lin + 6, &src_ptr.to_le_bytes());
        m.write_guest_block(desc_lin + 10, &handle.to_le_bytes()); // dst handle
        m.write_guest_block(desc_lin + 12, &0u32.to_le_bytes()); // dst offset 0

        // DS:SI -> descriptor. AH=0Bh move.
        m.cpu
            .registers
            .set_segment(SegmentIndex::Ds, SegmentRegister::real(0x3000));
        m.cpu.registers.set_esi(0);
        m.cpu.registers.set_eax(0x0B00);
        m.handle_xms();
        assert_eq!(m.cpu.registers.eax() as u16, 0x0001, "move success");

        // The pattern landed at the EMB base.
        let got = m.read_guest_block(emb_base, 8);
        assert_eq!(got.as_slice(), &pattern, "EMB now holds the pattern");
    }

    #[test]
    fn xms_odd_length_move_is_rejected() {
        let mut m = int15_machine(16);
        let desc_lin = 0x3000u32 << 4;
        m.write_guest_block(desc_lin, &7u32.to_le_bytes()); // odd length
        m.write_guest_block(desc_lin + 4, &0u16.to_le_bytes());
        m.write_guest_block(desc_lin + 6, &0u32.to_le_bytes());
        m.write_guest_block(desc_lin + 10, &0u16.to_le_bytes());
        m.write_guest_block(desc_lin + 12, &0u32.to_le_bytes());
        m.cpu
            .registers
            .set_segment(SegmentIndex::Ds, SegmentRegister::real(0x3000));
        m.cpu.registers.set_esi(0);
        m.cpu.registers.set_eax(0x0B00);
        m.handle_xms();
        assert_eq!(m.cpu.registers.eax() as u16, 0x0000, "odd length fails");
        assert_eq!(m.cpu.registers.ebx() as u8, xms::err::INVALID_LENGTH);
    }

    #[test]
    fn int1a_set_and_read_date_round_trips() {
        let mut m = int15_machine(16);
        // AH=05h set date: CH/CL century/year BCD, DH/DL month/day BCD -> 2021-07-15.
        m.cpu.registers.set_eax(0x0500);
        m.cpu.registers.set_ecx(0x2021);
        m.cpu.registers.set_edx(0x0715);
        m.handle_int1a();
        // AH=04h read date back.
        m.cpu.registers.set_eax(0x0400);
        m.handle_int1a();
        assert_eq!(m.cpu.registers.ecx() as u16, 0x2021);
        assert_eq!(m.cpu.registers.edx() as u16, 0x0715);
    }

    #[test]
    fn int1a_date_persists_a_non_default_century() {
        let mut m = int15_machine(16);
        // AH=05h set date to 1999-12-31 (CH=century 0x19, CL=year 0x99).
        m.cpu.registers.set_eax(0x0500);
        m.cpu.registers.set_ecx(0x1999);
        m.cpu.registers.set_edx(0x1231);
        m.handle_int1a();
        // The century reached CMOS 0x32 (binary 19), not just the in-memory year.
        assert_eq!(m.rtc.century(), 19, "century persisted to CMOS 0x32");
        // AH=04h reads the full BCD date back through the century accessor.
        m.cpu.registers.set_eax(0x0400);
        m.handle_int1a();
        assert_eq!(
            m.cpu.registers.ecx() as u16,
            0x1999,
            "century and year round-trip"
        );
        assert_eq!(m.cpu.registers.edx() as u16, 0x1231);
    }

    #[test]
    fn int1a_set_and_read_time_round_trips() {
        let mut m = int15_machine(16);
        // AH=03h set time: CH/CL hours/minutes BCD, DH seconds BCD -> 13:45:30.
        m.cpu.registers.set_eax(0x0300);
        m.cpu.registers.set_ecx(0x1345);
        m.cpu.registers.set_edx(0x3000);
        m.handle_int1a();
        m.cpu.registers.set_eax(0x0200);
        m.handle_int1a();
        assert_eq!(m.cpu.registers.ecx() as u16, 0x1345);
        assert_eq!((m.cpu.registers.edx() as u16) >> 8, 0x30);
    }

    #[test]
    fn int1a_day_counter_matches_calendar() {
        let mut m = int15_machine(16);
        // 1980-01-02 is day 1 since the 1980-01-01 epoch.
        m.cpu.registers.set_eax(0x0500);
        m.cpu.registers.set_ecx(0x1980);
        m.cpu.registers.set_edx(0x0102);
        m.handle_int1a();
        m.cpu.registers.set_eax(0x0A00);
        m.handle_int1a();
        assert_eq!(m.cpu.registers.ecx() as u16, 1);
    }

    #[test]
    fn days_since_1980_handles_leap_years() {
        assert_eq!(days_since_1980(1980, 1, 1), 0);
        assert_eq!(days_since_1980(1980, 3, 1), 60); // 1980 is a leap year (31+29)
        assert_eq!(days_since_1980(1981, 1, 1), 366);
    }

    #[test]
    fn int1a_set_day_counter_round_trips() {
        let mut m = int15_machine(16);
        // AH=0Bh latches CX into the BDA scratch word; it reads back unchanged.
        m.cpu.registers.set_eax(0x0B00);
        m.cpu.registers.set_ecx(0x1234);
        m.handle_int1a();
        assert_eq!(m.memory.read_u16(BDA_DAY_COUNT).unwrap(), 0x1234);
        // CF clear: the call succeeded.
        let ss = m.cpu.registers.segment(SegmentIndex::Ss).base;
        let sp = m.cpu.registers.esp() as u16;
        let flags = m
            .memory
            .read_u16((ss + u32::from(sp.wrapping_add(4))) as usize)
            .unwrap();
        assert_eq!(flags & 0x0001, 0, "CF clear");
    }

    #[test]
    fn int13_drive_parameters_report_real_floppy_count() {
        let mut m = int15_machine(16);
        m.mount_floppy(vec![0u8; 1_474_560]).unwrap(); // 1.44 MB
        m.cpu.registers.set_eax(0x0800);
        m.cpu.registers.set_edx(0x0000); // DL=0 drive A:
        m.handle_int13();
        // One drive is mounted: DL reports 1, derived from the equipment word.
        assert_eq!(m.cpu.registers.edx() as u8, 0x01, "DL = floppy count");
        assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH = success");
    }

    #[test]
    fn int13_drive_parameters_reject_fixed_disk() {
        let mut m = int15_machine(16);
        m.mount_floppy(vec![0u8; 1_474_560]).unwrap();
        m.cpu.registers.set_eax(0x0800);
        m.cpu.registers.set_edx(0x0080); // DL=0x80 fixed disk, none modeled
        m.handle_int13();
        assert_eq!(
            (m.cpu.registers.eax() >> 8) as u8,
            0x80,
            "AH = timeout/no drive"
        );
    }

    #[test]
    fn int13_dasd_type_honors_drive_presence() {
        let mut m = int15_machine(16);
        m.mount_floppy(vec![0u8; 1_474_560]).unwrap();
        // DL=0 with a floppy mounted: AH=01 (floppy, no change line), CF clear.
        m.cpu.registers.set_eax(0x1500);
        m.cpu.registers.set_edx(0x0000);
        m.handle_int13();
        assert_eq!(
            (m.cpu.registers.eax() >> 8) as u8,
            0x01,
            "AH = floppy, no change line"
        );
        // DL=1 is an absent second floppy: AH=00 (no such drive).
        m.cpu.registers.set_eax(0x1500);
        m.cpu.registers.set_edx(0x0001);
        m.handle_int13();
        assert_eq!(
            (m.cpu.registers.eax() >> 8) as u8,
            0x00,
            "AH = no such drive"
        );
    }

    #[test]
    fn bda_seeds_serial_parallel_and_video_state() {
        let m = int15_machine(16);
        // Serial/parallel base tables: COM1 + COM2 and LPT1 + LPT2 are wired.
        assert_eq!(m.memory.read_u16(0x400).unwrap(), 0x03f8); // COM1
        assert_eq!(m.memory.read_u16(0x402).unwrap(), 0x02f8); // COM2
        assert_eq!(m.memory.read_u16(0x408).unwrap(), 0x0378); // LPT1
        assert_eq!(m.memory.read_u16(0x40a).unwrap(), 0x0278); // LPT2
        // Timeout tables across all four ports each.
        assert_eq!(m.memory.read_u8(0x47f).unwrap(), 0x01); // COM4 timeout
        assert_eq!(m.memory.read_u8(0x47b).unwrap(), 0x14); // LPT4 timeout
        // Static video-state block and the system flags.
        assert_eq!(m.memory.read_u16(0x44c).unwrap(), 0x1000); // regen page size
        assert_eq!(m.memory.read_u8(0x485).unwrap(), 16); // char cell height
        assert_eq!(m.memory.read_u8(0x475).unwrap(), 0); // no fixed disks
        assert_eq!(m.memory.read_u16(0x472).unwrap(), 0x1234); // warm-boot magic
    }

    #[test]
    fn com2_scratch_round_trips_through_the_bus() {
        // A write then read of the COM2 scratch register (0x2FF) routes through the
        // serial2 port arm exactly the way COM1's (0x3FF) does.
        let mut m = int15_machine(16);
        let mut bus = m.make_bus();
        bus.write_io(0x02ff, BusWidth::Byte, 0xa5).unwrap();
        assert_eq!(bus.read_io(0x02ff, BusWidth::Byte).unwrap(), 0xa5);
        // COM1 stays separate: writing COM2 did not disturb COM1's scratch.
        assert_eq!(bus.read_io(0x03ff, BusWidth::Byte).unwrap(), 0x00);
    }

    #[test]
    fn lpt2_data_round_trips_through_the_bus() {
        // The LPT2 data latch at 0x278 reads back through the lpt2 port arm.
        let mut m = int15_machine(16);
        let mut bus = m.make_bus();
        bus.write_io(0x0278, BusWidth::Byte, 0x42).unwrap();
        assert_eq!(bus.read_io(0x0278, BusWidth::Byte).unwrap(), 0x42);
        // The LPT2 status port reports the always-ready idle byte.
        assert_eq!(bus.read_io(0x0279, BusWidth::Byte).unwrap(), 0xdf);
    }

    #[test]
    fn int29_writes_the_character_to_the_screen() {
        let mut m = int15_machine(16);
        // Place the cursor at the top-left so the byte lands at video offset 0.
        m.memory.write_u16(0x450, 0).unwrap();
        m.cpu.registers.set_eax(u32::from(b'Z'));
        m.handle_int29();
        assert_eq!(m.video.read_u8(0).unwrap(), b'Z');
    }

    #[test]
    fn int33_show_hide_cursor_counter_follows_microsoft_contract() {
        let mut m = int15_machine(16);
        // AX=0000 reset: the visibility counter starts hidden at -1.
        m.cpu.registers.set_eax(0x0000);
        m.handle_int33();
        assert_eq!(m.mouse.show_count, -1);
        // One Show (AX=0001) reaches the visible state (0).
        m.cpu.registers.set_eax(0x0001);
        m.handle_int33();
        assert_eq!(m.mouse.show_count, 0);
        // A second Show saturates at 0 rather than going positive.
        m.cpu.registers.set_eax(0x0001);
        m.handle_int33();
        assert_eq!(m.mouse.show_count, 0);
        // RBIL: N hides require N shows to unhide. Three hides take it to -3.
        for _ in 0..3 {
            m.cpu.registers.set_eax(0x0002);
            m.handle_int33();
        }
        assert_eq!(m.mouse.show_count, -3);
        // Two shows are not enough; the cursor stays hidden (< 0).
        for _ in 0..2 {
            m.cpu.registers.set_eax(0x0001);
            m.handle_int33();
        }
        assert!(m.mouse.show_count < 0);
        // The third show finally restores the visible state.
        m.cpu.registers.set_eax(0x0001);
        m.handle_int33();
        assert_eq!(m.mouse.show_count, 0);
    }

    #[test]
    fn int11_equipment_word_tracks_floppy_mount() {
        let mut m = int15_machine(16);
        // Mounting sets the floppy-installed bit; ejecting clears the floppy field.
        m.mount_floppy(vec![0u8; 1_474_560]).unwrap();
        m.cpu.registers.set_eax(0);
        m.handle_int11();
        assert_eq!(m.cpu.registers.eax() as u16 & 0x0001, 0x0001);
        m.eject_floppy();
        m.cpu.registers.set_eax(0);
        m.handle_int11();
        assert_eq!(m.cpu.registers.eax() as u16 & 0x00C1, 0x0000);
    }

    #[test]
    fn int10_1a_reports_vga_color_dcc() {
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0x1A00);
        m.handle_int10();
        assert_eq!(m.cpu.registers.eax() as u8, 0x1A); // AL = function supported
        assert_eq!(m.cpu.registers.ebx() as u8, 0x08); // BL = VGA colour DCC
    }

    #[test]
    fn int10_1b_fills_state_block_and_signals_vga() {
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0x0003); // mode 03h so the BDA has a known mode
        m.handle_int10();
        m.cpu.registers.set_eax(0x1B00); // ES:DI = 0:0 -> block at physical 0
        m.handle_int10();
        assert_eq!(m.cpu.registers.eax() as u8, 0x1B);
        assert_eq!(m.read_physical_u8(4), 0x03); // video mode at +4
        assert_eq!(m.read_physical_u8(0x25), 0x08); // active DCC
        assert_eq!(m.read_physical_u8(0x2A), 0x03); // 480 scan lines (VGA)
    }

    #[test]
    fn timing_factors_track_the_active_mode() {
        let mut machine = Machine::new(
            MachineProfile::gsw_386(1, izarravm_core::VideoCard::Et4000Ax),
            vec![0u8; BIOS_ROM_SIZE],
        )
        .unwrap();
        // Boot mode is 386 @ 22 MHz: the PIT factor is PIT_INPUT_HZ / 22 MHz.
        assert_eq!(machine.active_mode(), GswMode::Gsw386);
        assert!((machine.timing.pit_per_clock - PIT_INPUT_HZ as f64 / 22_000_000.0).abs() < 1e-9);
        // Switching to 586 @ 266 MHz recomputes the factor.
        machine.set_mode(GswMode::Gsw586);
        assert_eq!(machine.active_mode(), GswMode::Gsw586);
        assert!((machine.timing.pit_per_clock - PIT_INPUT_HZ as f64 / 266_000_000.0).abs() < 1e-9);
        // Super Slow (286) @ 8.33 MHz.
        machine.set_mode(GswMode::Gsw286);
        assert_eq!(machine.active_mode(), GswMode::Gsw286);
        assert!((machine.timing.pit_per_clock - PIT_INPUT_HZ as f64 / 8_333_333.0).abs() < 1e-9);
    }

    #[test]
    fn set_mode_drives_cpu_level_and_cache_table() {
        let mut machine = Machine::new(
            MachineProfile::gsw_386(1, izarravm_core::VideoCard::Et4000Ax),
            vec![0u8; BIOS_ROM_SIZE],
        )
        .unwrap();
        // The CPU boots at the full ISA so POST is never restricted, regardless of the
        // 386 boot mode, until the guest writes a Lotura mode.
        assert_eq!(machine.cpu.level(), CpuLevel::I586);

        machine.set_mode(GswMode::Gsw286);
        assert_eq!(machine.cpu.level(), CpuLevel::I286);
        assert_eq!(machine.cache_config(), (0, 0));

        machine.set_mode(GswMode::Gsw386);
        assert_eq!(machine.cpu.level(), CpuLevel::I386);
        assert_eq!(machine.cache_config(), (0, 64));

        machine.set_mode(GswMode::Gsw486);
        assert_eq!(machine.cpu.level(), CpuLevel::I486);
        assert_eq!(machine.cache_config(), (16, 128));

        machine.set_mode(GswMode::Gsw586);
        assert_eq!(machine.cpu.level(), CpuLevel::I586);
        assert_eq!(machine.cache_config(), (32, 512));
    }

    #[test]
    fn lotura_code_3_selects_286_mode() {
        assert_eq!(gsw_mode_from_code(3), Some(GswMode::Gsw286));
        assert_eq!(gsw_mode_code(GswMode::Gsw286), 3);
        assert_eq!(cpu_level_for_mode(GswMode::Gsw286), CpuLevel::I286);
    }

    fn rom_with_code(code: &[u8]) -> Vec<u8> {
        let mut rom = vec![0; BIOS_ROM_SIZE];
        rom[..code.len()].copy_from_slice(code);
        // The ROM IRET at offset 0xF000 (FF00:0000) the real izarra BIOS emits.
        // The host-intercepted BIOS service vectors return through it, so the
        // bare test ROM supplies it too.
        rom[0xF000] = 0xCF;
        rom[0xfff0..0xfff5].copy_from_slice(&[0xea, 0x00, 0x00, 0x00, 0xf0]);
        rom
    }

    #[test]
    fn injected_key_is_readable_on_port_0x60_and_requests_irq1() {
        // A bare machine: inject a scancode, then read it back through the bus the
        // way the CPU would, and confirm IRQ1 became pending on the PIC.
        let profile = MachineProfile::gsw_386(1, izarravm_core::VideoCard::Et4000Ax);
        let mut machine = Machine::new(profile, vec![0u8; BIOS_ROM_SIZE]).unwrap();
        machine.inject_key_scancodes(&[0x1e]); // 'A' make
        assert_eq!(machine.read_io_port_u8(0x60), 0x1e);
        assert!(machine.irq1_pending(), "injecting a key requests IRQ1");
    }

    /// Run a .COM that reads one key via INT 16h AH=00h and stores AX at DS:0x200,
    /// after injecting `scancodes`. Returns the value INT 16h handed the program.
    /// This is the editor's keyboard path end to end: 8042 -> IRQ1 -> INT 09h ISR
    /// -> BDA ring -> INT 16h read.
    fn int16_read_after(scancodes: &[u8]) -> u16 {
        // mov ah,0; int 16h; mov [0x200],ax; int 20h
        const PROG: [u8; 9] = [0xB4, 0x00, 0xCD, 0x16, 0xA3, 0x00, 0x02, 0xCD, 0x20];
        let mut machine =
            Machine::new_dos_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), &PROG)
                .unwrap();
        machine.inject_key_scancodes(scancodes);
        machine.run_until_halt_or_cycles(3_000_000).unwrap();
        read_u16(&mut machine, (u32::from(DOS_LOAD_SEGMENT) << 4) + 0x200)
    }

    #[test]
    fn int16_returns_extended_scancode_for_up_arrow() {
        // Up arrow is the bare scancode 0x48 (make) / 0xC8 (break); no 0xE0 prefix.
        // The layout table has no ASCII for it, so INT 16h returns scancode 0x48
        // with ASCII 0 -- the value a full-screen editor keys arrow navigation off.
        assert_eq!(int16_read_after(&[0x48, 0xC8]), 0x4800);
    }

    #[test]
    fn int16_emits_control_code_for_ctrl_s() {
        // Ctrl down, S, S up, Ctrl up. Holding Ctrl turns S into the DC3 control
        // code (0x13), the way a real BIOS does, so the editor reads Ctrl-S as a
        // single ring entry (scancode 0x1f, ASCII 0x13) with no modifier polling.
        assert_eq!(int16_read_after(&[0x1d, 0x1f, 0x9f, 0x9d]), 0x1f13);
    }

    /// Same path as `int16_read_after`, but the program reads with AH=10h (the
    /// enhanced read). Before the DOS keyboard ROM aliased AH=10h to the AH=00h
    /// reader, this fell through the int16 dispatch and returned stale AX.
    fn int16_enhanced_read_after(scancodes: &[u8]) -> u16 {
        // mov ah,0x10; int 16h; mov [0x200],ax; int 20h
        const PROG: [u8; 9] = [0xB4, 0x10, 0xCD, 0x16, 0xA3, 0x00, 0x02, 0xCD, 0x20];
        let mut machine =
            Machine::new_dos_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), &PROG)
                .unwrap();
        machine.inject_key_scancodes(scancodes);
        machine.run_until_halt_or_cycles(3_000_000).unwrap();
        read_u16(&mut machine, (u32::from(DOS_LOAD_SEGMENT) << 4) + 0x200)
    }

    #[test]
    fn int16_enhanced_read_matches_plain_read() {
        // AH=10h must hand a DOS program the same ring entry AH=00h does. Up
        // arrow gives scancode 0x48 / ASCII 0, the editor-navigation case.
        assert_eq!(int16_enhanced_read_after(&[0x48, 0xC8]), 0x4800);
        assert_eq!(
            int16_enhanced_read_after(&[0x48, 0xC8]),
            int16_read_after(&[0x48, 0xC8]),
        );
    }

    #[test]
    fn io_port_reports_last_post_write() {
        // mov al,0x42; out 0x80,al; hlt
        let mut machine = Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
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
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), [0u8; 8]).unwrap_err();

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
    fn int10_mode13h_routes_a000_through_chain4() {
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
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();

        assert_eq!(reason, StopReason::Halted);
        // Chain-4 routes the A0000 byte at offset 0x7B to plane 0x7B & 3 = 3 at
        // plane offset 0x7B >> 2 = 30.
        assert_eq!(machine.video().plane_byte(3, 30), 0x2a);
        assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
        assert!(machine.is_graphics_mode());
        assert!(machine.bus_trace().cycles().iter().any(|cycle| {
            cycle.kind == BusAccessKind::InterruptAcknowledge && cycle.address == 0x10
        }));
    }

    #[test]
    fn unittester_exit_command_stops_with_the_guest_code() {
        // index=REG_EXIT; data=42; command=CMD_EXIT.
        let rom = rom_with_code(&[
            0xB0, 0x0C, 0xE6, 0xE4, // mov al,12; out 0E4h,al  (index = REG_EXIT)
            0xB0, 0x2A, 0xE6, 0xE5, // mov al,42; out 0E5h,al  (exit code 42)
            0xB0, 0x03, 0xE6, 0xE6, // mov al,3;  out 0E6h,al  (CMD_EXIT)
            0xF4, // hlt (not reached)
        ]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::TestExit { code: 42 });
    }

    #[test]
    fn unittester_crc_command_matches_the_rust_helper() {
        // Program a 2x2 rectangle and issue CMD_CRC; the run loop computes it and
        // stores it at REG_CRC, where the guest (here, a bus read) can read it.
        let rom = rom_with_code(&[
            0xB0, 0x00, 0xE6, 0xE4, // index = REG_X (0)
            0xB0, 0x00, 0xE6, 0xE5, // X lo
            0xB0, 0x00, 0xE6, 0xE5, // X hi
            0xB0, 0x00, 0xE6, 0xE5, // Y lo
            0xB0, 0x00, 0xE6, 0xE5, // Y hi
            0xB0, 0x02, 0xE6, 0xE5, // W lo = 2
            0xB0, 0x00, 0xE6, 0xE5, // W hi
            0xB0, 0x02, 0xE6, 0xE5, // H lo = 2
            0xB0, 0x00, 0xE6, 0xE5, // H hi
            0xB0, 0x01, 0xE6, 0xE6, // CMD_CRC
            0xF4, // hlt
        ]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
        machine.run_until_halt_or_cycles(1_000_000).unwrap();

        let reported = with_bus(&mut machine, |bus| {
            bus.write_io(0xE4, BusWidth::Byte, 8).unwrap(); // index = REG_CRC
            let mut crc = [0u8; 4];
            for byte in &mut crc {
                *byte = bus.read_io(0xE5, BusWidth::Byte).unwrap() as u8;
            }
            u32::from_le_bytes(crc)
        });
        assert_eq!(reported, machine.screen_crc32(0, 0, 2, 2));
    }

    #[test]
    fn int10_ah0f_reports_mode_after_set() {
        // Set mode 13h, then AH=0Fh returns AL=mode, AH=columns.
        let rom = rom_with_code(&[
            0xB8, 0x13, 0x00, 0xCD, 0x10, // mov ax,0013h; int 10h (set mode 13h)
            0xB4, 0x0F, 0xCD, 0x10, // mov ah,0Fh; int 10h (get mode)
            0xF4,
        ]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        let ax = machine.cpu().registers.eax() as u16;
        assert_eq!(ax & 0xff, 0x13, "AL = current mode");
        assert_eq!(ax >> 8, 40, "AH = column count for mode 13h");
    }

    #[test]
    fn boot_image_starts_at_bios_loaded_boot_sector() {
        let mut machine = Machine::new_boot_image(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
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
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
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
        // Chain-4 routes the linear byte at offset N to plane N & 3 at plane
        // offset N >> 2, so the boot image's three drawn pixels land as:
        // 0 -> plane 0 @ 0, 319 -> plane 3 @ 79, 63680 -> plane 0 @ 15920.
        assert_eq!(machine.video().plane_byte(0, 0), 0x2a);
        assert_eq!(machine.video().plane_byte(3, 79), 0x13);
        assert_eq!(machine.video().plane_byte(0, 15920), 0x7f);
        assert!(results.records.iter().any(|record| {
            record.status == izarravm_firmware::SuiteRecordStatus::Pass
                && record.name == "sound.sb_16bit_dma"
        }));
    }

    #[test]
    fn boot_suite_timer_passes_at_native_266mhz() {
        // The boot suite is wall-time-bound: the timer test waits for ten IRQ0
        // edges and the PIT runs at a fixed rate regardless of the CPU clock. At
        // the 266 MHz native default the cycle budget must scale (clock_hz / 5,
        // about 200 ms) or the timer test never reaches its tick target.
        let profile = MachineProfile {
            cpu: GswMode::Gsw586,
            clock_hz: GswMode::Gsw586.clock_hz(),
            memory_mib: 16,
            video: VideoCard::Et4000Ax,
            sound_blaster: SoundBlasterConfig::default(),
            wss: WssConfig::default(),
            wait_states: WaitStateProfile::default(),
            address_pipelining: false,
            cache_enabled: false,
        };
        let budget = profile.clock_hz / 5;
        let mut machine =
            Machine::new_boot_image(profile, izarravm_firmware::X86_BOOT_TEST_IMAGE).unwrap();

        let reason = machine.run_until_halt_or_cycles(budget).unwrap();
        let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();

        assert_eq!(reason, StopReason::Halted);
        let timer = results
            .records
            .iter()
            .find(|record| record.name == "timer.irq0")
            .expect("timer.irq0 record present");
        assert_eq!(
            timer.status,
            izarravm_firmware::SuiteRecordStatus::Pass,
            "timer.irq0 must pass at 266 MHz with the scaled budget"
        );
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
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

        // Host path latches Margo as the active display.
        machine.set_margo_mode_640x480x8();
        assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);

        // A guest VGA mode-set must hand the display back to VGA.
        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
    }

    #[test]
    fn host_mode_set_selects_margo_lfb() {
        let mut machine = test_machine();
        assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);

        machine.set_margo_mode_640x480x8();

        assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);
        assert_eq!(machine.margo().display().width, 640);
        assert_eq!(machine.margo().display().height, 480);
    }

    #[test]
    fn int13_read_places_sector_in_memory() {
        // A 720 KB image whose first sector starts with a recognizable marker.
        let mut img = vec![0u8; 737_280];
        img[0] = 0xEB;
        img[1] = 0x55;
        // Stub: ES=0, BX=0x2000, read 1 sector at CHS(0,0,1) of drive 0 via INT 13h,
        // then halt. AX=0x0201 (AH=02 read, AL=01 sector), CX=0x0001 (cyl 0,
        // sector 1), DX=0x0000 (head 0, drive A:). The buffer sits well clear of
        // the IRET stub the BIOS keeps near 0x0600.
        let rom = rom_with_code(&[
            0x31, 0xC0, // xor ax, ax
            0x8E, 0xC0, // mov es, ax
            0xBB, 0x00, 0x20, // mov bx, 0x2000
            0xB8, 0x01, 0x02, // mov ax, 0x0201
            0xB9, 0x01, 0x00, // mov cx, 0x0001
            0xBA, 0x00, 0x00, // mov dx, 0x0000
            0xCD, 0x13, // int 13h
            0xF4, // hlt
        ]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
        machine.mount_floppy(img).unwrap();

        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        // The sector bytes landed at physical 0x2000.
        assert_eq!(machine.read_physical_u8(0x2000), 0xEB);
        assert_eq!(machine.read_physical_u8(0x2001), 0x55);
        // AH cleared, AL reports one sector read, CF clear on success.
        let ax = machine.cpu().registers.eax() as u16;
        assert_eq!(ax >> 8, 0x00);
        assert_eq!(ax & 0xff, 0x01);
        let flags = machine.cpu().registers.eflags;
        assert_eq!(flags & 0x0001, 0, "CF must be clear after a good read");
    }

    #[test]
    fn int10_pixel_write_read_round_trips_in_mode13h() {
        let mut m = int15_machine(16);
        m.video_mut().set_mode13h();
        // AH=0Ch write pixel: AL=colour 0x43 (bit7 clear = plain write), CX=col 5,
        // DX=row 2 -> framebuffer offset 2*320+5.
        m.cpu.registers.set_eax(0x0C43);
        m.cpu.registers.set_ecx(5);
        m.cpu.registers.set_edx(2);
        m.handle_int10();
        // AH=0Dh read the same pixel back into AL.
        m.cpu.registers.set_eax(0x0D00);
        m.cpu.registers.set_ecx(5);
        m.cpu.registers.set_edx(2);
        m.handle_int10();
        assert_eq!(
            m.cpu.registers.eax() as u8,
            0x43,
            "pixel reads back its colour"
        );
        // Mode 13h is a 256-color mode: AL is the full 8-bit colour, bit 7 included,
        // with no XOR. Writing 0x8F stores colour 0x8F (143), not an XOR.
        m.cpu.registers.set_eax(0x0C8F); // colour 0x8F, bit7 part of the value
        m.cpu.registers.set_ecx(5);
        m.cpu.registers.set_edx(2);
        m.handle_int10();
        m.cpu.registers.set_eax(0x0D00);
        m.cpu.registers.set_ecx(5);
        m.cpu.registers.set_edx(2);
        m.handle_int10();
        assert_eq!(
            m.cpu.registers.eax() as u8,
            0x8F,
            "high colours write directly, no bit-7 XOR in 256-colour mode"
        );
    }

    #[test]
    fn int10_write_string_places_chars_and_attr_in_text_buffer() {
        let mut m = int15_machine(16);
        m.video_mut().set_text_mode();
        // Place a 3-char string "Hi!" at ES:BP = 0x0000:0x4000 (physical 0x4000).
        m.write_physical_u8(0x4000, b'H');
        m.write_physical_u8(0x4001, b'i');
        m.write_physical_u8(0x4002, b'!');
        m.cpu
            .registers
            .set_segment(SegmentIndex::Es, SegmentRegister::real(0x0000));
        m.cpu.registers.set_ebp(0x4000);
        // AH=13h AL=01 (advance cursor, no attr bytes), BL=attr 0x1E, CX=3,
        // DH=row 4, DL=col 10.
        m.cpu.registers.set_eax(0x1301);
        m.cpu.registers.set_ebx(0x001E);
        m.cpu.registers.set_ecx(3);
        m.cpu.registers.set_edx((4 << 8) | 10);
        m.handle_int10();
        // The chars and attribute landed at row 4, col 10.. of the text buffer.
        let base = (4 * 80 + 10) * 2;
        assert_eq!(m.video().read_u8(base).unwrap(), b'H');
        assert_eq!(m.video().read_u8(base + 1).unwrap(), 0x1E);
        assert_eq!(m.video().read_u8(base + 2).unwrap(), b'i');
        assert_eq!(m.video().read_u8(base + 4).unwrap(), b'!');
        // AL bit 0 set leaves the BDA cursor at the end of the string (col 13).
        assert_eq!(m.memory.read_u16(0x450).unwrap(), (4 << 8) | 13);
    }

    #[test]
    fn int10_write_string_honors_interleaved_attribute_bytes() {
        let mut m = int15_machine(16);
        m.video_mut().set_text_mode();
        // AL bit 1 set: the source is char,attr,char,attr. "Ab" with attrs 0x12,0x34.
        m.write_physical_u8(0x5000, b'A');
        m.write_physical_u8(0x5001, 0x12);
        m.write_physical_u8(0x5002, b'b');
        m.write_physical_u8(0x5003, 0x34);
        m.cpu
            .registers
            .set_segment(SegmentIndex::Es, SegmentRegister::real(0x0000));
        m.cpu.registers.set_ebp(0x5000);
        m.cpu.registers.set_eax(0x1302); // AL bit1 = interleaved attrs, bit0 clear
        m.cpu.registers.set_ebx(0x0000);
        m.cpu.registers.set_ecx(2);
        m.cpu.registers.set_edx(0); // row 0, col 0
        m.handle_int10();
        assert_eq!(m.video().read_u8(0).unwrap(), b'A');
        assert_eq!(m.video().read_u8(1).unwrap(), 0x12);
        assert_eq!(m.video().read_u8(2).unwrap(), b'b');
        assert_eq!(m.video().read_u8(3).unwrap(), 0x34);
    }

    #[test]
    fn int10_save_restore_state_round_trips_the_bda_block() {
        let mut m = int15_machine(16);
        // AL=00 reports the buffer size in 64-byte blocks (96 bytes -> 2 blocks).
        m.cpu.registers.set_eax(0x1C00);
        m.handle_int10();
        assert_eq!(m.cpu.registers.ebx() as u16, 2, "two 64-byte blocks");
        assert_eq!(m.cpu.registers.eax() as u8, 0x1C);
        // Mark the BDA mode byte, save into ES:BX, change it, then restore.
        let _ = m.memory.write_u8(0x449, 0x12);
        m.cpu
            .registers
            .set_segment(SegmentIndex::Es, SegmentRegister::real(0x0000));
        m.cpu.registers.set_ebx(0x6000);
        m.cpu.registers.set_eax(0x1C01); // save
        m.cpu.registers.set_ecx(0x0007);
        m.handle_int10();
        // Corrupt the live BDA, then restore it from the saved buffer.
        let _ = m.memory.write_u8(0x449, 0x99);
        m.cpu.registers.set_ebx(0x6000);
        m.cpu.registers.set_eax(0x1C02); // restore
        m.handle_int10();
        assert_eq!(m.memory.read_u8(0x449).unwrap(), 0x12, "BDA mode restored");
    }

    #[test]
    fn int15_c0_reports_honest_feature_byte() {
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0xC000);
        m.handle_int15();
        assert_eq!(
            (m.cpu.registers.eax() >> 8) as u8,
            0x00,
            "AH = 00 on success"
        );
        // ES:BX points at the seeded config table.
        let es = m.cpu.registers.segment(SegmentIndex::Es).base;
        let bx = m.cpu.registers.ebx() as u16;
        let addr = es + u32::from(bx);
        let len = m.read_guest_word(addr);
        assert_eq!(len, 8, "table reports 8 bytes following");
        assert_eq!(m.read_physical_u8(addr + 2), 0xFC, "AT-class model byte");
        let feature1 = m.read_physical_u8(addr + 5);
        assert_eq!(feature1 & 0x40, 0x40, "second PIC present");
        assert_eq!(feature1 & 0x20, 0x20, "RTC present");
        assert_eq!(feature1 & 0x04, 0x04, "EBDA allocated");
        assert_eq!(
            feature1 & 0x10,
            0x00,
            "no AH=4Fh keyboard-intercept callout"
        );
        assert_eq!(feature1 & 0x08, 0x00, "wait-for-event not supported");
        assert_eq!(feature1 & 0x02, 0x00, "ISA bus, not Micro Channel");
    }

    #[test]
    fn int15_c1_returns_ebda_segment_and_size_byte() {
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0xC100);
        m.handle_int15();
        assert_eq!(
            m.cpu.registers.segment(SegmentIndex::Es).selector,
            0x9FC0,
            "ES = EBDA segment"
        );
        // The EBDA size byte at 0x9FC00 reports 1 KB, and INT 12h dropped to 639.
        assert_eq!(m.memory.read_u8(0x9FC00).unwrap(), 1, "EBDA size = 1 KB");
        assert_eq!(
            m.memory.read_u16(0x413).unwrap(),
            639,
            "conventional lowered"
        );
    }

    #[test]
    fn int13_ah05_format_track_fills_with_f6() {
        let mut m = int15_machine(16);
        m.mount_floppy(vec![0u8; 737_280]).unwrap(); // 720 KB, 9 spt
        // AH=05 AL=9 sectors, CH=3 (track 3), DH=1 (head 1), DL=0 (A:).
        m.cpu.registers.set_eax(0x0509);
        m.cpu.registers.set_ecx(0x0300); // CH=3, CL=0
        m.cpu.registers.set_edx(0x0100); // DH=1, DL=0
        m.handle_int13();
        assert_eq!(
            (m.cpu.registers.eax() >> 8) as u8,
            0x00,
            "AH = 00 on success"
        );
        // The BDA last-disk-status byte records success. (CF rides the IRET frame,
        // which a direct handler call has no real stack for; AH and 0040:0041 carry
        // the result either way.)
        assert_eq!(
            m.memory.read_u8(0x441).unwrap(),
            0x00,
            "disk status = success"
        );
        // A CHS read of that track returns the 0xF6 filler.
        let sector = m
            .floppy
            .as_ref()
            .unwrap()
            .read_sector(3, 1, 1)
            .unwrap()
            .to_vec();
        assert_eq!(sector[0], 0xF6);
        assert_eq!(sector[511], 0xF6);
    }

    #[test]
    fn int13_ah05_format_track_rejects_bad_track_and_fixed_disk() {
        let mut m = int15_machine(16);
        m.mount_floppy(vec![0u8; 737_280]).unwrap(); // 80 cylinders, 2 heads
        // Track 80 is off an 80-cylinder disk: AH=0Ch bad track.
        m.cpu.registers.set_eax(0x0509);
        m.cpu.registers.set_ecx(0x5000); // CH=0x50 = 80
        m.cpu.registers.set_edx(0x0000);
        m.handle_int13();
        assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x0C, "bad-track error");
        assert_eq!(m.memory.read_u8(0x441).unwrap(), 0x0C, "status = bad track");
        // The track was not formatted: its first sector is still zero, not 0xF6.
        assert_eq!(
            m.floppy.as_ref().unwrap().read_sector(0, 0, 1).unwrap()[0],
            0x00
        );
        // A fixed-disk unit (DL>=0x80) reports no such drive (AH=0x80).
        m.cpu.registers.set_eax(0x0509);
        m.cpu.registers.set_ecx(0x0000);
        m.cpu.registers.set_edx(0x0080); // DL = 0x80
        m.handle_int13();
        assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x80, "no fixed disk");
        assert_eq!(m.memory.read_u8(0x441).unwrap(), 0x80, "status = no drive");
    }

    /// A small hard-disk image whose first byte per sector marks the LBA, plus an
    /// otherwise-zero machine with the disk mounted as C:.
    fn machine_with_hdd(sectors: usize) -> Machine {
        let mut bytes = vec![0u8; sectors * 512];
        for s in 0..sectors {
            bytes[s * 512] = (s as u8).wrapping_add(0x10);
        }
        let mut m = int15_machine(16);
        m.mount_hdd(bytes);
        m
    }

    #[test]
    fn mount_hdd_seeds_the_bda_fixed_disk_count() {
        let m = machine_with_hdd(64);
        assert_eq!(m.memory.read_u8(0x475).unwrap(), 1, "one fixed disk");
    }

    #[test]
    fn int13_ah02_reads_a_hard_disk_sector_through_es_bx() {
        let mut m = machine_with_hdd(4032); // 16*63 = one cylinder of 1008, 4 cyls
        // Read LBA 63 (CHS cyl 0, head 1, sector 1). AL=1, CH=0, CL=1 (sector),
        // DH=1 (head), DL=0x80 (C:), ES:BX = 4000:0000 (physical 0x40000).
        m.cpu.registers.set_eax(0x0201);
        m.cpu.registers.set_ecx(0x0001); // CH=0, CL=1
        m.cpu.registers.set_edx(0x0180); // DH=1, DL=0x80
        m.cpu
            .registers
            .set_segment(SegmentIndex::Es, SegmentRegister::real(0x4000));
        m.cpu.registers.set_ebx(0x0000);
        m.handle_int13();
        assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=0 success");
        assert_eq!(m.cpu.registers.eax() as u8, 0x01, "AL=1 sector moved");
        // The marker for LBA 63 is 63 + 0x10.
        assert_eq!(m.read_physical_u8(0x4_0000), 63u8.wrapping_add(0x10));
    }

    #[test]
    fn int13_ah03_write_then_ah02_read_round_trips() {
        let mut m = machine_with_hdd(64);
        // Seed a pattern in a guest buffer at ES:BX = 2000:0000 (0x20000).
        for i in 0..512u32 {
            m.write_physical_u8(0x2_0000 + i, (i & 0xff) as u8 ^ 0x5A);
        }
        // Write LBA 0 (CHS 0,0,1): AH=03 AL=1, CH=0 CL=1, DH=0 DL=0x80.
        m.cpu.registers.set_eax(0x0301);
        m.cpu.registers.set_ecx(0x0001);
        m.cpu.registers.set_edx(0x0080);
        m.cpu
            .registers
            .set_segment(SegmentIndex::Es, SegmentRegister::real(0x2000));
        m.cpu.registers.set_ebx(0x0000);
        m.handle_int13();
        assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "write AH=0");
        assert!(m.hdd_dirty(), "the write marked the image dirty");

        // Read it back into a fresh buffer at 3000:0000.
        m.cpu.registers.set_eax(0x0201);
        m.cpu.registers.set_ecx(0x0001);
        m.cpu.registers.set_edx(0x0080);
        m.cpu
            .registers
            .set_segment(SegmentIndex::Es, SegmentRegister::real(0x3000));
        m.cpu.registers.set_ebx(0x0000);
        m.handle_int13();
        assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "read AH=0");
        for i in 0..512u32 {
            assert_eq!(m.read_physical_u8(0x3_0000 + i), (i & 0xff) as u8 ^ 0x5A);
        }
    }

    #[test]
    fn int13_ah08_reports_hard_disk_geometry() {
        let mut m = machine_with_hdd(4032); // 4 cylinders, 16 heads, 63 spt
        m.cpu.registers.set_eax(0x0800);
        m.cpu.registers.set_edx(0x0080); // DL = C:
        m.handle_int13();
        assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=0 success");
        let cx = m.cpu.registers.ecx() as u16;
        let dx = m.cpu.registers.edx() as u16;
        let cl = cx as u8;
        let ch = (cx >> 8) as u8;
        let sectors = cl & 0x3f;
        let max_cyl = u16::from(ch) | (u16::from(cl & 0xc0) << 2);
        assert_eq!(sectors, 63, "63 sectors per track");
        assert_eq!(max_cyl, 3, "max cylinder index = 4 - 1");
        assert_eq!((dx >> 8) as u8, 15, "max head index = 16 - 1");
        assert_eq!(dx as u8, 1, "one fixed disk in DL");
    }

    #[test]
    fn int13_ah15_reports_fixed_disk_dasd_and_capacity() {
        let mut m = machine_with_hdd(4032);
        m.cpu.registers.set_eax(0x1500);
        m.cpu.registers.set_edx(0x0080);
        m.handle_int13();
        assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x03, "AH=03 fixed disk");
        let cx = m.cpu.registers.ecx() as u16;
        let dx = m.cpu.registers.edx() as u16;
        let total = (u32::from(cx) << 16) | u32::from(dx);
        assert_eq!(total, 4032, "CX:DX = total sectors");
    }

    #[test]
    fn int13_hard_disk_read_past_end_sets_carry() {
        let mut m = machine_with_hdd(8); // 8 sectors, all on cylinder 0
        // Read at CHS that maps past the image (cyl 1 does not exist).
        m.cpu.registers.set_eax(0x0201);
        m.cpu.registers.set_ecx(0x0001); // sector 1
        m.cpu.registers.set_edx(0x0180); // head 1, DL=0x80
        m.cpu
            .registers
            .set_segment(SegmentIndex::Es, SegmentRegister::real(0x4000));
        m.cpu.registers.set_ebx(0x0000);
        m.handle_int13();
        // head 1 * 63 spt = LBA 63, past an 8-sector disk: sector-not-found.
        assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x04, "AH=04 not found");
        assert_eq!(m.memory.read_u8(0x474).unwrap(), 0x04, "fixed-disk status");
    }

    #[test]
    fn int13_ah41_edd_install_check() {
        let mut m = machine_with_hdd(64);
        m.cpu.registers.set_eax(0x4100);
        m.cpu.registers.set_ebx(0x55AA); // the documented input magic
        m.cpu.registers.set_edx(0x0080);
        m.handle_int13();
        assert_eq!(m.cpu.registers.ebx() as u16, 0xAA55, "BX=0xAA55 present");
        assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x30, "EDD version 3.0");
        assert_eq!(m.cpu.registers.ecx() as u16 & 0x0001, 0x0001, "ext access");
    }

    #[test]
    fn int13_ah42_extended_read_via_disk_address_packet() {
        let mut m = machine_with_hdd(64);
        // Build a Disk Address Packet at DS:SI = 5000:0000 (physical 0x50000):
        // size 16, reserved 0, blocks 1, reserved 0, buffer 6000:0000, LBA 7.
        let dap = 0x5_0000u32;
        m.write_physical_u8(dap, 16); // packet size
        m.write_physical_u8(dap + 2, 1); // block count
        // buffer offset (0) at 4-5, segment 0x6000 at 6-7.
        m.write_physical_u8(dap + 6, 0x00);
        m.write_physical_u8(dap + 7, 0x60);
        m.write_physical_u8(dap + 8, 7); // LBA low byte = 7
        m.cpu.registers.set_eax(0x4200);
        m.cpu.registers.set_edx(0x0080);
        m.cpu
            .registers
            .set_segment(SegmentIndex::Ds, SegmentRegister::real(0x5000));
        m.cpu.registers.set_esi(0x0000);
        m.handle_int13();
        assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=0 success");
        // The buffer at 0x60000 holds the LBA 7 marker (7 + 0x10).
        assert_eq!(m.read_physical_u8(0x6_0000), 7u8.wrapping_add(0x10));
        // The packet's block count was rewritten to 1 (sectors moved).
        assert_eq!(m.read_physical_u8(dap + 2), 1);
    }

    #[test]
    fn int13_ah48_extended_drive_params() {
        let mut m = machine_with_hdd(4032);
        let buf = 0x5_0000u32;
        m.cpu.registers.set_eax(0x4800);
        m.cpu.registers.set_edx(0x0080);
        m.cpu
            .registers
            .set_segment(SegmentIndex::Ds, SegmentRegister::real(0x5000));
        m.cpu.registers.set_esi(0x0000);
        m.handle_int13();
        assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=0 success");
        let total = (0..8u32).fold(0u64, |acc, i| {
            acc | (u64::from(m.read_physical_u8(buf + 16 + i)) << (i * 8))
        });
        assert_eq!(total, 4032, "qword total sectors");
        let bps = u16::from(m.read_physical_u8(buf + 24))
            | (u16::from(m.read_physical_u8(buf + 25)) << 8);
        assert_eq!(bps, 512, "bytes per sector");
    }

    #[test]
    fn primary_channel_ports_read_open_bus_when_empty() {
        // With no disk mounted, the primary channel reads 0xFF (open bus) so a
        // probe sees no device, and a write is harmlessly dropped.
        let mut machine = test_machine();
        with_bus(&mut machine, |bus| {
            bus.write_io(0x1F2, BusWidth::Byte, 0x55).unwrap();
            let v = bus.read_io(0x1F7, BusWidth::Byte).unwrap();
            assert_eq!(v, 0xFF, "empty channel reads open bus");
        });
    }

    #[test]
    fn primary_channel_identify_runs_through_the_bus() {
        let mut machine = int15_machine(16);
        machine.mount_hdd(vec![0u8; 4032 * 512]);
        with_bus(&mut machine, |bus| {
            // IDENTIFY DEVICE on the command port, then drain word 0 of the block.
            bus.write_io(0x1F7, BusWidth::Byte, 0xEC).unwrap();
            let lo = bus.read_io(0x1F0, BusWidth::Byte).unwrap();
            let hi = bus.read_io(0x1F0, BusWidth::Byte).unwrap();
            let word0 = u16::from(lo as u8) | (u16::from(hi as u8) << 8);
            assert_eq!(word0, 0x0040, "fixed ATA device general config");
        });
    }

    #[test]
    fn int26_write_then_int25_read_round_trips_a_sector() {
        let mut m = int15_machine(16);
        m.mount_floppy(vec![0u8; 1_474_560]).unwrap(); // 1.44 MB, 18 spt, 2 heads
        // Seed a pattern in a guest buffer at DS:BX = 2000:0000 (physical 0x20000).
        for i in 0..512u32 {
            m.write_physical_u8(0x2_0000 + i, (i & 0xff) as u8);
        }
        // INT 26h: AL=0 (A:), CX=1 sector, DX=LBA 40, DS:BX -> 0x20000.
        m.cpu.registers.set_eax(0x0000);
        m.cpu.registers.set_ecx(0x0001);
        m.cpu.registers.set_edx(40);
        m.cpu
            .registers
            .set_segment(SegmentIndex::Ds, SegmentRegister::real(0x2000));
        m.cpu.registers.set_ebx(0x0000);
        m.handle_int26();
        assert_eq!(m.cpu.registers.eax() as u16, 0x0000, "write AX=0");

        // LBA 40 with 18 spt, 2 heads: cyl=1, head=0, sector=5 (40 = 1*36 + 0*18 + 4).
        let on_disk = m.floppy.as_ref().unwrap().read_sector(1, 0, 5).unwrap();
        assert_eq!(on_disk[0], 0x00);
        assert_eq!(on_disk[5], 0x05);
        assert_eq!(on_disk[511], 0xFF);

        // INT 25h reads it back into a fresh buffer at 3000:0000.
        m.cpu.registers.set_eax(0x0000);
        m.cpu.registers.set_ecx(0x0001);
        m.cpu.registers.set_edx(40);
        m.cpu
            .registers
            .set_segment(SegmentIndex::Ds, SegmentRegister::real(0x3000));
        m.cpu.registers.set_ebx(0x0000);
        m.handle_int25();
        assert_eq!(m.cpu.registers.eax() as u16, 0x0000, "read AX=0");
        for i in 0..512u32 {
            assert_eq!(m.read_physical_u8(0x3_0000 + i), (i & 0xff) as u8);
        }
    }

    #[test]
    fn int25_out_of_range_sector_sets_carry() {
        let mut m = int15_machine(16);
        m.mount_floppy(vec![0u8; 737_280]).unwrap(); // 720 KB = 1440 sectors (0..1439)
        // LBA 5000 is well past the media; the read must report an error.
        m.cpu.registers.set_eax(0x0000);
        m.cpu.registers.set_ecx(0x0001);
        m.cpu.registers.set_edx(5000);
        m.cpu
            .registers
            .set_segment(SegmentIndex::Ds, SegmentRegister::real(0x2000));
        m.cpu.registers.set_ebx(0x0000);
        m.handle_int25();
        assert_ne!(
            m.cpu.registers.eax() as u16,
            0x0000,
            "AX carries an error code"
        );
        assert_eq!(
            (m.cpu.registers.eax() as u16 >> 8) as u8,
            0x40,
            "high byte 0x40"
        );
    }

    #[test]
    fn int25_no_media_sets_carry() {
        // No floppy mounted: the absolute read reports drive-not-ready.
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0x0000);
        m.cpu.registers.set_ecx(0x0001);
        m.cpu.registers.set_edx(0);
        m.handle_int25();
        assert_eq!(m.cpu.registers.eax() as u16, 0x4002, "drive not ready");
    }

    #[test]
    fn int25_reads_the_fat32_volume_and_int26_is_write_protected() {
        let dir = std::env::temp_dir().join(format!(
            "izarra_int25_fat32_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("HELLO.TXT"), b"hi").unwrap();
        let vol = build_fat32(&dir, 64 * 1024 * 1024, 0x1234_5678).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        let mut m = int15_machine(16);
        m.mount_fat32(vol);

        // INT 25h: AL=2 (C:), CX=2 sectors from LBA 0 (boot + FSInfo), DS:BX ->
        // 0x20000. Two sectors exercise the i*512 buffer stepping.
        m.cpu.registers.set_eax(0x0002);
        m.cpu.registers.set_ecx(0x0002);
        m.cpu.registers.set_edx(0x0000);
        m.cpu
            .registers
            .set_segment(SegmentIndex::Ds, SegmentRegister::real(0x2000));
        m.cpu.registers.set_ebx(0x0000);
        m.handle_int25();
        assert_eq!(m.cpu.registers.eax() as u16, 0x0000, "INT 25h read AX=0");
        // Sector 0 (the boot sector) landed at buffer+0: the FAT32 type string and
        // the 0x55AA signature prove the volume's sector reached guest RAM.
        let fstype: Vec<u8> = (0..8u32)
            .map(|i| m.read_physical_u8(0x2_0000 + 82 + i))
            .collect();
        assert_eq!(&fstype, b"FAT32   ", "boot sector filesystem type");
        assert_eq!(m.read_physical_u8(0x2_0000 + 510), 0x55, "signature lo");
        assert_eq!(m.read_physical_u8(0x2_0000 + 511), 0xAA, "signature hi");
        // Sector 1 (FSInfo) landed at buffer+512: its lead signature 0x41615252
        // confirms the second sector stepped to the right offset.
        let fsi_lead: Vec<u8> = (0..4u32)
            .map(|i| m.read_physical_u8(0x2_0000 + 512 + i))
            .collect();
        assert_eq!(
            &fsi_lead,
            &0x4161_5252u32.to_le_bytes(),
            "FSInfo lead signature"
        );

        // INT 26h write to the read-only volume is write-protected.
        m.cpu.registers.set_eax(0x0002);
        m.cpu.registers.set_ecx(0x0001);
        m.cpu.registers.set_edx(0x0000);
        m.cpu
            .registers
            .set_segment(SegmentIndex::Ds, SegmentRegister::real(0x2000));
        m.cpu.registers.set_ebx(0x0000);
        m.handle_int26();
        assert_eq!(
            m.cpu.registers.eax() as u16,
            0x0300,
            "INT 26h write to a read-only volume is write-protected"
        );
    }

    #[test]
    fn int25_drive_c_without_a_volume_is_drive_not_ready() {
        // AL=2 with no FAT32 volume mounted falls through to the drive-not-ready
        // path, the same as before this wiring existed.
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0x0002);
        m.cpu.registers.set_ecx(0x0001);
        m.cpu.registers.set_edx(0x0000);
        m.handle_int25();
        assert_eq!(m.cpu.registers.eax() as u16, 0x4002, "drive not ready");
    }

    #[test]
    fn booter_inert_stands_down_dos_vectors_but_keeps_the_bios() {
        let mut m = int15_machine(16);

        // By default the HLE intercepts INT 21h (the DOS kernel).
        m.make_bus().interrupt_acknowledge(0x21, 0).unwrap();
        assert_eq!(
            m.pending_soft_int,
            Some(0x21),
            "INT 21h is intercepted by default"
        );
        m.pending_soft_int = None;

        // Booter-inert mode stands the DOS/IEMM vectors down so the guest's own
        // handlers run through the IVT.
        m.set_booter_inert(true);
        assert!(m.booter_inert());
        m.make_bus().interrupt_acknowledge(0x21, 0).unwrap();
        assert_eq!(
            m.pending_soft_int, None,
            "INT 21h stands down in booter mode"
        );
        m.make_bus().interrupt_acknowledge(0x2f, 0).unwrap();
        assert_eq!(
            m.pending_soft_int, None,
            "INT 2Fh (multiplex/XMS) stands down too"
        );
        m.make_bus().interrupt_acknowledge(0x66, 0).unwrap();
        assert_eq!(m.pending_soft_int, None, "INT 66h (XMS) stands down too");

        // The BIOS hardware services stay intercepted even in booter mode.
        m.make_bus().interrupt_acknowledge(0x10, 0).unwrap();
        assert_eq!(
            m.pending_soft_int,
            Some(0x10),
            "INT 10h (BIOS video) stays intercepted"
        );
        m.pending_soft_int = None;
        m.make_bus().interrupt_acknowledge(0x13, 0).unwrap();
        assert_eq!(
            m.pending_soft_int,
            Some(0x13),
            "INT 13h (BIOS disk) stays intercepted"
        );

        // A vector the HLE never intercepts is recorded in neither mode.
        m.pending_soft_int = None;
        m.make_bus().interrupt_acknowledge(0x80, 0).unwrap();
        assert_eq!(
            m.pending_soft_int, None,
            "an un-intercepted vector is ignored"
        );
    }

    #[test]
    fn int11_returns_equipment_word() {
        // Stub: INT 11h then halt. AX must hold the seeded BDA equipment word.
        // The BIOS service vectors return through the ROM IRET at offset 0xF000
        // that rom_with_code supplies, matching the real izarra BIOS.
        let rom = rom_with_code(&[
            0xCD, 0x11, // int 11h
            0xF4, // hlt
        ]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        let ax = machine.cpu().registers.eax() as u16;
        assert_eq!(ax, BIOS_EQUIPMENT_WORD);
        // Bits 11-9 = 010b: two serial ports advertised (COM1 + COM2).
        assert_eq!((ax >> 9) & 0x07, 2, "two serial ports advertised");
        // Bits 15-14 = 10b: two parallel printer ports advertised (LPT1 + LPT2).
        assert_eq!((ax >> 14) & 0x03, 2, "two parallel ports advertised");
        // Bit 1 (80x87 coprocessor) stays clear: the Izarra 3000 has no FPU.
        assert_eq!(ax & 0x0002, 0, "no coprocessor advertised");
    }

    #[test]
    fn int12_returns_conventional_memory_kib() {
        // Stub: INT 12h then halt. AX must hold the conventional memory size. The
        // 1 KB EBDA reserved at POST drops the reported size from 640 to 639 KB.
        let rom = rom_with_code(&[
            0xCD, 0x12, // int 12h
            0xF4, // hlt
        ]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        let ax = machine.cpu().registers.eax() as u16;
        assert_eq!(ax, BIOS_BASE_MEMORY_KIB - 1);
        assert_eq!(ax, 639);
    }

    #[test]
    fn int1a_ah00_reads_bda_tick() {
        // Seed the BDA tick to 0x00012345, then INT 1Ah AH=00h returns CX:DX.
        let rom = rom_with_code(&[
            0xB4, 0x00, // mov ah, 0
            0xCD, 0x1A, // int 1Ah
            0xF4, // hlt
        ]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
        machine.write_physical_u8(0x46c, 0x45);
        machine.write_physical_u8(0x46d, 0x23);
        machine.write_physical_u8(0x46e, 0x01);
        machine.write_physical_u8(0x46f, 0x00);
        machine.write_physical_u8(0x470, 0x00); // no rollover
        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        let cx = machine.cpu().registers.ecx() as u16;
        let dx = machine.cpu().registers.edx() as u16;
        assert_eq!(cx, 0x0001, "CX = high word of tick");
        assert_eq!(dx, 0x2345, "DX = low word of tick");
        assert_eq!(
            machine.cpu().registers.eax() as u8,
            0x00,
            "AL = rollover count"
        );
    }

    #[test]
    fn int1a_ah02_ah04_return_bcd_clock() {
        // AH=04h clobbers CX/DX, so the AH=02h time result must be stashed to
        // memory before the date call overwrites it. Set DS=0, run AH=02h, store
        // CX/DX into BIOS scratch at 0:0500h, then run AH=04h and HLT. The date
        // result stays live in CX/DX; the time result is read back from scratch.
        let rom = rom_with_code(&[
            0x31, 0xC0, // xor ax, ax
            0x8E, 0xD8, // mov ds, ax (DS = 0)
            0xB4, 0x02, 0xCD, 0x1A, // int 1Ah AH=02h (time)
            0x89, 0x0E, 0x00, 0x05, // mov [0500h], cx
            0x89, 0x16, 0x02, 0x05, // mov [0502h], dx
            0xB4, 0x04, 0xCD, 0x1A, // int 1Ah AH=04h (date)
            0xF4,
        ]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
        machine.seed_rtc(2026, 6, 21, 1, 13, 45, 30); // helper forwards to rtc.seed
        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        // After AH=04h: CH=century 0x20, CL=year 0x26, DH=month 0x06, DL=day 0x21.
        let cx = machine.cpu().registers.ecx() as u16;
        let dx = machine.cpu().registers.edx() as u16;
        assert_eq!(cx, 0x2026);
        assert_eq!(dx, 0x0621);
        // AH=02h stashed time: CH=hour 0x13, CL=minute 0x45, DH=second 0x30, DL=0.
        let time_cx = u16::from(machine.read_physical_u8(0x0500))
            | (u16::from(machine.read_physical_u8(0x0501)) << 8);
        let time_dx = u16::from(machine.read_physical_u8(0x0502))
            | (u16::from(machine.read_physical_u8(0x0503)) << 8);
        assert_eq!(time_cx, 0x1345, "CH=hour BCD, CL=minute BCD");
        assert_eq!(time_dx, 0x3000, "DH=second BCD, DL=0");
    }

    #[test]
    fn int15_ah87_block_move_across_1mb() {
        // Build a GDT in low RAM with source = 0x20000, dest = 0x30000, move 4 words.
        let rom = rom_with_code(&[
            0xB4, 0x87, // mov ah,87h
            0xB9, 0x04, 0x00, // mov cx,4 (words)
            0xBE, 0x00, 0x10, // mov si,1000h (GDT offset)
            0xCD, 0x15, 0xF4,
        ]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
        // ES = 0 so the GDT sits at linear 0x1000. Descriptors at +0x10 (src), +0x18 (dst).
        let gdt = 0x1000u32;
        let write_desc = |m: &mut Machine, at: u32, base: u32| {
            m.write_physical_u8(at, 0xFF); // limit low
            m.write_physical_u8(at + 1, 0xFF);
            m.write_physical_u8(at + 2, base as u8); // base 0..7
            m.write_physical_u8(at + 3, (base >> 8) as u8); // base 8..15
            m.write_physical_u8(at + 4, (base >> 16) as u8); // base 16..23
            m.write_physical_u8(at + 5, 0x93); // access
            m.write_physical_u8(at + 6, 0x00);
            m.write_physical_u8(at + 7, (base >> 24) as u8); // base 24..31
        };
        write_desc(&mut machine, gdt + 0x10, 0x20000);
        write_desc(&mut machine, gdt + 0x18, 0x30000);
        for i in 0..8u32 {
            machine.write_physical_u8(0x20000 + i, 0xA0 + i as u8);
        }
        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        for i in 0..8u32 {
            assert_eq!(machine.read_physical_u8(0x30000 + i), 0xA0 + i as u8);
        }
        assert_eq!(
            (machine.cpu().registers.eax() as u16 >> 8) as u8,
            0x00,
            "AH=0 success"
        );
    }

    #[test]
    fn int15_ah86_wait_advances_guest_clock() {
        let rom = rom_with_code(&[
            0xB4, 0x86, 0xB9, 0x00, 0x00, // CX=0
            0xBA, 0x40, 0x42, // DX=0x4240 -> with CX=0 that is 16960 us
            0xCD, 0x15, 0xF4,
        ]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
        let before = machine.elapsed_clocks();
        let reason = machine.run_until_halt_or_cycles(10_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        // CX:DX = 0x00004240 = 16960 microseconds. stall_for converts that to guest
        // clocks at the active mode's rate, so the elapsed-clock jump must dwarf the
        // handful of setup-instruction clocks. Require at least half the expected
        // stall to leave margin for the rounding in stall_for.
        let wait_secs = 16_960.0 / 1_000_000.0;
        let expected_stall = (wait_secs * machine.active_mode().clock_hz() as f64) as u64;
        let advanced = machine.elapsed_clocks() - before;
        assert!(
            advanced >= expected_stall / 2,
            "AH=86h stall too small: advanced {advanced} clocks, expected ~{expected_stall}"
        );
        let flags = machine.cpu().registers.eflags;
        assert_eq!(flags & 0x0001, 0, "CF clear after WAIT");
    }

    #[test]
    fn mouse_movement_requests_irq12_after_enable() {
        // Bring up the PS/2 mouse the way a driver does (command byte bit 1 set
        // for the mouse interrupt, then 0xF4 enable reporting via the 0xD4 path),
        // then inject a host move and confirm IRQ12 is pending on the PIC and the
        // three-byte packet is readable on port 0x60 with the AUX status bit set.
        let profile = MachineProfile::gsw_386(1, VideoCard::Et4000Ax);
        let mut machine = Machine::new(profile, vec![0u8; BIOS_ROM_SIZE]).unwrap();
        // Drive the controller through the bus the way the CPU would.
        {
            let mut bus = machine.make_bus();
            bus.write_io(0x64, BusWidth::Byte, 0x60).unwrap(); // write command byte
            bus.write_io(0x60, BusWidth::Byte, 0x03).unwrap(); // IRQ1 + IRQ12 enabled
            bus.write_io(0x64, BusWidth::Byte, 0xD4).unwrap(); // next byte to aux
            bus.write_io(0x60, BusWidth::Byte, 0xF4).unwrap(); // enable data reporting
            assert_eq!(bus.read_io(0x60, BusWidth::Byte).unwrap(), 0xFA); // mouse ACK
        }
        // Move right 4, down 2, left button down.
        machine.inject_mouse(4, 2, 0x01);
        assert!(machine.irq12_pending(), "movement requests IRQ12");
        // The packet is on port 0x60 and the status reports an AUX byte.
        assert_eq!(machine.read_io_port_u8(0x64) & 0x20, 0x20, "AUX status bit");
        let b0 = machine.read_io_port_u8(0x60);
        assert_eq!(b0 & 0x08, 0x08, "always-one bit");
        assert_eq!(b0 & 0x01, 0x01, "left button");
        assert_eq!(b0 & 0x10, 0x00, "X positive");
        assert_eq!(b0 & 0x20, 0x20, "Y sign set (screen-down move)");
        assert_eq!(machine.read_io_port_u8(0x60), 4, "dx byte");
        assert_eq!(machine.read_io_port_u8(0x60) as i8 as i32, -2, "dy byte");
    }

    #[test]
    fn bios_aux_enable_then_packet_reads_back_with_no_stray_keyboard_byte() {
        // Drive the exact sequence the BIOS bootbox menu runs (izbios-bootbox.inc
        // bx2_aux_init): read the controller command byte, set the IRQ1+IRQ12
        // enable bits, then enable AUX reporting via the 0xD4 prefix and drain the
        // mouse ACK. The two things this guards that the menu has no automated
        // coverage for: the injected packet reads back on 0x60 with the AUX status
        // bit set, AND the enable handshake never drops a stray byte into the
        // keyboard scancode ring (which the keyboard ISR reads unconditionally).
        let profile = MachineProfile::gsw_386(1, VideoCard::Et4000Ax);
        let mut machine = Machine::new(profile, vec![0u8; BIOS_ROM_SIZE]).unwrap();
        {
            let mut bus = machine.make_bus();
            // Read CCB (0x20) -> 0x60, OR in IRQ1 (bit0) + IRQ12 (bit1), write back.
            bus.write_io(0x64, BusWidth::Byte, 0x20).unwrap();
            let ccb = bus.read_io(0x60, BusWidth::Byte).unwrap() as u8;
            let new_ccb = ccb | 0x01 | 0x02;
            bus.write_io(0x64, BusWidth::Byte, 0x60).unwrap();
            bus.write_io(0x60, BusWidth::Byte, new_ccb as u32).unwrap();
            // Enable AUX data reporting: 0xD4 routes 0xF4 to the mouse.
            bus.write_io(0x64, BusWidth::Byte, 0xD4).unwrap();
            bus.write_io(0x60, BusWidth::Byte, 0xF4).unwrap();
            // Drain the AUX ACK (0xFA): it must arrive flagged as an AUX byte.
            let status = bus.read_io(0x64, BusWidth::Byte).unwrap() as u8;
            assert_eq!(status & 0x01, 0x01, "ACK waiting (OBF)");
            assert_eq!(status & 0x20, 0x20, "ACK is an AUX byte, not a key");
            assert_eq!(
                bus.read_io(0x60, BusWidth::Byte).unwrap(),
                0xFA,
                "mouse ACK"
            );
        }
        // The enable handshake must not have armed IRQ1 or queued a keyboard byte.
        assert!(
            !machine.irq1_pending(),
            "AUX enable must not arm the keyboard interrupt"
        );
        assert_eq!(
            machine.read_io_port_u8(0x64) & 0x01,
            0,
            "no byte left in the output buffer after the ACK drain"
        );

        // Now a host move queues a three-byte packet, flagged AUX, with IRQ12.
        machine.inject_mouse(6, -3, 0x01); // right 6, up 3, left button down
        assert!(machine.irq12_pending(), "movement requests IRQ12");
        assert_eq!(
            machine.read_io_port_u8(0x64) & 0x20,
            0x20,
            "packet byte is flagged AUX"
        );
        let b0 = machine.read_io_port_u8(0x60);
        assert_eq!(b0 & 0x08, 0x08, "sync bit");
        assert_eq!(b0 & 0x01, 0x01, "left button");
        assert_eq!(machine.read_io_port_u8(0x60), 6, "dx byte");
        assert_eq!(
            machine.read_io_port_u8(0x60),
            3,
            "dy byte (screen up -> +3)"
        );
        // The packet drained cleanly: nothing left, and still no keyboard IRQ.
        assert_eq!(
            machine.read_io_port_u8(0x64) & 0x01,
            0,
            "output buffer empty after the packet"
        );
        assert!(
            !machine.irq1_pending(),
            "the AUX packet never touched the keyboard interrupt"
        );
    }

    #[test]
    fn int33_set_then_get_position_round_trips() {
        // Stub: set the cursor to (100, 50) via AX=0004, then read it back via
        // AX=0003. The host injects a left-button-down move first so the get
        // reports the button mask too. After the get, BX=buttons, CX=col, DX=row.
        let rom = rom_with_code(&[
            0xB8, 0x04, 0x00, // mov ax, 0x0004 (set position)
            0xB9, 0x64, 0x00, // mov cx, 100 (column)
            0xBA, 0x32, 0x00, // mov dx, 50 (row)
            0xCD, 0x33, // int 33h
            0xB8, 0x03, 0x00, // mov ax, 0x0003 (get position + buttons)
            0xCD, 0x33, // int 33h
            0xF4, // hlt
        ]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
        // A prior host move sets the left button; position is overwritten by the
        // AX=0004 set, so only the button mask survives into the get.
        machine.inject_mouse(0, 0, 0x01);
        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        let bx = machine.cpu().registers.ebx() as u16;
        let cx = machine.cpu().registers.ecx() as u16;
        let dx = machine.cpu().registers.edx() as u16;
        assert_eq!(cx, 100, "column round-trips through set/get");
        assert_eq!(dx, 50, "row round-trips through set/get");
        assert_eq!(bx, 0x0001, "left button reported in BX");
    }

    #[test]
    fn int33_reset_reports_present_and_two_buttons() {
        // Stub: AX=0000 reset/detect, then halt. AX=FFFFh (installed), BX=2.
        let rom = rom_with_code(&[
            0x31, 0xC0, // xor ax, ax (AX=0000)
            0xCD, 0x33, // int 33h
            0xF4, // hlt
        ]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        let ax = machine.cpu().registers.eax() as u16;
        let bx = machine.cpu().registers.ebx() as u16;
        assert_eq!(ax, 0xFFFF, "driver reports installed");
        assert_eq!(bx, 0x0002, "two-button mouse");
    }

    #[test]
    fn bios_service_vectors_survive_low_memory_wipe() {
        // A booter that zeroes low RAM (including the 0x600 RAM IRET stub) must not
        // strand INT 11h/12h: their IVT targets point at the ROM IRET, so the
        // service still returns. Stub: zero 0x600, then INT 11h, then halt.
        // rom_with_code supplies the ROM IRET at FF00:0000 that survives the wipe.
        let rom = rom_with_code(&[
            0x31, 0xC0, // xor ax, ax
            0x8E, 0xD8, // mov ds, ax
            0xC7, 0x06, 0x00, 0x06, 0x00, 0x00, // mov word [0x600], 0
            0xCD, 0x11, // int 11h
            0xF4, // hlt
        ]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        assert_eq!(machine.cpu().registers.eax() as u16, BIOS_EQUIPMENT_WORD);
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
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

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
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        // The VGA mode-set hands the display back to VGA, but the 4F02 call must
        // still have set the Margo mode (width stays set; only margo_active clears).
        assert_eq!(machine.margo().display().width, 640);
        assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
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
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

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
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);
        assert_eq!(machine.cpu().registers.ebx() as u16, 0x0101);
    }

    #[test]
    fn passive_target_ports_allow_capability_probes_to_fail_cleanly() {
        // 0x226 is the SB DSP reset port: still an unimplemented passive port
        // (0x224/0x225 are now the CT1745 mixer, 0x388 the OPL chip).
        let mut machine = test_machine();
        let value = with_bus(&mut machine, |bus| {
            bus.read_io(0x0226, BusWidth::Byte).unwrap()
        });

        assert_eq!(value, 0xff);
        assert!(
            machine
                .bus_trace()
                .cycles()
                .iter()
                .any(|cycle| cycle.kind == BusAccessKind::IoRead && cycle.address == 0x0226)
        );
    }

    #[test]
    fn mixer_index_port_decodes_instead_of_falling_through_passive() {
        // 0x224 used to read 0xFF as a passive port; it is now the CT1745 mixer
        // index register, whose read returns the latched index (0 at reset).
        let mut machine = test_machine();
        let index_read = with_bus(&mut machine, |bus| {
            bus.read_io(0x0224, BusWidth::Byte).unwrap()
        });
        assert_eq!(index_read, 0x00, "0x224 returns the latched mixer index");
        // Programming register 0x80 (IRQ7) round-trips through 0x225.
        with_bus(&mut machine, |bus| {
            bus.write_io(0x224, BusWidth::Byte, 0x80).unwrap();
            bus.write_io(0x225, BusWidth::Byte, 0x04).unwrap();
        });
        let routed = with_bus(&mut machine, |bus| {
            bus.write_io(0x224, BusWidth::Byte, 0x80).unwrap();
            bus.read_io(0x225, BusWidth::Byte).unwrap()
        });
        assert_eq!(routed, 0x04, "IRQ7 latched in mixer register 0x80");
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
    fn sb_dma_irq5_fires_from_the_cpu_clock_without_host_audio_pull() {
        let mut machine = test_machine();
        // 8-bit ramp at 0x01_0000; arm DMA ch1 + DSP exactly like the playback golden.
        for (i, b) in (0..16u8).map(|i| i * 16).enumerate() {
            machine.write_physical_u8(0x1_0000 + i as u32, b);
        }
        with_bus(&mut machine, |bus| {
            bus.write_io(0x0B, BusWidth::Byte, 0x49).unwrap();
            bus.write_io(0x02, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x02, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x03, BusWidth::Byte, 0x0F).unwrap();
            bus.write_io(0x03, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x83, BusWidth::Byte, 0x01).unwrap();
            bus.write_io(0x0A, BusWidth::Byte, 0x01).unwrap();
            for &b in &[0x41u8, 0x2B, 0x11, 0x48, 0x0F, 0x00, 0x14] {
                bus.write_io(0x22C, BusWidth::Byte, u32::from(b)).unwrap();
            }
        });
        let before = with_bus(&mut machine, |bus| bus.interrupt_pending());
        assert!(!before, "no IRQ pending before time advances");
        // Advance CPU time for well over the 16-sample block (single-cycle -> end IRQ).
        machine.advance_devices_clocks(200_000);
        let after = with_bus(&mut machine, |bus| bus.interrupt_pending());
        assert!(
            after,
            "IRQ5 must be raised by the per-clock sample advance, not the host render path"
        );
    }

    #[test]
    fn sb_mixer_selects_irq7_and_routes_the_dma_irq() {
        let mut machine = test_machine();
        // 8-bit ramp at 0x01_0000 (DMA ch1, the mixer's default 8-bit channel).
        for (i, b) in (0..16u8).map(|i| i * 16).enumerate() {
            machine.write_physical_u8(0x1_0000 + i as u32, b);
        }
        with_bus(&mut machine, |bus| {
            // Route the DSP IRQ on IRQ7 (mixer register 0x80 = 0x04).
            bus.write_io(0x224, BusWidth::Byte, 0x80).unwrap();
            bus.write_io(0x225, BusWidth::Byte, 0x04).unwrap();
            // PIC base 0x08 so IRQ7 -> vector 0x0F; mask everything except IR7.
            bus.write_io(0x20, BusWidth::Byte, 0x11).unwrap();
            bus.write_io(0x21, BusWidth::Byte, 0x08).unwrap();
            bus.write_io(0x21, BusWidth::Byte, 0x04).unwrap();
            bus.write_io(0x21, BusWidth::Byte, 0x01).unwrap();
            bus.write_io(0x21, BusWidth::Byte, 0x7F).unwrap();
            // DMA ch1 + DSP 8-bit single-cycle, exactly like the IRQ5 golden.
            bus.write_io(0x0B, BusWidth::Byte, 0x49).unwrap();
            bus.write_io(0x02, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x02, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x03, BusWidth::Byte, 0x0F).unwrap();
            bus.write_io(0x03, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x83, BusWidth::Byte, 0x01).unwrap();
            bus.write_io(0x0A, BusWidth::Byte, 0x01).unwrap();
            for &b in &[0x41u8, 0x2B, 0x11, 0x48, 0x0F, 0x00, 0x14] {
                bus.write_io(0x22C, BusWidth::Byte, u32::from(b)).unwrap();
            }
        });
        machine.advance_devices_clocks(200_000);
        let vector = with_bus(&mut machine, |bus| bus.acknowledge_interrupt());
        assert_eq!(vector, Some(0x0F), "the DMA IRQ must land on line 7, not 5");
    }

    #[test]
    fn sb_mixer_selects_dma_channel_3() {
        let mut machine = test_machine();
        let bytes: Vec<u8> = (0..16).map(|i| (i * 16) as u8).collect();
        for (i, &b) in bytes.iter().enumerate() {
            machine.write_physical_u8(0x1_0000 + i as u32, b);
        }
        with_bus(&mut machine, |bus| {
            // Route the 8-bit DMA through DMA3 (mixer register 0x81 = 0x08).
            bus.write_io(0x224, BusWidth::Byte, 0x81).unwrap();
            bus.write_io(0x225, BusWidth::Byte, 0x08).unwrap();
            // DMA ch3: page 0x82, byte addr 0, count 15 (16 bytes), single read.
            bus.write_io(0x0B, BusWidth::Byte, 0x4B).unwrap();
            bus.write_io(0x06, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x06, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x07, BusWidth::Byte, 0x0F).unwrap();
            bus.write_io(0x07, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x82, BusWidth::Byte, 0x01).unwrap();
            bus.write_io(0x0A, BusWidth::Byte, 0x03).unwrap();
            // DSP: 11025 Hz, block 16, single-cycle 8-bit DMA output.
            for &b in &[0x41u8, 0x2B, 0x11, 0x48, 0x0F, 0x00, 0x14] {
                bus.write_io(0x22C, BusWidth::Byte, u32::from(b)).unwrap();
            }
        });
        let out = {
            machine.advance_devices_clocks(200_000);
            machine.render_dsp_audio(16)
        };
        assert_eq!(out.len(), 16, "buffer drained via DMA channel 3");
        assert!(out.iter().any(|&(l, _)| l < 0), "expected negative samples");
        assert!(
            out.iter().all(|&(l, r)| l == r),
            "8-bit mono duplicated L/R"
        );
        // Single mode masks channel 3 at terminal count, proving the producer
        // drew from channel 3 (channel 1 stayed masked and untouched).
        assert_eq!(machine.dma_read_byte(3), None, "ch3 reached TC");
    }

    #[test]
    fn sb_mixer_reset_restores_irq5_default() {
        let mut machine = test_machine();
        with_bus(&mut machine, |bus| {
            // Route the IRQ on IRQ7, then reset the mixer (any value to 0x00).
            bus.write_io(0x224, BusWidth::Byte, 0x80).unwrap();
            bus.write_io(0x225, BusWidth::Byte, 0x04).unwrap();
            bus.write_io(0x224, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x225, BusWidth::Byte, 0x01).unwrap();
            // A guest reset restores the hardware IRQ5 default, not the host config.
            bus.write_io(0x224, BusWidth::Byte, 0x80).unwrap();
            let byte = bus.read_io(0x225, BusWidth::Byte).unwrap();
            assert_eq!(byte, 0x02);
        });
    }

    #[test]
    fn machine_applies_host_sound_blaster_config_at_boot() {
        let mut profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
        profile.sound_blaster = SoundBlasterConfig {
            enabled: true,
            irq: SbIrq::I7,
            dma: SbDma8::D3,
            high_dma: SbDma16::D6,
        };
        let mut machine = Machine::new(profile, I386DX25_TEST_ROM).unwrap();
        // The mixer boots on the configured routing, not the hardware IRQ5/DMA1/DMA5.
        assert_eq!(machine.sb_selected_irq(), 7);
        let (irq_byte, dma_byte) = with_bus(&mut machine, |bus| {
            bus.write_io(0x224, BusWidth::Byte, 0x80).unwrap();
            let irq = u8::try_from(bus.read_io(0x225, BusWidth::Byte).unwrap()).unwrap();
            bus.write_io(0x224, BusWidth::Byte, 0x81).unwrap();
            let dma = u8::try_from(bus.read_io(0x225, BusWidth::Byte).unwrap()).unwrap();
            (irq, dma)
        });
        assert_eq!(irq_byte, 0x04, "register 0x80 boots on IRQ7");
        assert_eq!(dma_byte, 0x48, "register 0x81 boots on DMA3 | DMA6");
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
        let out = {
            // Playback is now clock-driven: advance CPU time for well over the
            // 16-sample block (single-cycle -> end IRQ), then drain the ring.
            machine.advance_devices_clocks(200_000);
            machine.render_dsp_audio(16)
        };
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
    fn sb_pro_8bit_stereo_deinterleaves_two_bytes_per_frame_at_the_halved_rate() {
        let mut machine = test_machine();
        // A 16-byte unsigned interleaved L/R pattern in conventional memory:
        // bytes 0,16,32,... so each frame's left byte differs from its right.
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
            // Mixer register 0x0E bit1: SB Pro stereo.
            bus.write_io(0x224, BusWidth::Byte, 0x0E).unwrap();
            bus.write_io(0x225, BusWidth::Byte, 0x02).unwrap();
            // Voice volume to unity so the decoded L/R samples survive the mixer.
            bus.write_io(0x224, BusWidth::Byte, 0x32).unwrap();
            bus.write_io(0x225, BusWidth::Byte, 0x1F).unwrap();
            bus.write_io(0x224, BusWidth::Byte, 0x33).unwrap();
            bus.write_io(0x225, BusWidth::Byte, 0x1F).unwrap();
            // DSP: set the interleaved byte rate via the 0x40 TIME CONSTANT
            // (tc 0xD3 -> 1_000_000/45 = 22_222 byte/s; SB Pro stereo halves it
            // to the per-channel frame rate), block 16, single 8-bit DMA output.
            for &b in &[0x40u8, 0xD3, 0x48, 0x0F, 0x00, 0x14] {
                bus.write_io(0x22C, BusWidth::Byte, u32::from(b)).unwrap();
            }
        });
        // Advance well past the 16-byte block (8 stereo frames at 2 bytes/frame).
        machine.advance_devices_clocks(200_000);
        // Drain the raw producer ring: each frame must carry DISTINCT L/R pulled
        // from two interleaved DMA bytes (left = even byte, right = odd byte).
        let raw = machine.render_dsp_audio(8);
        assert_eq!(raw.len(), 8, "8 stereo frames from a 16-byte block");
        // Frame 0: left byte 0 (= -32768), right byte 16 (= -28672); distinct.
        assert_ne!(raw[0].0, raw[0].1, "frame 0 de-interleaves distinct L/R");
        assert!(
            raw.iter().any(|&(l, r)| l != r),
            "stereo de-interleave yields a per-channel L != R through the DMA path"
        );
        // Single mode masks channel 1 at terminal count.
        assert_eq!(machine.dma_read_byte(1), None);
        // And the resampler runs at the HALVED per-channel rate: byte rate 22_222
        // (1_000_000/45) -> 11_111 Hz.
        let out = machine.render_audio(OPL_NATIVE_HZ as usize / 50);
        assert!(!out.is_empty(), "SB Pro stereo produces output");
        assert_eq!(
            machine.dsp_rate_hz, 11_111,
            "DSP resampler configured at the halved per-channel rate"
        );
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
            // Voice volume to unity (0 dB) so the exact -1/+1 samples survive the
            // CT1745 voice attenuation and the test stays about 16-bit decoding.
            bus.write_io(0x224, BusWidth::Byte, 0x32).unwrap();
            bus.write_io(0x225, BusWidth::Byte, 0x1F).unwrap();
            bus.write_io(0x224, BusWidth::Byte, 0x33).unwrap();
            bus.write_io(0x225, BusWidth::Byte, 0x1F).unwrap();
            // DSP: 22050 Hz, 16-bit auto-init output, signed, stereo, count 15.
            for &b in &[0x41u8, 0x56, 0x22, 0xB6, 0x30, 0x0F, 0x00] {
                bus.write_io(0x22C, BusWidth::Byte, u32::from(b)).unwrap();
            }
        });
        let out = {
            // Playback is now clock-driven: advance CPU time for well over the
            // 8-frame stereo buffer (auto-init keeps feeding), then drain the ring.
            machine.advance_devices_clocks(200_000);
            machine.render_dsp_audio(8)
        };
        assert_eq!(out.len(), 8);
        assert_eq!(out[0].0, -1, "left channel is signed -1");
        assert_eq!(out[0].1, 1, "right channel is signed +1");
        assert!(
            out.iter().all(|&(l, r)| l == -1 && r == 1),
            "every stereo frame decodes L=-1, R=+1"
        );
        // Auto-init: channel 5 (the mixer's default 16-bit channel) still feeds.
        assert!(
            machine.dma_read_word(5).is_some(),
            "auto-init keeps feeding"
        );
    }

    // ---- AD1848 / Windows Sound System integration ------------------------

    // The default WSS board: config region at 0x530, codec direct registers at
    // 0x534-0x537 (base+4), IRQ7, byte-wide DMA channel 0.
    const WSS_CODEC: u16 = 0x534; // R0 Index
    const WSS_DATA: u16 = 0x535; // R1 Indexed Data

    /// Write one AD1848 indirect register through the codec's R0 (index) + R1
    /// (data) direct ports on the machine bus.
    fn wss_write_indirect(bus: &mut MachineBus, index: u8, value: u8) {
        bus.write_io(WSS_CODEC, BusWidth::Byte, u32::from(index))
            .unwrap();
        bus.write_io(WSS_DATA, BusWidth::Byte, u32::from(value))
            .unwrap();
    }

    #[test]
    fn wss_16bit_stereo_dma_plays_and_irqs_through_the_machine() {
        let mut machine = test_machine();
        // 8 signed-LE 16-bit stereo frames (32 bytes) at byte base 0x01_0000 over
        // the WSS byte-wide DMA channel 0. Each frame is asymmetric: L = +1
        // (0x0001), R = -2 (0xFFFE), so a real de-interleave yields L != R and the
        // codec's left-before-right ordering is observable.
        let frame: [u8; 4] = [0x01, 0x00, 0xFE, 0xFF];
        for i in 0..8u32 {
            for (j, &b) in frame.iter().enumerate() {
                machine.write_physical_u8(0x1_0000 + i * 4 + j as u32, b);
            }
        }
        with_bus(&mut machine, |bus| {
            // DMA ch0 (the WSS default): byte addr 0x0000, page 0x01 -> 0x01_0000,
            // count 31 (32 bytes), single read. Channel 0 ports: addr 0x00,
            // count 0x01, mode 0x0B, page 0x87, single-mask 0x0A.
            bus.write_io(0x0B, BusWidth::Byte, 0x48).unwrap(); // mode ch0: single, read
            bus.write_io(0x00, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x00, BusWidth::Byte, 0x00).unwrap(); // addr 0x0000
            bus.write_io(0x01, BusWidth::Byte, 0x1F).unwrap();
            bus.write_io(0x01, BusWidth::Byte, 0x00).unwrap(); // count 31 -> 32 bytes
            bus.write_io(0x87, BusWidth::Byte, 0x01).unwrap(); // ch0 page -> 0x01_0000
            bus.write_io(0x0A, BusWidth::Byte, 0x00).unwrap(); // unmask ch0

            // Program the codec for 16-bit signed stereo at 48000 Hz (XTAL1 CFS6).
            // I8 = FMT(0x40) | S/M(0x10) | CFS6(0x0C) -> 0x5C, MCE-gated.
            bus.write_io(WSS_CODEC, BusWidth::Byte, u32::from(0x40u8 | 0x08))
                .unwrap(); // R0: MCE | index 8
            bus.write_io(WSS_DATA, BusWidth::Byte, 0x5C).unwrap();
            bus.write_io(WSS_CODEC, BusWidth::Byte, 0x08).unwrap(); // clear MCE
            // Enable the external INT pin (I10 IEN, bit1) so terminal count forwards.
            wss_write_indirect(bus, 10, 0x02);
            // Base count 7 -> underflow at frame 8 (N+1 cadence).
            wss_write_indirect(bus, 15, 0x07); // I15 lower count
            wss_write_indirect(bus, 14, 0x00); // I14 upper count (loads current)
            // Arm playback: I9 PEN (bit0) + ACAL (bit3).
            wss_write_indirect(bus, 9, 0x09);
            // Unmute both DACs at 0 dB so the decoded samples pass through.
            wss_write_indirect(bus, 6, 0x00);
            wss_write_indirect(bus, 7, 0x00);
        });

        // Drain the codec ring directly to prove de-interleave (render_audio mixes
        // OPL in, which would mask the exact values).
        machine.advance_devices_clocks(200_000);
        let mut frames = Vec::new();
        while let Some(f) = machine.wss.drain_frame() {
            frames.push(f);
        }
        assert!(!frames.is_empty(), "WSS produced rendered frames");
        assert_eq!(
            frames[0],
            (1, -2),
            "16-bit LE de-interleave, left before right"
        );
        assert!(
            frames.iter().any(|&(l, r)| l != r),
            "asymmetric L/R proves a real stereo de-interleave (not a mono dup)"
        );

        // The terminal-count interrupt reached the PIC on the WSS line (IRQ7).
        assert!(
            machine.pic.irr_bit(7),
            "WSS terminal count raised its IRQ on the configured line"
        );

        // render_audio still produces a full mix here (this drained the WSS ring
        // above, so the WSS contribution is proven through render_audio separately
        // in wss_stream_reaches_the_mixed_render_output_through_render_audio; this
        // only checks the OPL/DSP/speaker mix is not truncated by the WSS path).
        let mixed = machine.render_audio(64);
        assert!(
            !mixed.is_empty(),
            "render_audio still mixes the other streams after the WSS ring is drained"
        );
    }

    #[test]
    fn wss_coexists_with_sb16_and_opl_without_cross_talk() {
        // With WSS enabled, the SB16 DSP + OPL must still function and there must
        // be no port/IRQ/DMA cross-talk: WSS uses base 0x530 / IRQ7 / DMA0, the
        // SB16 uses 0x220-0x22F / IRQ5 / DMA1, the OPL uses 0x388/9.
        let mut machine = test_machine();

        // SB16 8-bit mono playback on DMA ch1 (the SB default), exactly like the
        // standalone DSP golden, at byte base 0x02_0000.
        let bytes: Vec<u8> = (0..16).map(|i| (i * 16) as u8).collect();
        for (i, &b) in bytes.iter().enumerate() {
            machine.write_physical_u8(0x2_0000 + i as u32, b);
        }
        // A distinct WSS 8-bit mono buffer on DMA ch0 at byte base 0x01_0000:
        // a constant near-full-positive value so the WSS stream is unmistakable.
        for i in 0..16u32 {
            machine.write_physical_u8(0x1_0000 + i, 0xFF);
        }

        with_bus(&mut machine, |bus| {
            // --- SB16 DMA ch1 + DSP (IRQ5/DMA1, ports 0x220-0x22F) ---
            bus.write_io(0x0B, BusWidth::Byte, 0x49).unwrap(); // mode ch1: single, read
            bus.write_io(0x02, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x02, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x03, BusWidth::Byte, 0x0F).unwrap();
            bus.write_io(0x03, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x83, BusWidth::Byte, 0x02).unwrap(); // ch1 page -> 0x02_0000
            bus.write_io(0x0A, BusWidth::Byte, 0x01).unwrap(); // unmask ch1
            for &b in &[0x41u8, 0x2B, 0x11, 0x48, 0x0F, 0x00, 0x14] {
                bus.write_io(0x22C, BusWidth::Byte, u32::from(b)).unwrap();
            }
            // OPL: key a full sustained tone on channel 0 (modulator + carrier +
            // key-on) so the OPL stream is genuinely audible, not just touched.
            program_tone(bus, 0x388, 0x389);

            // --- WSS DMA ch0 + codec (IRQ7/DMA0, ports 0x530-0x537) ---
            bus.write_io(0x0B, BusWidth::Byte, 0x48).unwrap(); // mode ch0: single, read
            bus.write_io(0x00, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x00, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x01, BusWidth::Byte, 0x0F).unwrap();
            bus.write_io(0x01, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x87, BusWidth::Byte, 0x01).unwrap(); // ch0 page -> 0x01_0000
            bus.write_io(0x0A, BusWidth::Byte, 0x00).unwrap(); // unmask ch0
            // Codec: 8-bit unsigned PCM mono at 48000 Hz (I8 = CFS6 only -> 0x0C).
            bus.write_io(WSS_CODEC, BusWidth::Byte, u32::from(0x40u8 | 0x08))
                .unwrap();
            bus.write_io(WSS_DATA, BusWidth::Byte, 0x0C).unwrap();
            bus.write_io(WSS_CODEC, BusWidth::Byte, 0x08).unwrap();
            wss_write_indirect(bus, 10, 0x02); // IEN
            wss_write_indirect(bus, 15, 0x07); // count low
            wss_write_indirect(bus, 14, 0x00); // count high
            wss_write_indirect(bus, 9, 0x09); // PEN | ACAL
            wss_write_indirect(bus, 6, 0x00);
            wss_write_indirect(bus, 7, 0x00);
        });

        machine.advance_devices_clocks(200_000);

        // Both producers filled their own rings independently.
        let mut wss_frames = Vec::new();
        while let Some(f) = machine.wss.drain_frame() {
            wss_frames.push(f);
        }
        assert!(!wss_frames.is_empty(), "WSS still plays alongside the SB16");
        assert!(
            wss_frames.iter().all(|&(l, r)| l == r && l > 0),
            "WSS 8-bit unsigned 0xFF -> near-full-positive mono dup, undisturbed"
        );
        let dsp_out = machine.render_dsp_audio(16);
        assert_eq!(dsp_out.len(), 16, "SB16 DSP still plays its own buffer");

        // No IRQ cross-talk: WSS fired IRQ7, the SB16 fired its mixer-selected
        // IRQ5, and neither stepped on the other.
        assert_eq!(
            machine.sb_selected_irq(),
            5,
            "SB16 default IRQ unchanged by WSS"
        );
        assert!(machine.pic.irr_bit(7), "WSS raised IRQ7");
        assert!(
            machine.pic.irr_bit(machine.sb_selected_irq()),
            "SB16 raised its own (IRQ5) line"
        );

        // No DMA cross-talk: the WSS drew from ch0, the SB16 from ch1; both single
        // channels reached terminal count on their own.
        assert_eq!(machine.dma_read_byte(0), None, "WSS ch0 reached TC");
        assert_eq!(machine.dma_read_byte(1), None, "SB16 ch1 reached TC");

        // The OPL really produces output (the keyed note is audible), not just a
        // non-empty render_audio that the speaker/WSS streams alone would satisfy.
        // Render the OPL in isolation and assert a non-zero sample magnitude.
        let opl_nonsilent = (0..512)
            .map(|_| machine.opl.render_sample())
            .any(|(l, r)| l != 0 || r != 0);
        assert!(
            opl_nonsilent,
            "OPL still synthesizes its keyed note alongside the WSS and SB16"
        );

        // The full mix is non-empty (OPL + SB16 + WSS all summed).
        let mixed = machine.render_audio(64);
        assert!(!mixed.is_empty());
    }

    /// Program DMA channel 0 (the WSS default) for a single-cycle 8-bit read of
    /// `count + 1` bytes at physical `0x01_0000`, then arm the AD1848 codec for
    /// 8-bit unsigned mono at 48000 Hz with IEN set and `count` base count.
    fn wss_arm_8bit_mono(bus: &mut MachineBus, count: u8) {
        // DMA ch0: mode single+read, addr 0x0000, count, page 0x01, unmask.
        bus.write_io(0x0B, BusWidth::Byte, 0x48).unwrap();
        bus.write_io(0x00, BusWidth::Byte, 0x00).unwrap();
        bus.write_io(0x00, BusWidth::Byte, 0x00).unwrap();
        bus.write_io(0x01, BusWidth::Byte, u32::from(count))
            .unwrap();
        bus.write_io(0x01, BusWidth::Byte, 0x00).unwrap();
        bus.write_io(0x87, BusWidth::Byte, 0x01).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x00).unwrap();
        // Codec: 8-bit unsigned PCM mono at 48000 Hz (I8 = CFS6 -> 0x0C), MCE-gated.
        bus.write_io(WSS_CODEC, BusWidth::Byte, u32::from(0x40u8 | 0x08))
            .unwrap();
        bus.write_io(WSS_DATA, BusWidth::Byte, 0x0C).unwrap();
        bus.write_io(WSS_CODEC, BusWidth::Byte, 0x08).unwrap(); // clear MCE
        wss_write_indirect(bus, 10, 0x02); // I10 IEN
        wss_write_indirect(bus, 15, count); // I15 lower count
        wss_write_indirect(bus, 14, 0x00); // I14 upper count (loads current)
        wss_write_indirect(bus, 9, 0x09); // I9 PEN | ACAL
        wss_write_indirect(bus, 6, 0x00);
        wss_write_indirect(bus, 7, 0x00);
    }

    #[test]
    fn wss_irq7_wakes_a_halted_cpu_via_fast_forward() {
        // Mirror sb_dma_irq5_wakes_a_halted_cpu_via_fast_forward for the WSS wake
        // branch in next_device_wake: a guest arms WSS playback with IEN set and
        // IRQ7 unmasked, then sti;hlt. The run loop must fast-forward across the
        // codec's terminal-count window and deliver IRQ7 -- proving the wss_wake
        // estimator drives the machine, not just the wss.rs unit test.
        let mut machine = Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            // mov ax,0; mov ds,ax; sti; hlt; cli; hlt
            rom_with_code(&[0xb8, 0x00, 0x00, 0x8e, 0xd8, 0xfb, 0xf4, 0xfa, 0xf4]),
        )
        .unwrap();
        // 64-byte buffer at 0x01_0000 (DMA ch0 page 0x01) so the codec never
        // underruns before terminal count.
        for i in 0..64u32 {
            machine.write_physical_u8(0x1_0000 + i, 0x80);
        }
        // IRQ7 handler at 0x0700: inc word [0x0610]; mov al,0x20; out 0x20,al; iret.
        let handler: [u8; 9] = [0xff, 0x06, 0x10, 0x06, 0xb0, 0x20, 0xe6, 0x20, 0xcf];
        for (i, &b) in handler.iter().enumerate() {
            machine.write_physical_u8(0x0700 + i as u32, b);
        }
        // IVT[0x0F] (IRQ7 with PIC base 0x08) -> 0x0000:0x0700; clear the counter.
        machine.write_physical_u8(0x3C, 0x00);
        machine.write_physical_u8(0x3D, 0x07);
        machine.write_physical_u8(0x3E, 0x00);
        machine.write_physical_u8(0x3F, 0x00);
        machine.write_physical_u8(0x0610, 0x00);
        machine.write_physical_u8(0x0611, 0x00);
        with_bus(&mut machine, |bus| {
            // PIC base 0x08 so IRQ7 -> vector 0x0F; all IRQs unmasked.
            bus.write_io(0x20, BusWidth::Byte, 0x11).unwrap();
            bus.write_io(0x21, BusWidth::Byte, 0x08).unwrap();
            bus.write_io(0x21, BusWidth::Byte, 0x04).unwrap();
            bus.write_io(0x21, BusWidth::Byte, 0x01).unwrap();
            bus.write_io(0x21, BusWidth::Byte, 0x00).unwrap(); // unmask all
            // Base count 31 -> terminal count after 32 frames.
            wss_arm_8bit_mono(bus, 31);
        });
        let reason = machine.run_until_halt_or_cycles(5_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        let ticks = u16::from(machine.read_physical_u8(0x0610))
            | (u16::from(machine.read_physical_u8(0x0611)) << 8);
        assert!(ticks >= 1, "the WSS IRQ7 handler should have run");
        // The fast-forward crossed a real sample window (32 frames at 48 kHz ~=
        // 14.6k CPU clocks at 22 MHz), not a no-op halt.
        assert!(
            machine.elapsed_clocks() > 10_000,
            "the fast-forward should advance emulated time across the WSS window"
        );
    }

    #[test]
    fn wss_honors_a_configured_slave_irq11_end_to_end() {
        // The default integration tests only exercise IRQ7 (master). Prove a machine
        // configured with a slave line (IRQ11) actually raises THAT line: wss_irq is
        // taken from profile.wss.irq.line() and fed to pic.request(wss_irq), so a
        // transposed or hardcoded line would route the terminal-count IRQ to the
        // wrong pin. Arm the codec and advance device time across its window; the
        // configured line must latch in the PIC's IRR and IRQ7 must stay clear.
        let mut profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
        profile.wss = WssConfig {
            irq: izarravm_core::WssIrq::I11,
            ..WssConfig::default()
        };
        let mut machine = Machine::new(profile, I386DX25_TEST_ROM).unwrap();
        for i in 0..64u32 {
            machine.write_physical_u8(0x1_0000 + i, 0x80);
        }
        with_bus(&mut machine, |bus| {
            wss_arm_8bit_mono(bus, 31); // base count 31 -> TC after 32 frames
        });
        machine.advance_devices_clocks(200_000);
        assert!(
            machine.pic.irr_bit(11),
            "the codec raised its configured IRQ11 (slave line)"
        );
        assert!(
            !machine.pic.irr_bit(7),
            "the default IRQ7 was NOT raised when IRQ11 is configured"
        );
    }

    #[test]
    fn wss_masked_irq7_does_not_wake_the_cpu() {
        // The IRQ-masked path: with IRQ7 masked, next_device_wake's wss branch must
        // be None (it gates on pic.deliverable), so sti;hlt is a genuine halt the
        // codec cannot wake. We mask EVERY line (IMR = 0xFF) so the WSS is the only
        // armed device and no other source can confound the wake -- the run loop
        // therefore halts at the first hlt and the handler never runs, even with
        // interrupts enabled. (The sticky Status INT bit is proven separately in
        // wss_masked_ien_clear_sets_sticky_status_without_a_pic_edge.)
        let mut machine = Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            // mov ax,0; mov ds,ax; sti; hlt; cli; hlt
            rom_with_code(&[0xb8, 0x00, 0x00, 0x8e, 0xd8, 0xfb, 0xf4, 0xfa, 0xf4]),
        )
        .unwrap();
        for i in 0..64u32 {
            machine.write_physical_u8(0x1_0000 + i, 0x80);
        }
        // IRQ7 handler that bumps a counter, so we can prove it never runs.
        let handler: [u8; 9] = [0xff, 0x06, 0x10, 0x06, 0xb0, 0x20, 0xe6, 0x20, 0xcf];
        for (i, &b) in handler.iter().enumerate() {
            machine.write_physical_u8(0x0700 + i as u32, b);
        }
        machine.write_physical_u8(0x3C, 0x00);
        machine.write_physical_u8(0x3D, 0x07);
        machine.write_physical_u8(0x0610, 0x00);
        machine.write_physical_u8(0x0611, 0x00);
        with_bus(&mut machine, |bus| {
            // PIC base 0x08, then mask ALL lines (IMR = 0xFF) so only the codec is
            // armed and nothing can wake the CPU.
            bus.write_io(0x20, BusWidth::Byte, 0x11).unwrap();
            bus.write_io(0x21, BusWidth::Byte, 0x08).unwrap();
            bus.write_io(0x21, BusWidth::Byte, 0x04).unwrap();
            bus.write_io(0x21, BusWidth::Byte, 0x01).unwrap();
            bus.write_io(0x21, BusWidth::Byte, 0xFF).unwrap(); // mask every line
            wss_arm_8bit_mono(bus, 31);
        });
        // Run long enough that, were the WSS line a wake source, the codec would
        // fast-forward to terminal count and the CPU would advance well past the
        // window. A genuine halt makes no progress at the first hlt.
        let reason = machine.run_until_halt_or_cycles(2_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        let ticks = u16::from(machine.read_physical_u8(0x0610))
            | (u16::from(machine.read_physical_u8(0x0611)) << 8);
        assert_eq!(ticks, 0, "masked IRQ7 must not deliver the WSS interrupt");
        assert!(
            !machine.pic.irq_unmasked(7),
            "IRQ7 stayed masked for the duration"
        );
        // The codec did NOT wake the CPU: a masked WSS line is not a wake source,
        // so the run loop genuinely halted instead of fast-forwarding across the
        // ~32-frame codec window (which would have advanced emulated time by 10k+
        // clocks, as the unmasked twin test asserts).
        assert!(
            machine.elapsed_clocks() < 5_000,
            "a genuine halt does not fast-forward across the masked codec window"
        );
    }

    #[test]
    fn wss_masked_ien_clear_sets_sticky_status_without_a_pic_edge() {
        // Underflow sets the codec's *internal* sticky Status INT bit regardless of
        // IEN (datasheet: the internal INT bit becomes one on counter underflow even
        // if IEN is zero), but the external INT *pin* -- and hence the PIC forward in
        // advance_devices -- is gated by IEN. Arm with IEN CLEAR and drive the codec
        // to terminal count directly (advance_devices, no CPU); the sticky bit must
        // be set while no edge ever reaches the PIC, proving the two are distinct.
        const R2_INT: u8 = 0x01;
        let mut machine = test_machine();
        for i in 0..64u32 {
            machine.write_physical_u8(0x1_0000 + i, 0x80);
        }
        with_bus(&mut machine, |bus| {
            // DMA ch0 for 32 bytes at 0x01_0000, 8-bit unsigned mono at 48 kHz, but
            // with IEN CLEAR (I10 = 0) so the underflow forwards no pin edge.
            bus.write_io(0x0B, BusWidth::Byte, 0x48).unwrap();
            bus.write_io(0x00, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x00, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x01, BusWidth::Byte, 0x1F).unwrap();
            bus.write_io(0x01, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x87, BusWidth::Byte, 0x01).unwrap();
            bus.write_io(0x0A, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(WSS_CODEC, BusWidth::Byte, u32::from(0x40u8 | 0x08))
                .unwrap();
            bus.write_io(WSS_DATA, BusWidth::Byte, 0x0C).unwrap();
            bus.write_io(WSS_CODEC, BusWidth::Byte, 0x08).unwrap();
            wss_write_indirect(bus, 10, 0x00); // IEN CLEAR
            wss_write_indirect(bus, 15, 0x1F); // base count 31 -> TC after 32 frames
            wss_write_indirect(bus, 14, 0x00);
            wss_write_indirect(bus, 9, 0x09); // PEN | ACAL
            wss_write_indirect(bus, 6, 0x00);
            wss_write_indirect(bus, 7, 0x00);
        });
        // Advance device time across the full ~32-frame window at 48 kHz (~14.6k CPU
        // clocks at 22 MHz; use a generous budget so terminal count is reached).
        machine.advance_devices_clocks(200_000);
        assert_ne!(
            machine.wss.status() & R2_INT,
            0,
            "underflow sets the internal sticky Status INT bit even with IEN clear"
        );
        assert!(
            !machine.pic.irr_bit(7),
            "IEN clear forwards no pin edge, so the PIC line stays clear"
        );
    }

    #[test]
    fn wss_disabled_leaves_its_ports_undecoded() {
        // With the codec disabled, its config/codec ports must NOT decode at all:
        // 0x530-0x537 is not in known_passive_ports(), so the bus must return
        // Err(UnsupportedPort) for both reads and writes -- not a swallowed error
        // and not a stale latched value. Contrast with the enabled machine, which
        // answers the I12 revision read with 0x0A, so the test proves the gate
        // toggles real decode rather than relying on an error either way.
        let mut profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
        profile.wss = WssConfig {
            enabled: false,
            ..WssConfig::default()
        };
        let mut machine = Machine::new(profile, I386DX25_TEST_ROM).unwrap();
        with_bus(&mut machine, |bus| {
            // A write to the index port must not decode.
            assert!(
                matches!(
                    bus.write_io(WSS_CODEC, BusWidth::Byte, 0x0C),
                    Err(BusError::UnsupportedPort { port }) if port == WSS_CODEC
                ),
                "disabled WSS index write does not decode"
            );
            // A read of the data port (where an enabled codec would surface the
            // I12 revision) must not decode either.
            assert!(
                matches!(
                    bus.read_io(WSS_DATA, BusWidth::Byte),
                    Err(BusError::UnsupportedPort { port }) if port == WSS_DATA
                ),
                "disabled WSS data read does not decode"
            );
            // The window edges (base+7) are likewise undecoded.
            assert!(
                matches!(
                    bus.read_io(0x537, BusWidth::Byte),
                    Err(BusError::UnsupportedPort { port }) if port == 0x537
                ),
                "disabled WSS upper window edge does not decode"
            );
        });

        // The same index-12 read on an ENABLED machine DOES decode to 0x0A, so the
        // disabled assertions above are a genuine contrast, not a vacuous pass.
        let mut enabled = test_machine();
        with_bus(&mut enabled, |bus| {
            bus.write_io(WSS_CODEC, BusWidth::Byte, 0x0C).unwrap(); // select I12
            assert_eq!(
                bus.read_io(WSS_DATA, BusWidth::Byte).unwrap(),
                0x0A,
                "enabled WSS answers the I12 revision read"
            );
        });
    }

    #[test]
    fn wss_disabled_advance_and_render_run_cleanly_and_stay_silent() {
        // The disabled-codec branches in the producer loop (`if self.wss_enabled`)
        // and in render_audio (the `} else { Vec::new() }` WSS arm) are never reached
        // by the port-decode disabled test, which exits before any audio work. Run a
        // disabled machine through advance_devices AND render_audio to prove those
        // branches execute cleanly (no panic) and the WSS contributes silence. We
        // arm DMA/codec ports first to confirm a disabled codec ignores them entirely
        // -- but the ports do not decode, so the writes that would land on the codec
        // are skipped; only the DMA programming (separate decoder) is set up.
        let mut profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
        profile.wss = WssConfig {
            enabled: false,
            ..WssConfig::default()
        };
        let mut machine = Machine::new(profile, I386DX25_TEST_ROM).unwrap();
        for i in 0..64u32 {
            machine.write_physical_u8(0x1_0000 + i, 0xFF);
        }
        with_bus(&mut machine, |bus| {
            // Program DMA ch0 (a separate decoder, still live) so the producer loop,
            // had it run the codec, would have data to read. The codec ports do not
            // decode while disabled, so we do not touch them here.
            bus.write_io(0x0B, BusWidth::Byte, 0x48).unwrap();
            bus.write_io(0x00, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x00, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x01, BusWidth::Byte, 0x3F).unwrap();
            bus.write_io(0x01, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x87, BusWidth::Byte, 0x01).unwrap();
            bus.write_io(0x0A, BusWidth::Byte, 0x00).unwrap();
        });
        // The disabled producer branch must be a no-op: this runs cleanly and queues
        // no codec frames.
        machine.advance_devices_clocks(200_000);
        assert!(
            machine.wss.drain_frame().is_none(),
            "a disabled codec renders no frames"
        );
        // The disabled render_audio arm (Vec::new()) must contribute nothing; with
        // OPL/DSP idle and the speaker silent the whole mix is silence.
        let mixed = machine.render_audio(64);
        assert!(
            mixed.iter().all(|&(l, r)| l == 0 && r == 0),
            "a disabled WSS adds nothing; idle OPL/DSP/speaker leave silence"
        );
    }

    /// Program DMA channel 0 and the codec for 16-bit signed stereo at 48 kHz with
    /// IEN set, drawing `frames` frames (4 bytes each) at physical 0x01_0000.
    fn wss_arm_16bit_stereo(bus: &mut MachineBus, frames: u8) {
        let byte_count = u16::from(frames) * 4 - 1; // count is bytes-1
        bus.write_io(0x0B, BusWidth::Byte, 0x48).unwrap(); // mode ch0: single, read
        bus.write_io(0x00, BusWidth::Byte, 0x00).unwrap();
        bus.write_io(0x00, BusWidth::Byte, 0x00).unwrap();
        bus.write_io(0x01, BusWidth::Byte, u32::from(byte_count & 0xFF))
            .unwrap();
        bus.write_io(0x01, BusWidth::Byte, u32::from(byte_count >> 8))
            .unwrap();
        bus.write_io(0x87, BusWidth::Byte, 0x01).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x00).unwrap();
        // I8 = FMT(0x40) | S/M(0x10) | CFS6(0x0C) -> 0x5C, MCE-gated.
        bus.write_io(WSS_CODEC, BusWidth::Byte, u32::from(0x40u8 | 0x08))
            .unwrap();
        bus.write_io(WSS_DATA, BusWidth::Byte, 0x5C).unwrap();
        bus.write_io(WSS_CODEC, BusWidth::Byte, 0x08).unwrap(); // clear MCE
        wss_write_indirect(bus, 10, 0x02); // IEN
        let count = u16::from(frames) - 1;
        wss_write_indirect(bus, 15, (count & 0xFF) as u8);
        wss_write_indirect(bus, 14, (count >> 8) as u8);
        wss_write_indirect(bus, 9, 0x09); // PEN | ACAL
        wss_write_indirect(bus, 6, 0x00); // left DAC 0 dB
        wss_write_indirect(bus, 7, 0x00); // right DAC 0 dB
    }

    /// Load `frames` asymmetric 16-bit LE stereo frames at 0x01_0000: L = +0x4000,
    /// R = -0x4000, so the de-interleaved, mixed output carries L > 0 and R < 0.
    fn load_asymmetric_stereo(machine: &mut Machine, frames: u32) {
        // L = 0x4000 (+16384) -> bytes 0x00,0x40; R = 0xC000 (-16384) -> 0x00,0xC0.
        let frame: [u8; 4] = [0x00, 0x40, 0x00, 0xC0];
        for i in 0..frames {
            for (j, &b) in frame.iter().enumerate() {
                machine.write_physical_u8(0x1_0000 + i * 4 + j as u32, b);
            }
        }
    }

    #[test]
    fn wss_stream_reaches_the_mixed_render_output_through_render_audio() {
        // Finding: the de-interleave smoke test pre-drains the ring before calling
        // render_audio, so the resampler + L/R summation path is never proven to
        // carry WSS audio. Here we arm an asymmetric stereo buffer, advance devices,
        // and call render_audio WITHOUT draining -- with OPL/DSP idle and the speaker
        // silent, the only possible signal is the WSS stream, so the mixed output
        // must show the codec's L>0 / R<0 sign pattern. Disabling WSS for the same
        // buffer must then yield silence, proving the contribution came from the
        // WSS mix path and not from some other stream.
        let mut machine = test_machine();
        load_asymmetric_stereo(&mut machine, 64);
        with_bus(&mut machine, |bus| wss_arm_16bit_stereo(bus, 64));
        machine.advance_devices_clocks(200_000);
        let mixed = machine.render_audio(64);
        assert!(
            mixed.iter().any(|&(l, r)| l > 0 && r < 0),
            "the WSS stream reaches the mixed L/R output with its asymmetric sign \
             pattern (left positive, right negative)"
        );

        // The identical buffer with WSS disabled produces silence: nothing else is
        // sounding, so the signal above was the WSS mix path, not a stray stream.
        let mut silent_profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
        silent_profile.wss = WssConfig {
            enabled: false,
            ..WssConfig::default()
        };
        let mut silent = Machine::new(silent_profile, I386DX25_TEST_ROM).unwrap();
        load_asymmetric_stereo(&mut silent, 64);
        silent.advance_devices_clocks(200_000);
        let quiet = silent.render_audio(64);
        assert!(
            quiet.iter().all(|&(l, r)| l == 0 && r == 0),
            "with WSS disabled the same buffer mixes to silence"
        );
    }

    #[test]
    fn wss_is_summed_raw_bypassing_the_ct1745_master_gain() {
        // Design contract: the WSS stream is summed into render_audio WITHOUT the
        // CT1745 master/voice/outgain scaling that OPL and the SB16 DSP take. Prove
        // it by HARD-MUTING the CT1745 master (level 0 = exactly 0.0 gain): an OPL
        // tone is silenced, but the WSS stream must still reach the output unchanged.
        let mut machine = test_machine();
        load_asymmetric_stereo(&mut machine, 64);
        with_bus(&mut machine, |bus| {
            // Hard-mute the CT1745 master (0x30/0x31 = 0) -- this scales OPL+DSP to
            // exactly zero but, per the contract, must NOT touch the WSS stream.
            bus.write_io(0x224, BusWidth::Byte, 0x30).unwrap();
            bus.write_io(0x225, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x224, BusWidth::Byte, 0x31).unwrap();
            bus.write_io(0x225, BusWidth::Byte, 0x00).unwrap();
            // Key a loud OPL tone: with the master muted it must contribute zero.
            program_tone(bus, 0x388, 0x389);
            wss_arm_16bit_stereo(bus, 64);
        });
        machine.advance_devices_clocks(200_000);
        let mixed = machine.render_audio(64);
        assert!(
            mixed.iter().any(|&(l, r)| l > 0 && r < 0),
            "the WSS stream survives a hard-muted CT1745 master (summed raw)"
        );

        // Control: the SAME muted master with the OPL tone but WSS disabled yields
        // silence -- confirming the master mute really does zero the OPL/DSP path,
        // so the non-silence above is the raw WSS sum and not a leaking OPL tone.
        let mut control = test_machine();
        with_bus(&mut control, |bus| {
            bus.write_io(0x224, BusWidth::Byte, 0x30).unwrap();
            bus.write_io(0x225, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x224, BusWidth::Byte, 0x31).unwrap();
            bus.write_io(0x225, BusWidth::Byte, 0x00).unwrap();
            program_tone(bus, 0x388, 0x389);
        });
        control.advance_devices_clocks(200_000);
        let muted = control.render_audio(64);
        assert!(
            muted.iter().all(|&(l, r)| l == 0 && r == 0),
            "a hard-muted master zeroes the OPL/DSP path (proving the mute is live)"
        );
    }

    #[test]
    fn wss_autocal_window_drains_even_under_an_invalid_sample_rate() {
        // The autocal converter clock retires the ~128-sample ACI window regardless
        // of the programmed sample rate. If the producer only advanced the autocal
        // when rate_hz() > 0, a guest that cleared MCE while I8 selects one of the
        // two unsupported XTAL1 rates (rate_hz() == 0) would leave ACI asserted
        // forever. Arm an invalid rate (XTAL1 CFS4 -> 0 Hz), trigger autocal by an
        // MCE clear, and advance device time: ACI must retire on the fallback clock.
        let mut machine = test_machine();
        with_bus(&mut machine, |bus| {
            // Select an UNSUPPORTED rate: I8 = CFS4 (bits3:1 = 4 -> 0x08), CSS=0
            // (XTAL1). rate_hz() decodes this to 0. Set MCE to latch I8, then clear
            // MCE to assert ACI.
            bus.write_io(WSS_CODEC, BusWidth::Byte, u32::from(0x40u8 | 0x08))
                .unwrap(); // MCE | index 8
            bus.write_io(WSS_DATA, BusWidth::Byte, 0x08).unwrap(); // CFS4, XTAL1 -> 0 Hz
            bus.write_io(WSS_CODEC, BusWidth::Byte, 0x08).unwrap(); // clear MCE -> ACI
        });
        assert!(
            machine.wss.autocal_active(),
            "clearing MCE asserts the ACI autocal window"
        );
        assert_eq!(
            machine.wss.rate_hz(),
            0,
            "the selected rate is one of the unsupported (0 Hz) XTAL1 cells"
        );
        // Advance well past the 128-sample window at the 8000 Hz fallback cadence
        // (~16 ms; 200k clocks at 22 MHz is ~9 ms, so use a larger budget).
        machine.advance_devices_clocks(1_000_000);
        assert!(
            !machine.wss.autocal_active(),
            "the ACI window retires on the fallback converter clock despite rate 0"
        );
    }

    #[test]
    fn wss_port_window_edges_and_config_region_decode_through_the_bus() {
        // Pin the wss_offset window math (`port.checked_sub(base).filter(|o| o < 8)`)
        // at its boundaries through the machine bus, plus the config-region readback
        // the decode comment promises does not overlap the SB16/mixer/OPL ranges:
        //   base+1 (0x531) -> IRQ7/DMA0 jumper byte 0x70,
        //   base+7 (0x537) -> decodes (Ok),
        //   base+8 (0x538) and base-1 (0x52F) -> Err(UnsupportedPort).
        let mut machine = test_machine();
        with_bus(&mut machine, |bus| {
            assert_eq!(
                bus.read_io(0x531, BusWidth::Byte).unwrap(),
                0x70,
                "config region reads the IRQ7/DMA0 jumper byte"
            );
            assert!(
                bus.read_io(0x537, BusWidth::Byte).is_ok(),
                "base+7 is the last decoded WSS port"
            );
            assert!(
                matches!(
                    bus.read_io(0x538, BusWidth::Byte),
                    Err(BusError::UnsupportedPort { port }) if port == 0x538
                ),
                "base+8 is past the 8-port window"
            );
            assert!(
                matches!(
                    bus.read_io(0x52F, BusWidth::Byte),
                    Err(BusError::UnsupportedPort { port }) if port == 0x52F
                ),
                "base-1 is below the window"
            );
        });
    }

    #[test]
    fn dma_software_request_drives_a_mem_to_mem_block_copy() {
        // Program the 8237A through the ports for a memory-to-memory copy, then
        // arm it with a software DREQ on channel 0 (a write to the request
        // register) and confirm the destination block in guest memory matches the
        // source. The machine fires the burst on that request-register write.
        let mut machine = test_machine();
        let src = [0xDE, 0xAD, 0xBE, 0xEFu8];
        for (i, &b) in src.iter().enumerate() {
            machine.write_physical_u8(0x0100 + i as u32, b);
        }
        with_bus(&mut machine, |bus| {
            // Channel 0 source address 0x0100, channel 1 dest address 0x0200.
            bus.write_io(0x00, BusWidth::Byte, 0x00).unwrap(); // ch0 addr LSB
            bus.write_io(0x00, BusWidth::Byte, 0x01).unwrap(); // ch0 addr MSB -> 0x0100
            bus.write_io(0x02, BusWidth::Byte, 0x00).unwrap(); // ch1 addr LSB
            bus.write_io(0x02, BusWidth::Byte, 0x02).unwrap(); // ch1 addr MSB -> 0x0200
            bus.write_io(0x03, BusWidth::Byte, 0x03).unwrap(); // ch1 count LSB
            bus.write_io(0x03, BusWidth::Byte, 0x00).unwrap(); // ch1 count MSB -> 3 (4 bytes)
            bus.write_io(0x87, BusWidth::Byte, 0x00).unwrap(); // ch0 page 0
            bus.write_io(0x83, BusWidth::Byte, 0x00).unwrap(); // ch1 page 0
            bus.write_io(0x0A, BusWidth::Byte, 0x00).unwrap(); // unmask ch0 (the requester)
            bus.write_io(0x08, BusWidth::Byte, 0x01).unwrap(); // command: mem-to-mem enable
            // Arm the software DREQ on channel 0: bit2 set, channel bits 0-1 = 0.
            // This write triggers the block copy.
            bus.write_io(0x09, BusWidth::Byte, 0x04).unwrap();
        });
        for (i, &b) in src.iter().enumerate() {
            assert_eq!(
                machine.read_physical_u8(0x0200 + i as u32),
                b,
                "dest byte {i} copied from the source block"
            );
        }
    }

    #[test]
    fn dma_software_request_without_mem_to_mem_enable_does_nothing() {
        // The same request-register write, but with mem-to-mem disabled (command
        // bit0 clear), must not move any memory: the destination stays zero.
        let mut machine = test_machine();
        for i in 0..4 {
            machine.write_physical_u8(0x0100 + i, 0xAB);
        }
        with_bus(&mut machine, |bus| {
            bus.write_io(0x00, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x00, BusWidth::Byte, 0x01).unwrap();
            bus.write_io(0x02, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x02, BusWidth::Byte, 0x02).unwrap();
            bus.write_io(0x03, BusWidth::Byte, 0x03).unwrap();
            bus.write_io(0x03, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x0A, BusWidth::Byte, 0x00).unwrap(); // unmask ch0
            bus.write_io(0x09, BusWidth::Byte, 0x04).unwrap(); // arm, but command bit0 not set
        });
        for i in 0..4 {
            assert_eq!(
                machine.read_physical_u8(0x0200 + i),
                0x00,
                "no copy when mem-to-mem is disabled"
            );
        }
    }

    // Run one closure against a freshly-borrowed bus over the whole machine.
    fn with_bus<R>(machine: &mut Machine, f: impl FnOnce(&mut MachineBus) -> R) -> R {
        let mut bus = MachineBus {
            memory: &mut machine.memory,
            video: &mut machine.video,
            margo: &mut machine.margo,
            rom: &machine.rom,
            serial: &mut machine.serial,
            serial2: &mut machine.serial2,
            lpt: &mut machine.lpt,
            lpt2: &mut machine.lpt2,
            device_ports: &mut machine.device_ports,
            pic: &mut machine.pic,
            pit: &mut machine.pit,
            keyboard: &mut machine.keyboard,
            speaker: &mut machine.speaker,
            rtc: &mut machine.rtc,
            dma: &mut machine.dma,
            fdc: &mut machine.fdc,
            floppy: &mut machine.floppy,
            opl: &mut machine.opl,
            dsp: &mut machine.dsp,
            mixer: &mut machine.mixer,
            wss: &mut machine.wss,
            wss_base: machine.wss_base,
            wss_enabled: machine.wss_enabled,
            ide: &mut machine.ide,
            ata: &mut machine.ata,
            trace: &mut machine.trace,
            pending_soft_int: &mut machine.pending_soft_int,
            active_mode: machine.active_mode,
            pending_mode: &mut machine.pending_mode,
            fast_post: machine.fast_post,
            booter_inert: machine.booter_inert,
            pending_toka_service: &mut machine.pending_toka_service,
            toka_service_status: machine.toka_service_status,
            unittester: &mut machine.unittester,
            wait_states: machine.profile.wait_states,
            io_touched: &mut machine.io_touched,
        };
        f(&mut bus)
    }

    #[test]
    fn rtc_ports_round_trip_through_the_bus() {
        let mut machine = test_machine();
        with_bus(&mut machine, |bus| {
            bus.write_io(0x70, BusWidth::Byte, 0x00).unwrap(); // select seconds
            bus.write_io(0x71, BusWidth::Byte, 42).unwrap();
            bus.write_io(0x70, BusWidth::Byte, 0x00).unwrap();
            let secs = bus.read_io(0x70 + 1, BusWidth::Byte).unwrap();
            assert_eq!(secs, 42);
        });
    }

    #[test]
    fn rtc_advances_seconds_on_the_machine_clock() {
        let mut machine = test_machine();
        machine.seed_rtc(2026, 6, 20, 6, 12, 0, 0);
        // Step roughly three seconds of emulated time, in ~10 ms chunks so the
        // sub-second accumulator carries the way it does during a real run.
        let clock_hz = machine.profile.clock_hz;
        let chunk = clock_hz / 100; // ~10 ms
        for _ in 0..300 {
            machine.advance_devices_clocks(chunk);
        }
        let bytes = machine.cmos_bytes();
        // Seconds register (0x00) should have advanced to about 3.
        assert!(
            (2..=4).contains(&bytes[0x00]),
            "expected the seconds register near 3, got {}",
            bytes[0x00]
        );
    }

    #[test]
    fn cmos_persists_and_reloads_via_bytes() {
        let mut machine = test_machine();
        // Guest writes a layout byte and a boot-order byte, then refreshes the
        // checksum the way the setup page would.
        with_bus(&mut machine, |bus| {
            bus.write_io(0x70, BusWidth::Byte, 0x10).unwrap();
            bus.write_io(0x71, BusWidth::Byte, 3).unwrap(); // FR layout
            bus.write_io(0x70, BusWidth::Byte, 0x11).unwrap();
            bus.write_io(0x71, BusWidth::Byte, 1).unwrap(); // disk-first
        });
        assert!(
            machine.take_cmos_dirty(),
            "an NVRAM write should mark dirty"
        );
        let saved = machine.cmos_bytes();

        // A fresh machine loads the saved image and reads the same bytes back.
        let mut other = test_machine();
        other.load_cmos(&saved);
        assert_eq!(other.cmos_bytes()[0x10], 3);
        assert_eq!(other.cmos_bytes()[0x11], 1);
    }

    #[test]
    fn pc_speaker_renders_a_square_wave() {
        let mut machine = test_machine();
        with_bus(&mut machine, |bus| {
            bus.write_io(0x43, BusWidth::Byte, 0xb6).unwrap(); // ch2, lo/hi, mode 3
            bus.write_io(0x42, BusWidth::Byte, 0x00).unwrap(); // divisor low
            bus.write_io(0x42, BusWidth::Byte, 0x04).unwrap(); // divisor high (0x0400)
            bus.write_io(0x61, BusWidth::Byte, 0x03).unwrap(); // GATE2 + data enable
        });
        let clock_hz = machine.profile.clock_hz;
        let chunk = clock_hz / 100_000; // ~10 us, mimicking per-instruction advance
        for _ in 0..2_000 {
            machine.advance_devices_clocks(chunk); // ~20 ms total
        }
        let pcm = machine.render_audio(OPL_NATIVE_HZ as usize / 50);
        assert!(
            pcm.iter().any(|&(l, _)| l > 0) && pcm.iter().any(|&(l, _)| l < 0),
            "a toggling speaker tone should produce both polarities"
        );
    }

    #[test]
    fn port_61_reports_out_gate_enable_and_refresh() {
        let mut machine = test_machine();
        with_bus(&mut machine, |bus| {
            bus.write_io(0x43, BusWidth::Byte, 0xb6).unwrap();
            bus.write_io(0x42, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x42, BusWidth::Byte, 0x04).unwrap();
            bus.write_io(0x61, BusWidth::Byte, 0x03).unwrap();
        });
        let clock_hz = machine.profile.clock_hz;
        machine.advance_devices_clocks(clock_hz / 100_000); // ~10 us
        let b = with_bus(&mut machine, |bus| {
            bus.read_io(0x61, BusWidth::Byte).unwrap() as u8
        });
        assert_eq!(
            (b >> 5) & 1,
            u8::from(machine.pit.channel_out(2)),
            "bit 5 = ch2 OUT"
        );
        assert_eq!(b & 0x03, 0x03, "bits 0,1 read back GATE2 + data enable");

        // Bit 4 is now PIT channel 1 OUT (the AT DRAM-refresh timer, mode 2),
        // pre-seeded at power-on. This guest never programmed channel 1, yet the
        // bit must still toggle. Mode 2 pulses OUT low for one input clock per
        // refresh period, so over a couple of periods sampled finely bit 4 reads
        // both high (the bulk) and low (the short pulse).
        let mut saw_high = false;
        let mut saw_low = false;
        // Advance one PIT input clock at a time; one CPU step worth of clocks is
        // clock_hz / PIT_INPUT_HZ, so step that to move roughly one PIT tick.
        let per_pit_clock = (clock_hz / u64::from(PIT_INPUT_HZ)).max(1);
        for _ in 0..40 {
            machine.advance_devices_clocks(per_pit_clock);
            let bit4 = with_bus(&mut machine, |bus| {
                (bus.read_io(0x61, BusWidth::Byte).unwrap() as u8 >> 4) & 1
            });
            if bit4 == 1 {
                saw_high = true;
            } else {
                saw_low = true;
            }
        }
        assert!(
            saw_high,
            "refresh bit (4) reads high for the bulk of a period"
        );
        assert!(
            saw_low,
            "refresh bit (4) pulses low once per refresh period"
        );
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
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
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
        // One tick is about 1000 PIT clocks, near 18000 CPU clocks at 22 MHz, so a
        // real fast-forward clears this slack floor while a no-op halt would not.
        assert!(
            machine.elapsed_clocks() > 10_000,
            "the fast-forward should have advanced emulated time across the tick interval"
        );
    }

    #[test]
    fn boot_suite_reports_timer_irq0_pass() {
        let mut machine = Machine::new_boot_image(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
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
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
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
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
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
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
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
    fn boot_suite_reports_sb_8bit_dma_pass() {
        let mut machine = Machine::new_boot_image(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            izarravm_firmware::X86_BOOT_TEST_IMAGE,
        )
        .unwrap();
        let reason = machine.run_until_halt_or_cycles(5_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();
        assert!(
            results.records.iter().any(|record| {
                record.status == izarravm_firmware::SuiteRecordStatus::Pass
                    && record.name == "sound.sb_8bit_dma"
            }),
            "boot suite should report PASS sound.sb_8bit_dma (clock-driven single-cycle DMA + IRQ5)"
        );
    }

    #[test]
    fn boot_suite_reports_sb_16bit_dma_pass() {
        let mut machine = Machine::new_boot_image(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            izarravm_firmware::X86_BOOT_TEST_IMAGE,
        )
        .unwrap();
        let reason = machine.run_until_halt_or_cycles(5_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();
        assert!(
            results.records.iter().any(|record| {
                record.status == izarravm_firmware::SuiteRecordStatus::Pass
                    && record.name == "sound.sb_16bit_dma"
            }),
            "boot suite should report PASS sound.sb_16bit_dma (clock-driven auto-init DMA + IRQ5)"
        );
    }

    #[test]
    fn sb_dma_irq5_wakes_a_halted_cpu_via_fast_forward() {
        // A guest arms 8-bit single-cycle DMA + IRQ5, then `sti;hlt`. The run loop
        // must fast-forward across the DSP sample window (the new IRQ5 wake) and
        // deliver the half-buffer IRQ5, so the handler runs and real emulated time
        // advances -- not a genuine no-wake halt. Setup mirrors the 8-bit probe.
        let mut machine = Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            // mov ax,0; mov ds,ax; sti; hlt; cli; hlt
            rom_with_code(&[0xb8, 0x00, 0x00, 0x8e, 0xd8, 0xfb, 0xf4, 0xfa, 0xf4]),
        )
        .unwrap();
        // 16-byte unsigned ramp at 0x01_0000 (DMA page 0x01, byte addr 0).
        for (i, b) in (0..16u8).map(|i| i * 16).enumerate() {
            machine.write_physical_u8(0x1_0000 + i as u32, b);
        }
        // IRQ5 handler at 0x0700: inc word [0x0610]; mov al,0x20; out 0x20,al; iret.
        let handler: [u8; 9] = [0xff, 0x06, 0x10, 0x06, 0xb0, 0x20, 0xe6, 0x20, 0xcf];
        for (i, &b) in handler.iter().enumerate() {
            machine.write_physical_u8(0x0700 + i as u32, b);
        }
        // IVT[0x0D] -> 0x0000:0x0700; clear the tick counter.
        machine.write_physical_u8(0x34, 0x00);
        machine.write_physical_u8(0x35, 0x07);
        machine.write_physical_u8(0x36, 0x00);
        machine.write_physical_u8(0x37, 0x00);
        machine.write_physical_u8(0x0610, 0x00);
        machine.write_physical_u8(0x0611, 0x00);
        with_bus(&mut machine, |bus| {
            // PIC base 0x08 (ICW1..ICW4) so IRQ5 -> vector 0x0D; all IRQs unmasked.
            bus.write_io(0x20, BusWidth::Byte, 0x11).unwrap();
            bus.write_io(0x21, BusWidth::Byte, 0x08).unwrap();
            bus.write_io(0x21, BusWidth::Byte, 0x04).unwrap();
            bus.write_io(0x21, BusWidth::Byte, 0x01).unwrap();
            // DMA ch1: page 0x01, byte addr 0, count 15 (16 bytes), single read.
            bus.write_io(0x0B, BusWidth::Byte, 0x49).unwrap();
            bus.write_io(0x02, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x02, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x03, BusWidth::Byte, 0x0F).unwrap();
            bus.write_io(0x03, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x83, BusWidth::Byte, 0x01).unwrap();
            bus.write_io(0x0A, BusWidth::Byte, 0x01).unwrap();
            // DSP: 11025 Hz, block 16, single-cycle 8-bit DMA output.
            for &b in &[0x41u8, 0x2B, 0x11, 0x48, 0x0F, 0x00, 0x14] {
                bus.write_io(0x22C, BusWidth::Byte, u32::from(b)).unwrap();
            }
        });
        let reason = machine.run_until_halt_or_cycles(5_000_000).unwrap();
        // The handler ran (after the cli the second hlt is genuine).
        assert_eq!(reason, StopReason::Halted);
        let ticks = u16::from(machine.read_physical_u8(0x0610))
            | (u16::from(machine.read_physical_u8(0x0611)) << 8);
        assert!(ticks >= 1, "the IRQ5 handler should have run");
        // The fast-forward crossed a real sample window (half-buffer at 8 samples
        // ~= 16k CPU clocks at 22 MHz), not a no-op halt.
        assert!(
            machine.elapsed_clocks() > 15_000,
            "the fast-forward should advance emulated time across the DSP sample window"
        );
    }

    #[test]
    fn cli_hlt_is_a_genuine_halt() {
        // With interrupts off, HLT must still halt immediately, not spin.
        let mut machine = Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
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
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
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

    // Throughput probe for the run-loop batching (item 2.3). Not a correctness
    // test; run with: cargo test --release -- --ignored --nocapture batch_throughput
    #[test]
    #[ignore]
    fn batch_throughput() {
        // cli; jmp $ — a tight interrupt-free loop with no port I/O, the case the
        // batch fully amortizes (one bus build + device fan-out per ~thousands of
        // instructions instead of per instruction).
        let mut machine = Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            rom_with_code(&[0xfa, 0xeb, 0xfe]),
        )
        .unwrap();
        let budget = 2_000_000_000u64;
        let t = std::time::Instant::now();
        machine.run_cycles(budget).unwrap();
        let secs = t.elapsed().as_secs_f64();
        println!(
            "batch_throughput: {budget} guest clocks in {secs:.3}s = {:.1} M guest-clocks/s",
            budget as f64 / secs / 1.0e6
        );
    }

    #[test]
    fn audio_sample_cap_is_one_dac_sample_and_never_zero() {
        // The run-loop batch services devices once per cap clocks; the cap must be
        // exactly one 44.1 kHz DAC sample so the PC speaker (samples ch2 OUT once
        // per advance_devices) and the DSP/CD producers never alias, and never 0
        // (which would stall the batch). Checked at the live 266 MHz default and a
        // pathologically slow clock where the floor division would otherwise be 0.
        assert_eq!(
            TimingFactors::for_clock(266_000_000).clocks_per_audio_sample,
            266_000_000 / u64::from(DAC_HZ)
        );
        assert_eq!(
            TimingFactors::for_clock(40_000).clocks_per_audio_sample,
            1,
            "a clock below the DAC rate must floor to 1, not 0"
        );
    }

    #[test]
    fn fdc_read_data_streams_a_sector_into_memory_over_dma_channel_2() {
        // A guest that programs the FDC directly: arm DMA channel 2 for a
        // device->memory write, then issue READ DATA. The sector bytes must land
        // in the guest buffer through the channel-2 datapath, and the controller
        // must raise IRQ6 and present its result phase.
        let mut machine = test_machine();

        // 720 KB image (9 sectors/track, 2 heads). Seed CHS(2,0,3) with a marker.
        // LBA = (cyl*heads + head)*spt + (sector-1) = (2*2+0)*9 + 2 = 38.
        let mut img = vec![0u8; 737_280];
        // LBA for CHS(2,0,3) on a 9-spt, 2-head disk: (2*2 + 0)*9 + (3-1) = 38.
        let lba = 38usize;
        let off = lba * 512;
        for (i, slot) in img[off..off + 512].iter_mut().enumerate() {
            *slot = (0xA0 + (i & 0x0F)) as u8;
        }
        machine.mount_floppy(img).unwrap();

        // Guest DMA target buffer at physical 0x2000 (512 bytes).
        const BUF: u16 = 0x2000;

        with_bus(&mut machine, |bus| {
            // --- Program DMA channel 2: device->memory (write), single, count 512.
            bus.write_io(0x0B, BusWidth::Byte, 0x46).unwrap(); // mode ch2: single, write
            bus.write_io(0x0C, BusWidth::Byte, 0x00).unwrap(); // clear the flip-flop
            bus.write_io(0x04, BusWidth::Byte, u32::from(BUF & 0xFF))
                .unwrap();
            bus.write_io(0x04, BusWidth::Byte, u32::from(BUF >> 8))
                .unwrap();
            bus.write_io(0x81, BusWidth::Byte, 0x00).unwrap(); // page (A16-A23) = 0
            bus.write_io(0x05, BusWidth::Byte, 0xFF).unwrap(); // count low (511)
            bus.write_io(0x05, BusWidth::Byte, 0x01).unwrap(); // count high -> 0x01FF
            bus.write_io(0x0A, BusWidth::Byte, 0x02).unwrap(); // unmask channel 2

            // --- Drive the FDC.
            bus.write_io(0x3F2, BusWidth::Byte, 0x1C).unwrap(); // DOR: motor A, gate, out of reset, drive 0
            bus.write_io(0x3F5, BusWidth::Byte, 0x08).unwrap(); // SENSE INT (clear power-up irq)
            while bus.read_io(0x3F4, BusWidth::Byte).unwrap() & 0x40 != 0 {
                bus.read_io(0x3F5, BusWidth::Byte).unwrap();
            }
            // READ DATA: HDS+DS=0, C=2, H=0, R=3, N=2(512), EOT=3, GPL=0x1B, DTL=0xFF.
            for &b in &[0xE6u8, 0x00, 0x02, 0x00, 0x03, 0x02, 0x03, 0x1B, 0xFF] {
                bus.write_io(0x3F5, BusWidth::Byte, u32::from(b)).unwrap();
            }
        });

        // The sector landed in the guest buffer over channel 2.
        for i in 0..512usize {
            let got = machine.read_physical_u8(u32::from(BUF) + i as u32);
            let want = (0xA0 + (i & 0x0F)) as u8;
            assert_eq!(got, want, "byte {i} of the sector in memory");
        }

        // The completion interrupt is IRQ6 (the controller raised it; advance the
        // device pump so the bus collects it into the PIC).
        machine.advance_devices(1);
        let pending = with_bus(&mut machine, |bus| bus.interrupt_pending());
        assert!(pending, "FDC completion raised IRQ6");

        // The result phase is seven status bytes ending at sector 3.
        let result = with_bus(&mut machine, |bus| {
            let mut out = Vec::new();
            while bus.read_io(0x3F4, BusWidth::Byte).unwrap() & 0x40 != 0 {
                out.push(bus.read_io(0x3F5, BusWidth::Byte).unwrap() as u8);
            }
            out
        });
        assert_eq!(result.len(), 7, "ST0..N result phase");
        assert_eq!(result[0] & 0xC0, 0x00, "normal termination");
        assert_eq!(result[3], 2, "ending cylinder 2");
        assert_eq!(result[5], 3, "ending sector 3");
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

    /// Build a CD image with one data sector and a stretch of loud audio frames,
    /// for the CD-audio mixing test.
    fn audio_cd(frames: u32) -> CdImage {
        let cue = "TRACK 01 MODE1/2048\nINDEX 01 00:00:00\n\
                   TRACK 02 AUDIO\nINDEX 01 00:00:01\n";
        let mut bin = vec![0u8; cdimage::DATA_SECTOR + frames as usize * cdimage::RAW_SECTOR];
        // Fill the audio region with a loud constant so the mix is clearly nonzero.
        for chunk in bin[cdimage::DATA_SECTOR..].chunks_exact_mut(2) {
            chunk.copy_from_slice(&8000i16.to_le_bytes());
        }
        CdImage::from_cue(cue, bin).unwrap()
    }

    #[test]
    fn play_audio_mixes_cd_audio_into_render_audio() {
        let mut machine = test_machine();
        machine.mount_cd(audio_cd(20));
        // Open the CD volume to full (5-bit registers 0x36/0x37) via the mixer.
        with_bus(&mut machine, |bus| {
            for (index, value) in [(0x36u32, 31u32), (0x37, 31)] {
                bus.write_io(0x224, BusWidth::Byte, index).unwrap();
                bus.write_io(0x225, BusWidth::Byte, value).unwrap();
            }
        });
        // Issue PLAY AUDIO(10) over the secondary-channel ATAPI ports: PACKET
        // command, then the 12-byte CDB. Play from LBA 1 (audio start) for 16
        // frames.
        with_bus(&mut machine, |bus| {
            bus.write_io(0x177, BusWidth::Byte, 0xA0).unwrap(); // PACKET command
            let mut cdb = [0u8; 12];
            cdb[0] = 0x45; // PLAY AUDIO(10)
            cdb[5] = 1; // starting LBA 1
            cdb[8] = 16; // 16 frames
            for b in cdb {
                bus.write_io(0x170, BusWidth::Byte, u32::from(b)).unwrap();
            }
        });
        assert!(machine.cd_loaded());
        let pcm = machine.render_audio(2000);
        assert!(
            pcm.iter().any(|&(l, r)| l != 0 || r != 0),
            "PLAY AUDIO should mix nonzero CD audio into the DAC output"
        );
    }

    #[test]
    fn cd_audio_is_silent_with_the_volume_muted() {
        let mut machine = test_machine();
        machine.mount_cd(audio_cd(20));
        // Leave CD volume at its muted default (0). Start playback.
        with_bus(&mut machine, |bus| {
            bus.write_io(0x177, BusWidth::Byte, 0xA0).unwrap();
            let mut cdb = [0u8; 12];
            cdb[0] = 0x45;
            cdb[5] = 1;
            cdb[8] = 16;
            for b in cdb {
                bus.write_io(0x170, BusWidth::Byte, u32::from(b)).unwrap();
            }
        });
        let pcm = machine.render_audio(2000);
        assert!(
            pcm.iter().all(|&(l, r)| l == 0 && r == 0),
            "a muted CD volume yields silence even while playing"
        );
    }

    #[test]
    fn icdex_install_check_reports_installed() {
        let mut machine = test_machine();
        // The probe pushes DADAh, then the INT pushed IP, CS, FLAGS over it, so
        // the marker sits at SS:SP+6. Stand in for that frame here.
        machine
            .cpu
            .registers
            .set_segment(SegmentIndex::Ss, SegmentRegister::real(0x9000));
        machine.cpu.registers.set_esp(0x0100);
        let marker_addr = 0x9000 * 16 + (0x0100 + 6);
        machine.memory.write_u16(marker_addr, 0xDADA).unwrap();
        machine.cpu.registers.set_eax(0x1100);
        assert!(machine.handle_int2f());
        // AL = FFh means installed.
        assert_eq!(machine.cpu.registers.eax() as u8, 0xFF);
        // The pushed marker is rewritten to ADADh so a strict probe sees the
        // word change (RBIL INTERRUP.K, INT 2F/AX=1100h).
        assert_eq!(machine.memory.read_u16(marker_addr).unwrap(), 0xADAD);
    }

    #[test]
    fn icdex_install_check_ignores_non_dada_marker() {
        let mut machine = test_machine();
        machine
            .cpu
            .registers
            .set_segment(SegmentIndex::Ss, SegmentRegister::real(0x9000));
        machine.cpu.registers.set_esp(0x0100);
        let marker_addr = 0x9000 * 16 + (0x0100 + 6);
        // A pushed word other than DADAh is some other 1100h subfunction. We must
        // not claim installed or touch the stack word.
        machine.memory.write_u16(marker_addr, 0x1234).unwrap();
        machine.cpu.registers.set_eax(0x1100);
        assert!(!machine.handle_int2f());
        assert_eq!(machine.memory.read_u16(marker_addr).unwrap(), 0x1234);
    }

    #[test]
    fn icdex_drive_check_reports_the_cd_drive() {
        let mut machine = test_machine();
        machine.mount_cd(audio_cd(4));
        // AX=1500: BX = drive count, CX = first drive letter (D: = 3).
        machine.cpu.registers.set_eax(0x1500);
        assert!(machine.handle_int2f());
        assert_eq!(machine.cpu.registers.ebx() as u16, 1);
        assert_eq!(
            machine.cpu.registers.ecx() as u16,
            u16::from(CD_DRIVE_NUMBER)
        );
        // AX=150B drive check for D:: BX = ADADh, AX nonzero (supported).
        machine.cpu.registers.set_eax(0x150B);
        machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
        assert!(machine.handle_int2f());
        assert_eq!(machine.cpu.registers.ebx() as u16, 0xADAD);
        assert_ne!(machine.cpu.registers.eax() as u16, 0);
    }

    #[test]
    fn icdex_send_request_read_long_loads_a_sector() {
        let mut machine = test_machine();
        // A small data ISO with a marker per sector.
        let mut bytes = vec![0u8; 4 * cdimage::DATA_SECTOR];
        bytes[2 * cdimage::DATA_SECTOR] = 0x99; // LBA 2 marker
        machine.mount_cd(CdImage::from_iso(bytes).unwrap());

        // Build a READ LONG (0x80) device request header at linear 0x2000, with a
        // transfer buffer at 0x4000. ES:BX -> header via ES base 0, BX = 0x2000.
        let header = 0x2000u32;
        let xfer = 0x4000u32;
        machine.write_physical_u8(header + 2, 0x80); // command READ LONG
        machine.write_physical_u8(header + 0x0D, 0x00); // HSG addressing
        // transfer address dword at 0x0E
        for (i, b) in xfer.to_le_bytes().iter().enumerate() {
            machine.write_physical_u8(header + 0x0E + i as u32, *b);
        }
        // sector count (1) at 0x12
        machine.write_physical_u8(header + 0x12, 1);
        machine.write_physical_u8(header + 0x13, 0);
        // starting sector (LBA 2) dword at 0x14
        for (i, b) in 2u32.to_le_bytes().iter().enumerate() {
            machine.write_physical_u8(header + 0x14 + i as u32, *b);
        }

        machine.cpu.registers.set_eax(0x1510);
        machine.cpu.registers.set_ebx(header); // ES base 0, BX = header
        machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
        assert!(machine.handle_int2f());

        // The sector landed at the transfer address.
        assert_eq!(machine.read_physical_u8(xfer), 0x99);
        // Status word (offset 3) has the done bit set, no error.
        let status = machine.read_guest_word(header + 3);
        assert_eq!(status & 0x8000, 0, "no error bit");
        assert_ne!(status & 0x0100, 0, "done bit set");
    }

    #[test]
    fn icdex_send_request_play_audio_starts_playback() {
        let mut machine = test_machine();
        machine.mount_cd(audio_cd(40));
        let header = 0x2000u32;
        machine.write_physical_u8(header + 2, 0x84); // PLAY AUDIO
        machine.write_physical_u8(header + 0x0D, 0x00); // HSG
        // start sector (LBA 1, the audio track) dword at 0x0E
        for (i, b) in 1u32.to_le_bytes().iter().enumerate() {
            machine.write_physical_u8(header + 0x0E + i as u32, *b);
        }
        // play count (8 frames) dword at 0x12
        for (i, b) in 8u32.to_le_bytes().iter().enumerate() {
            machine.write_physical_u8(header + 0x12 + i as u32, *b);
        }
        machine.cpu.registers.set_eax(0x1510);
        machine.cpu.registers.set_ebx(header);
        machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
        assert!(machine.handle_int2f());
        assert!(machine.ide.device().playback().playing);
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
        // The default CT1745 volume attenuates it by voice (0x32=24, ~-14 dB)
        // and master (0x30=24, ~-14 dB): -16384 * 0.19953^2 ~= -652.
        const BYTE: u8 = 0x40;
        let expected: i32 =
            (-16384.0f32 * 10f32.powf(-14.0 / 20.0) * 10f32.powf(-14.0 / 20.0)) as i32;
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
        // level after the resampler warmup. Playback is clock-driven now, so
        // advance CPU time to let the per-clock producer fill the ring, then
        // render plenty of OPL-native time for the host drainer + resampler.
        machine.advance_devices_clocks(2_500_000);
        let out = machine.render_audio(4_000);
        assert!(!out.is_empty());
        let mid = &out[out.len() / 3..out.len() * 2 / 3];
        let (min_l, max_l) = mid
            .iter()
            .map(|f| f.0)
            .fold((i16::MAX, i16::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
        let center = (i32::from(min_l) + i32::from(max_l)) / 2;
        assert!(
            (center - expected).abs() < 400,
            "DSP DC center {center}, expected ~{expected}"
        );
        // Mono is duplicated to both channels.
        assert!(mid.iter().all(|f| f.0 == f.1), "DSP mono duplicated L/R");
    }

    #[test]
    fn sb_mixer_voice_and_master_volume_attenuate_output() {
        let mut machine = test_machine();
        // Constant 256-byte DC buffer: sample_u8(0x40) = -16384, auto-init.
        for i in 0..256u32 {
            machine.write_physical_u8(0x1_0000 + i, 0x40);
        }
        with_bus(&mut machine, |bus| {
            bus.write_io(0x0B, BusWidth::Byte, 0x59).unwrap();
            bus.write_io(0x02, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x02, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x03, BusWidth::Byte, 0xFF).unwrap();
            bus.write_io(0x03, BusWidth::Byte, 0x00).unwrap();
            bus.write_io(0x83, BusWidth::Byte, 0x01).unwrap();
            bus.write_io(0x0A, BusWidth::Byte, 0x01).unwrap();
            for &b in &[0x41u8, 0x2B, 0x11, 0x48, 0xFF, 0x00, 0x1C] {
                bus.write_io(0x22C, BusWidth::Byte, u32::from(b)).unwrap();
            }
        });

        fn set_reg(machine: &mut Machine, index: u8, value: u8) {
            with_bus(machine, |bus| {
                bus.write_io(0x224, BusWidth::Byte, u32::from(index))
                    .unwrap();
                bus.write_io(0x225, BusWidth::Byte, u32::from(value))
                    .unwrap();
            });
        }
        // Refill the clock-driven ring, then render a window of mixed output.
        fn render(machine: &mut Machine) -> Vec<(i16, i16)> {
            machine.advance_devices_clocks(2_500_000);
            machine.render_audio(4_000)
        }
        fn mid_quiet(out: &[(i16, i16)]) -> bool {
            let mid = &out[out.len() / 3..out.len() * 2 / 3];
            mid.iter().all(|&(l, r)| l.abs() <= 50 && r.abs() <= 50)
        }

        // Voice mute (0x32/0x33 = 0) silences the DSP path regardless of master.
        set_reg(&mut machine, 0x32, 0x00);
        set_reg(&mut machine, 0x33, 0x00);
        assert!(
            mid_quiet(&render(&mut machine)),
            "voice mute silences the DSP output"
        );

        // Master mute (0x30/0x31 = 0) silences the whole mix even at full voice.
        set_reg(&mut machine, 0x32, 0x1F);
        set_reg(&mut machine, 0x33, 0x1F);
        set_reg(&mut machine, 0x30, 0x00);
        set_reg(&mut machine, 0x31, 0x00);
        assert!(
            mid_quiet(&render(&mut machine)),
            "master mute silences the summed output"
        );

        // Defaults (master/voice 24 => -14 dB each) return the attenuated DC level.
        for (idx, val) in [(0x30u8, 24u8), (0x31, 24), (0x32, 24), (0x33, 24)] {
            set_reg(&mut machine, idx, val);
        }
        let restored = render(&mut machine);
        let mid = &restored[restored.len() / 3..restored.len() * 2 / 3];
        let (min_l, max_l) = mid
            .iter()
            .map(|f| f.0)
            .fold((i16::MAX, i16::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
        let center = (i32::from(min_l) + i32::from(max_l)) / 2;
        let expected = (-16384.0f32 * 10f32.powf(-14.0 / 20.0) * 10f32.powf(-14.0 / 20.0)) as i32;
        assert!(
            (center - expected).abs() < 200,
            "restored DC center {center}, expected ~{expected}"
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
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

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
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

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
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

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

        // 4 pixels -> busy_ns = 100 + 4*10 = 140 ns. At 22 MHz (45.4545 ns/clock),
        // three clocks (136 ns drained) leave it busy; the fourth clears it.
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
            Machine::new_dos_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), com)
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
            Machine::new_dos_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), com)
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
            Machine::new_dos_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), com)
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

        // 20 pixels -> busy_ns = 100 + 20*5 = 200 ns. At 22 MHz (45.4545 ns/clock),
        // four clocks (181 ns drained) leave it busy; the fifth clears it.
        machine.advance_devices(4);
        assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
        machine.advance_devices(1);
        assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 0);
    }

    #[test]
    fn dos_com_runs_the_committed_hello_fixture() {
        let mut machine = Machine::new_dos_program(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
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
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
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
            Machine::new_dos_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), com)
                .unwrap();
        available.set_dos_stdin(b"X");
        assert_eq!(
            available.run_until_halt_or_cycles(100_000).unwrap(),
            StopReason::DosExit { code: 0 }
        );
        assert_eq!(available.dos_output(), b"X"); // char path taken, AL echoed

        let mut empty =
            Machine::new_dos_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), com)
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
            Machine::new_dos_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), com)
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

        // 2 pixels written -> busy_ns = 100 + 2*5 = 110 ns. At 22 MHz (45.4545 ns/clock),
        // two clocks (90 ns drained) leave it busy; the third clears it.
        assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
        machine.advance_devices(2);
        assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
        machine.advance_devices(1);
        assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 0);
    }

    #[test]
    fn dos_com_runs_the_committed_echo_fixture() {
        let mut machine = Machine::new_dos_program(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            izarravm_firmware::ECHO_COM,
        )
        .unwrap();
        // The echo filter reads with AH=08h until it sees ^Z (0x1A). With the
        // blocking keyboard ring there is no ISR to refill it in this slice, so the
        // ^Z that ends input has to be seeded along with the data.
        machine.set_dos_stdin(b"hi\x1a");
        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::DosExit { code: 0 });
        assert_eq!(machine.dos_output(), b"hi");
    }

    #[test]
    fn dos_com_reads_a_file_from_c_drive() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("HELLO.TXT"), b"File data 123").unwrap();
        let mut machine = Machine::new_dos_program(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
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

        // 5 pixels -> busy_ns = 100 + 5*10 = 150 ns. At 22 MHz (45.4545 ns/clock),
        // three clocks (136 ns drained) leave it busy; the fourth clears it.
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

        // 16 pixels -> busy_ns = 100 + 16*5 = 180 ns. At 22 MHz (45.4545 ns/clock),
        // three clocks (136 ns drained) leave it busy; the fourth clears it.
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
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
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
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
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
        // 10 000 CPU clocks at 22 MHz with a 25.175 MHz dot clock advances
        // roughly 11 443 dots, well above zero.
        machine.advance_devices(10_000);
        assert!(machine.video().beam_dots() != before || machine.video().frames_completed() > 0);
    }

    #[test]
    fn display_refresh_matches_the_vga_mode() {
        let mut machine = test_machine();
        // Mode 0Dh is a ~359 200-dot frame at the 25.175 MHz dot clock, i.e.
        // ~70 Hz, the classic VGA graphics refresh.
        machine.set_vga_mode_0dh();
        let hz = machine.display_refresh_hz();
        assert!((hz - 70.0).abs() < 1.0, "expected ~70 Hz, got {hz}");
        // Mode 12h (640x480, 525 lines) is the 60 Hz timing.
        machine.set_vga_mode(0x12);
        let hz = machine.display_refresh_hz();
        assert!((hz - 60.0).abs() < 1.0, "expected ~60 Hz, got {hz}");
    }

    #[test]
    fn planar_mode_presents_a_vga_raster() {
        let mut machine = test_machine();
        machine.set_vga_mode_0dh();
        // Mode 0Dh frame is ~359 200 dots; 600 000 CPU clocks at 22 MHz yields
        // ~686 600 dot clocks, enough to complete at least one full frame.
        machine.advance_devices(600_000);
        assert!(matches!(machine.active_display(), ActiveDisplay::VgaRaster));
        assert!(machine.vga_raster().is_some());
    }

    #[test]
    fn text_mode_scanout_through_the_machine() {
        let mut machine = test_machine();
        // A CP437 cell at B8000:0 (the solid block 0xDB) with a white-on-black
        // attribute, written through the bus so it routes to text_memory.
        machine.write_physical_u8(VGA_TEXT_BASE, 0xDB);
        machine.write_physical_u8(VGA_TEXT_BASE + 1, 0x0F);
        // A distinct DAC entry for the foreground index (15): red.
        machine.video_mut().set_dac_entry(15, 63, 0, 0);
        // Enough CPU time to finalize at least one frame.
        machine.advance_devices(600_000);
        assert!(matches!(machine.active_display(), ActiveDisplay::VgaRaster));
        let raster = machine.vga_raster().expect("text presents a VgaRaster");
        assert_eq!(raster.width, 720);
        // The top-left glyph pixel scans out as DAC index 15 (the foreground).
        assert_eq!(raster.pixels[0], 15);
        // Resolved through the live DAC, entry 15 is red.
        assert_eq!(machine.palette_argb()[15], 0x00FF_0000);
    }

    #[test]
    fn cga_graphics_routes_b800_to_the_framebuffer() {
        let mut machine = test_machine();
        // Enter CGA mode 04h (320x200x4) the way INT 10h AH=00 AL=04 would.
        machine.video_mut().set_cga_mode(0x04);
        assert_eq!(machine.video().active_mode(), VideoMode::Cga);
        // A byte written to B800:0000 lands in the CGA framebuffer, not the text
        // buffer. 0b00_01_10_11 decodes to bg/green/red/brown on the default
        // palette (green=2, red=4, brown=6).
        machine.write_physical_u8(VGA_TEXT_BASE, 0b00_01_10_11);
        assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), 0b00_01_10_11);
        let raster = machine.video_mut().render_full_frame();
        assert_eq!(raster.width, 320);
        assert_eq!(raster.height, 262);
        // The first four pixels of scanline 0.
        assert_eq!(&raster.pixels[0..4], &[0, 2, 4, 6]);
    }

    #[test]
    fn cga_odd_scanline_reads_the_high_bank_through_the_machine() {
        let mut machine = test_machine();
        machine.video_mut().set_cga_mode(0x04);
        // Scanline 1 of a CGA frame reads framebuffer offset 0x2000 (the odd bank).
        // Write there through the B800 aperture and confirm it scans out on line 1.
        machine.write_physical_u8(VGA_TEXT_BASE + 0x2000, 0b01_01_01_01);
        let raster = machine.video_mut().render_full_frame();
        // Row 1 starts at offset width*1.
        let row1 = &raster.pixels[320..320 + 4];
        assert_eq!(row1, &[2, 2, 2, 2]); // value 1 -> green(2)
    }

    #[test]
    fn int10_11h_loads_user_font() {
        // A 2-glyph user font (two solid 8x16 blocks) at ES:BP = 4000h:0,
        // overwriting 'A' and 'B'. AL=00 loads it; BH=16 bytes/char, BL=0
        // (table 0), CX=2, DX=41h.
        let rom = rom_with_code(&[
            0xb8, 0x00, 0x40, // mov ax, 4000h
            0x8e, 0xc0, // mov es, ax
            0xbd, 0x00, 0x00, // mov bp, 0
            0xb9, 0x02, 0x00, // mov cx, 2
            0xba, 0x41, 0x00, // mov dx, 41h (first char 'A')
            0xbb, 0x00, 0x10, // mov bx, 1000h (BH=16, BL=0)
            0xb8, 0x00, 0x11, // mov ax, 1100h
            0xcd, 0x10, // int 10h
            0xf4, // hlt
        ]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
        machine.write_guest_block(0x40000, &[0xFF; 32]); // two solid glyphs
        // Display cell 0 = 'A', white on black.
        machine.write_physical_u8(VGA_TEXT_BASE, 0x41);
        machine.write_physical_u8(VGA_TEXT_BASE + 1, 0x0F);
        assert_eq!(
            machine.run_until_halt_or_cycles(1_000_000).unwrap(),
            StopReason::Halted
        );
        // The custom 'A' is solid, so its top row scans out as the foreground.
        // The stock 'A' would be blank on the top row (background), so 15
        // confirms the user font loaded and renders.
        assert_eq!(machine.video().render_text_row(0)[0], 15);
    }

    #[test]
    fn int10_11h_loads_rom_8x16() {
        // First a custom load blanks glyph 0xDB (AL=00); then AL=04 reloads the
        // ROM 8x16 font, restoring the solid full block.
        let rom = rom_with_code(&[
            0xb8, 0x00, 0x40, // mov ax, 4000h
            0x8e, 0xc0, // mov es, ax
            0xbd, 0x00, 0x00, // mov bp, 0
            0xb9, 0x01, 0x00, // mov cx, 1
            0xba, 0xdb, 0x00, // mov dx, 0DBh (full block)
            0xbb, 0x00, 0x10, // mov bx, 1000h (BH=16, BL=0)
            0xb8, 0x00, 0x11, // mov ax, 1100h (user font)
            0xcd, 0x10, // int 10h
            0xbb, 0x00, 0x10, // mov bx, 1000h
            0xb8, 0x04, 0x11, // mov ax, 1104h (ROM 8x16)
            0xcd, 0x10, // int 10h
            0xf4, // hlt
        ]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
        machine.write_guest_block(0x40000, &[0x00; 16]); // a blank glyph for 0xDB
        machine.write_physical_u8(VGA_TEXT_BASE, 0xDB);
        machine.write_physical_u8(VGA_TEXT_BASE + 1, 0x0F);
        assert_eq!(
            machine.run_until_halt_or_cycles(1_000_000).unwrap(),
            StopReason::Halted
        );
        // The ROM reload restored the solid full block; without it the custom
        // blank load would leave the top row as the background (0).
        assert_eq!(machine.video().render_text_row(0)[0], 15);
    }

    #[test]
    fn int10_11h_caps_a_pathological_glyph_count() {
        // CX = 0xFFFF with BH = 16 would read ~16 MB byte-at-a-time. The handler
        // caps the read at 256 glyphs (codes fold modulo 256), so the call still
        // loads the first glyph and returns promptly without stalling or
        // over-allocating.
        let rom = rom_with_code(&[
            0xb8, 0x00, 0x40, // mov ax, 4000h
            0x8e, 0xc0, // mov es, ax
            0xbd, 0x00, 0x00, // mov bp, 0
            0xb9, 0xff, 0xff, // mov cx, 0FFFFh
            0xba, 0x41, 0x00, // mov dx, 41h ('A')
            0xbb, 0x00, 0x10, // mov bx, 1000h (BH=16, BL=0)
            0xb8, 0x00, 0x11, // mov ax, 1100h
            0xcd, 0x10, // int 10h
            0xf4, // hlt
        ]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
        // A solid glyph for 'A' at the first 16 bytes; the rest of the 64 KB
        // page stays zero, so capping the read also proves only the real glyph
        // data is consulted.
        machine.write_guest_block(0x40000, &[0xFF; 16]);
        machine.write_physical_u8(VGA_TEXT_BASE, 0x41);
        machine.write_physical_u8(VGA_TEXT_BASE + 1, 0x0F);
        let reason = machine.run_until_halt_or_cycles(2_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        // The first glyph (solid) loaded and renders as the foreground.
        assert_eq!(machine.video().render_text_row(0)[0], 15);
    }

    #[test]
    fn int10_teletype_and_cursor() {
        let rom = rom_with_code(&[
            0xB8, 0x03, 0x00, 0xCD, 0x10, // set text mode 03h (homes cursor)
            0xB4, 0x0E, 0xB0, b'H', 0xCD, 0x10, // AH=0Eh teletype 'H'
            0xB4, 0x0E, 0xB0, b'i', 0xCD, 0x10, // AH=0Eh teletype 'i'
            0xB4, 0x03, 0xB7, 0x00, 0xCD, 0x10, // AH=03h get cursor (page 0)
            0xF4,
        ]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        // 'H' then 'i' landed at row 0 cols 0,1; cursor now at row 0 col 2.
        assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), b'H');
        assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE + 2), b'i');
        let dx = machine.cpu().registers.edx() as u16;
        assert_eq!(dx, 0x0002, "DH=row 0, DL=col 2");
    }

    #[test]
    fn int10_scroll_window_up_blanks_bottom() {
        // No mode set here: setting a text mode clears the framebuffer, which
        // would wipe the marker the host seeds below before the scroll runs.
        let rom = rom_with_code(&[
            0xB8, 0x01, 0x06, // mov ax,0601h (AH=06h scroll up 1 line)
            0xB7, 0x07, // mov bh,07h (fill attr)
            0xB9, 0x00, 0x00, // mov cx,0000h (top-left 0,0)
            0xBA, 0x4F, 0x18, // mov dx,184Fh (bottom-right row 24 col 79)
            0xCD, 0x10, 0xF4,
        ]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
        // Put a non-space at row 1 col 0; after scroll-up by 1 it lands at row 0.
        machine.write_physical_u8(VGA_TEXT_BASE + 80 * 2, b'X');
        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        assert_eq!(
            machine.read_physical_u8(VGA_TEXT_BASE),
            b'X',
            "row 1 scrolled to row 0"
        );
        assert_eq!(
            machine.read_physical_u8(VGA_TEXT_BASE + 24 * 80 * 2),
            b' ',
            "bottom row blanked"
        );
    }

    #[test]
    fn int10_scroll_window_down_blanks_top() {
        // No mode set here: setting a text mode clears the framebuffer, which
        // would wipe the marker the host seeds below before the scroll runs.
        let rom = rom_with_code(&[
            0xB8, 0x01, 0x07, // mov ax,0701h (AH=07h scroll down 1 line)
            0xB7, 0x07, // mov bh,07h (fill attr)
            0xB9, 0x00, 0x00, // mov cx,0000h (top-left 0,0)
            0xBA, 0x4F, 0x18, // mov dx,184Fh (bottom-right row 24 col 79)
            0xCD, 0x10, 0xF4,
        ]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
        // Put a non-space at row 0 col 0; after scroll-down by 1 it lands at row 1.
        machine.write_physical_u8(VGA_TEXT_BASE, b'Y');
        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        assert_eq!(
            machine.read_physical_u8(VGA_TEXT_BASE + 80 * 2),
            b'Y',
            "row 0 scrolled to row 1"
        );
        assert_eq!(
            machine.read_physical_u8(VGA_TEXT_BASE),
            b' ',
            "top row blanked"
        );
    }

    #[test]
    fn int10_scroll_subwindow_up() {
        // No mode set here: setting a text mode clears the framebuffer, which
        // would wipe the marker the host seeds below before the scroll runs.
        // CX = top-left, DX = bottom-right; for each, the high byte is the row
        // and the low byte is the column: CX=(row<<8)|col, DX=(row<<8)|col.
        let rom = rom_with_code(&[
            0xB8, 0x01, 0x06, // mov ax,0601h (AH=06h scroll up 1 line)
            0xB7, 0x07, // mov bh,07h (fill attr)
            0xB9, 0x04, 0x01, // mov cx,0104h (top-left row 1 col 4)
            0xBA, 0x0A, 0x03, // mov dx,030Ah (bottom-right row 3 col 10)
            0xCD, 0x10, 0xF4,
        ]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
        // Marker inside the window at row 2 col 5; after scroll-up by 1 it lands
        // at row 1 col 5.
        machine.write_physical_u8(VGA_TEXT_BASE + ((2 * 80) + 5) * 2, b'W');
        // Sentinels in cells outside the window (the framebuffer is otherwise
        // pre-blanked with spaces, so seed distinct bytes to prove the scroll's
        // row and column clamping never wrote here): row 0 col 0 is above the
        // window, row 2 col 0 is left of the col-4 left edge.
        machine.write_physical_u8(VGA_TEXT_BASE, b'A');
        machine.write_physical_u8(VGA_TEXT_BASE + (2 * 80) * 2, b'B');
        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        assert_eq!(
            machine.read_physical_u8(VGA_TEXT_BASE + (80 + 5) * 2),
            b'W',
            "row 2 col 5 scrolled to row 1 col 5"
        );
        // A cell above the window (row 0 col 0) is untouched.
        assert_eq!(
            machine.read_physical_u8(VGA_TEXT_BASE),
            b'A',
            "row 0 col 0 outside window left untouched"
        );
        // A cell to the left of the window (row 2 col 0, left edge is col 4) is
        // untouched.
        assert_eq!(
            machine.read_physical_u8(VGA_TEXT_BASE + (2 * 80) * 2),
            b'B',
            "row 2 col 0 left of window left untouched"
        );
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
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

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
    fn int10_returns_to_text_mode() {
        // mov ax,0013h; int 10h; mov ax,0003h; int 10h; hlt
        let rom = rom_with_code(&[
            0xb8, 0x13, 0x00, 0xcd, 0x10, 0xb8, 0x03, 0x00, 0xcd, 0x10, 0xf4,
        ]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

        // Stamp a recognizable pattern into the text buffer before the toggles.
        machine.video_mut().write_u8(0, b'X').unwrap();
        machine.video_mut().write_u8(1, 0x4e).unwrap();
        machine
            .video_mut()
            .write_u8(VGA_TEXT_MEMORY_SIZE - 2, b'Y')
            .unwrap();

        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        // Returning to text hands the display back to the VGA core text path
        // (now a raster) and clears the Margo latch.
        assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
        assert_eq!(machine.video().active_mode(), VideoMode::Text);
        // set_text_mode blanks the buffer to spaces with the 0x07 attribute.
        assert_eq!(machine.video().read_u8(0).unwrap(), b' ');
        assert_eq!(machine.video().read_u8(1).unwrap(), 0x07);
        assert_eq!(
            machine.video().read_u8(VGA_TEXT_MEMORY_SIZE - 2).unwrap(),
            b' '
        );
    }

    #[test]
    fn int10_0bh_sets_border_overscan() {
        // mov ax,0b00h; mov bx,0005h; int 10h; hlt  (AH=0Bh, BH=0 border, BL=5)
        let rom = rom_with_code(&[0xb8, 0x00, 0x0b, 0xbb, 0x05, 0x00, 0xcd, 0x10, 0xf4]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        assert_eq!(machine.video().overscan(), 5);
    }

    #[test]
    fn int10_ah05_sets_the_text_page_via_start_address() {
        // mov ax,0501h; int 10h; hlt  (AH=05h, AL=1 -> display page 1)
        let rom = rom_with_code(&[0xb8, 0x01, 0x05, 0xcd, 0x10, 0xf4]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        // Page 1 sits at byte 4096 = cell 2048. AH=05h routes through
        // set_start_address (the vretrace latch), so the value is buffered in
        // pending_start before the next frame boundary applies it.
        assert_eq!(
            machine.video().pending_start_address(),
            Some(2048),
            "AH=05h page 1 buffers start address 2048 (cell)"
        );
        assert_eq!(
            machine.video().crtc_start_address(),
            0,
            "start address applies at the next vretrace, not mid-frame"
        );
    }

    #[test]
    fn int10_ah05_page_flip_scrolls_through_the_machine() {
        // Drive a full AH=05h page flip end-to-end: pre-seed page 0 and page 1
        // with distinct glyphs, call the BIOS service for page 1, run a frame
        // so the latch applies, and confirm the pixel scanout reads page 1.
        //   mov ax,0501h ; AH=05h, AL=1 (display page 1)
        //   int 10h
        //   hlt
        let rom = rom_with_code(&[0xb8, 0x01, 0x05, 0xcd, 0x10, 0xf4]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

        // Page 0 cell 0 = 'A'; page 1 cell 0 (cell 2048, byte 4096) = 'Z'.
        let video = machine.video_mut();
        video.write_u8(0, b'A').unwrap();
        video.write_u8(1, 0x0F).unwrap();
        video.write_u8(4096, b'Z').unwrap();
        video.write_u8(4097, 0x0F).unwrap();

        machine.run_until_halt_or_cycles(1_000_000).unwrap();

        // The latch is buffered; the start address has not applied yet.
        let video = machine.video_mut();
        assert_eq!(
            video.frame().cells[0].character,
            b'A',
            "before vretrace the displayed page is still 0"
        );
        // Advance one frame so finalize_frame applies the buffered start address.
        let dots = video.frame_dots();
        video.advance(dots);
        assert_eq!(
            video.frame().cells[0].character,
            b'Z',
            "after vretrace the displayed page scrolls to page 1"
        );
    }

    #[test]
    fn int10_10h_sets_palette_register() {
        // mov ax,1000h; mov bx,0901h; int 10h; hlt  (AH=10h AL=00, BL=1, BH=9)
        let rom = rom_with_code(&[0xb8, 0x00, 0x10, 0xbb, 0x01, 0x09, 0xcd, 0x10, 0xf4]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        assert_eq!(machine.video().attr_palette_reg(1), 9);
    }

    #[test]
    fn int10_10h_sets_individual_dac() {
        // mov ax,1010h; mov bx,0028h; mov dx,3f00h; mov cx,0000h; int 10h; hlt
        // (AH=10h AL=10, BX=40, DH=63 R, CH=0 G, CL=0 B)
        let rom = rom_with_code(&[
            0xb8, 0x10, 0x10, 0xbb, 0x28, 0x00, 0xba, 0x00, 0x3f, 0xb9, 0x00, 0x00, 0xcd, 0x10,
            0xf4,
        ]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        assert_eq!(machine.video().dac_entry(40), [63, 0, 0]);
    }

    #[test]
    fn int10_10h_sets_dac_block() {
        // ES:DX -> a 3-triple buffer at 1000:0000 (physical 0x10000).
        // mov ax,1000h; mov es,ax; mov dx,0; mov ax,1012h; mov bx,000ah; mov cx,3; int 10h; hlt
        let rom = rom_with_code(&[
            0xb8, 0x00, 0x10, 0x8e, 0xc0, 0xba, 0x00, 0x00, 0xb8, 0x12, 0x10, 0xbb, 0x0a, 0x00,
            0xb9, 0x03, 0x00, 0xcd, 0x10, 0xf4,
        ]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

        // The three triples at 0x10000: red, green, blue.
        for (i, &b) in [63u8, 0, 0, 0, 63, 0, 0, 0, 63].iter().enumerate() {
            machine.write_physical_u8(0x1_0000 + i as u32, b);
        }

        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        assert_eq!(machine.video().dac_entry(10), [63, 0, 0]);
        assert_eq!(machine.video().dac_entry(11), [0, 63, 0]);
        assert_eq!(machine.video().dac_entry(12), [0, 0, 63]);
    }

    #[test]
    fn int10_10h_gets_dac_block() {
        // AL=17 reads CX DAC entries starting at BX into ES:DX.
        // mov ax,1000h; mov es,ax; mov dx,0; mov ax,1017h; mov bx,000ah; mov cx,3; int 10h; hlt
        let rom = rom_with_code(&[
            0xb8, 0x00, 0x10, 0x8e, 0xc0, 0xba, 0x00, 0x00, 0xb8, 0x17, 0x10, 0xbb, 0x0a, 0x00,
            0xb9, 0x03, 0x00, 0xcd, 0x10, 0xf4,
        ]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

        // Seed DAC entries 10/11/12 with known values, then let the readback run.
        machine.video_mut().set_dac_entry(10, 12, 34, 56);
        machine.video_mut().set_dac_entry(11, 1, 2, 3);
        machine.video_mut().set_dac_entry(12, 63, 63, 63);

        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        // The handler wrote CX*3 bytes to 0x10000.
        assert_eq!(machine.read_physical_u8(0x1_0000), 12);
        assert_eq!(machine.read_physical_u8(0x1_0001), 34);
        assert_eq!(machine.read_physical_u8(0x1_0002), 56);
        assert_eq!(machine.read_physical_u8(0x1_0006), 63);
        assert_eq!(machine.read_physical_u8(0x1_0007), 63);
        assert_eq!(machine.read_physical_u8(0x1_0008), 63);
    }

    #[test]
    fn int10_10h_reads_overscan() {
        // AL=01 sets the overscan to BH=0x2A, then AL=08 reads it back into BH.
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0x1001);
        m.cpu.registers.set_ebx(0x2A00); // BH = 0x2A
        m.handle_int10();
        m.cpu.registers.set_eax(0x1008);
        m.cpu.registers.set_ebx(0x0000);
        m.handle_int10();
        assert_eq!((m.cpu.registers.ebx() as u16 >> 8) as u8, 0x2A);
    }

    #[test]
    fn int10_10h_reads_all_palette_registers() {
        // AL=09 writes the 16 palette registers + overscan to ES:DX. With the
        // power-up palette (reg N = N) and overscan 0, expect 0,1,...,15,0.
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0x1009);
        m.cpu
            .registers
            .set_segment(SegmentIndex::Es, SegmentRegister::real(0x1000));
        m.cpu.registers.set_edx(0x0000);
        m.handle_int10();
        for i in 0..16u8 {
            assert_eq!(m.read_physical_u8(0x1_0000 + u32::from(i)), i);
        }
        assert_eq!(
            m.read_physical_u8(0x1_0010),
            0,
            "overscan trails the 16 regs"
        );
    }

    #[test]
    fn int10_10h_sums_dac_block_to_gray() {
        // AL=1B sums BX..BX+CX DAC entries to gray with NTSC luma weights.
        let mut m = int15_machine(16);
        m.video_mut().set_dac_entry(5, 63, 0, 0); // pure red
        m.video_mut().set_dac_entry(6, 0, 63, 0); // pure green
        m.cpu.registers.set_eax(0x101B);
        m.cpu.registers.set_ebx(0x0005); // start at index 5
        m.cpu.registers.set_ecx(0x0002); // two entries
        m.handle_int10();
        // Red gray = 63*77>>8 = 18; green gray = 63*151>>8 = 37. Each entry is now
        // an equal-component gray.
        let [r5, g5, b5] = m.video().dac_entry(5);
        assert_eq!((r5, g5, b5), (18, 18, 18));
        let [r6, g6, b6] = m.video().dac_entry(6);
        assert_eq!((r6, g6, b6), (37, 37, 37));
    }

    #[test]
    fn int10_10h_reads_dac_page_state_default() {
        // AL=1A reports the power-up DAC paging state: mode 0 (BL), page 0 (BH).
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0x101A);
        m.cpu.registers.set_ebx(0xFFFF);
        m.handle_int10();
        assert_eq!(m.cpu.registers.ebx() as u16, 0x0000);
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
        // clocks at 22 MHz = ~454 ns), letting the pump consume fill 2.
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
        //   mov bx,0x10 / mov ah,0x48 / int 21 ; allocate 16 paras
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
            Machine::new_dos_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), com)
                .unwrap();
        let reason = machine.run_until_halt_or_cycles(100_000).unwrap();
        assert_eq!(reason, StopReason::DosExit { code: 0 });
        let mem = machine.memory();
        // PSP at 0x0100 -> linear 0x1000; the program stored words at offsets 0x200..0x205.
        assert_eq!(mem.read_u16(0x1200).unwrap(), 0x0000); // ES from IVT[0x21] (stub segment)
        assert_eq!(mem.read_u16(0x1202).unwrap(), 0x0600); // BX from IVT[0x21] (stub offset)
        // AH=48h returns a data segment that follows the seeded BLASTER=/SETSOUND=
        // env block plus the new block's own reserved MCB header. Derive the
        // expected segment from the env block so the assertion tracks the env size,
        // not a hardcoded value. The block ends with the DOS 3.0+ argv0 trailer: a
        // terminator NUL, a WORD count of 1, and the ASCIIZ program path, so account
        // for it here.
        let env_seg = mem.read_u16(0x1000 + 0x2c).unwrap();
        let strings = sound_blaster_env_entries(&SoundBlasterConfig::default())
            .iter()
            .map(|(key, value)| key.len() + 1 + value.len() + 1)
            .sum::<usize>()
            + 1; // the terminating empty string
        let argv0_trailer = 2 + izarravm_dos::DEFAULT_ARGV0.len() + 1; // WORD count + ASCIIZ path
        let env_paras = (strings + argv0_trailer).div_ceil(16) as u16;
        // env_seg + env_paras is the env block's first free paragraph (its MCB header
        // slot); the AH=48h data segment is one paragraph above that header.
        assert_eq!(
            mem.read_u16(0x1204).unwrap(),
            env_seg + env_paras + 1,
            "AH=48h allocated segment follows the env block and its MCB header"
        );
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
    fn gc06_moved_aperture_routes_graphics_access_to_the_vga() {
        // Mode 13h: the default GC06 leaves the framebuffer at A0000, so an A0000
        // write still lands in the chain-4 plane (offset 6 -> plane 2, plane-offset
        // 1). Then move the aperture to the 32 KB B8000 window through GC06 and
        // confirm a B8000 access now routes to the VGA, while the default A0000
        // path stays exactly as it was.
        let mut machine = test_machine();
        machine.video_mut().set_mode13h();
        assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);

        // Default aperture: A0000 access routes to the chain-4 datapath unchanged.
        machine.write_physical_u8(0x000A_0000 + 6, 0xA5);
        assert_eq!(
            machine.video().plane_byte(2, 1),
            0xA5,
            "default A0000 window still routes to the VGA"
        );

        // Move the aperture to B8000 (GC06 memory map select = 0b11, a 32 KB
        // window): write index 06h then value 0b1100.
        machine.video_mut().write_port(0x3CE, 0x06);
        machine.video_mut().write_port(0x3CF, 0b1100);
        let ap = machine.video().gfx_aperture();
        assert_eq!((ap.base, ap.length), (0x000B_8000, 0x0000_8000));

        // A B8000 access in the moved window routes to the VGA chain-4 datapath.
        // Offset 10 -> plane 10 & 3 = 2, plane-offset 10 >> 2 = 2.
        machine.write_physical_u8(0x000B_8000 + 10, 0x7E);
        assert_eq!(
            machine.video().plane_byte(2, 2),
            0x7E,
            "the moved B8000 window routes to the VGA, not the text buffer"
        );
        // Read-back through the moved window returns the byte from the plane.
        assert_eq!(machine.read_physical_u8(0x000B_8000 + 10), 0x7E);
    }

    #[test]
    fn gc06_default_aperture_keeps_text_routing_at_b8000() {
        // In text mode the B8000 window is the character buffer regardless of GC06;
        // the moved-aperture routing only applies to graphics modes. Writing a
        // char/attr pair at B8000 must reach the text buffer, not a VGA plane.
        let mut machine = test_machine();
        machine.write_physical_u8(VGA_TEXT_BASE, b'Z');
        machine.write_physical_u8(VGA_TEXT_BASE + 1, 0x0F);
        assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), b'Z');
        assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE + 1), 0x0F);
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
    fn mode_x_pel_pan_smooth_scroll_through_the_machine() {
        let mut machine = test_machine();
        // Mode 13h, then unchained mode X.
        machine.video_mut().set_mode13h();
        machine.video_mut().write_port(0x3C4, 0x04);
        machine.video_mut().write_port(0x3C5, 0x06);
        assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
        // Abrash's 320x240 vertical timing through the CRTC ports (Black Book
        // Listing 47.1): double-scanned, 240 source rows over 480 scanlines.
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
        // Distinct bytes per plane at plane offset 0 (values above 0x3F prove the
        // 8-bit-direct DAC index is scanned out, not masked to 6 bits).
        let plane_byte: [u8; 4] = [0x40, 0x50, 0x60, 0x70];
        for (plane, &val) in plane_byte.iter().enumerate() {
            machine.video_mut().write_port(0x3C4, 0x02);
            machine.video_mut().write_port(0x3C5, 1u8 << plane); // map mask = this plane
            machine.video_mut().write_port(0x3CE, 0x08);
            machine.video_mut().write_port(0x3CF, 0xFF); // bit mask 0xFF, write mode 0
            machine.write_physical_u8(0x000A_0000, val);
        }
        // For each pel-pan 1..3, reset the attribute flip-flop, write AC index 0x13
        // then the pan value, run two frame periods, and assert the leftmost column
        // scans out plane `pan` at plane offset 0: the fine-shifted pixel, not plane 0.
        for pan in 1u8..=3 {
            machine.video_mut().read_status1(); // reset attr flip-flop to index mode
            machine.video_mut().write_port(0x3C0, 0x13); // attr index 0x13 (pixel pan)
            machine.video_mut().write_port(0x3C0, pan); // pel-pan value
            // Pel-pan is live (not latched): it takes effect at the scanline of the
            // write, so the in-progress frame's early rows still hold the prior pan.
            // Two frame periods flush that frame and then render a clean one whose row
            // zero is scanned after the write.
            machine.advance_devices(500_000); // flush the in-progress (mixed-pan) frame
            machine.advance_devices(500_000); // render a full frame with the new pan
            let raster = machine.vga_raster().expect("a frame presented");
            assert_eq!(
                raster.pixels[0], plane_byte[pan as usize],
                "pel-pan {pan} scans out plane {pan} at the leftmost column"
            );
        }
    }

    #[test]
    fn mode13h_320x200_through_the_machine() {
        let mut machine = test_machine();
        // INT 10h AH=00h AL=13h installs chained mode 13h; set_mode13h is its
        // programmatic equivalent (the INT path is proven by
        // int10_mode13h_routes_a000_through_chain4).
        machine.video_mut().set_mode13h();
        assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
        // Chain-4 routes the A0000 byte at offset 6 to plane 6 & 3 = 2 at plane
        // offset 6 >> 2 = 1. 0xC2 has bits above 0x3F, proving no 6-bit mask.
        machine.write_physical_u8(0x000A_0000 + 6, 0xC2);
        // Complete a frame (the standard mode-13h frame is ~359 200 dots; 500 000
        // clocks is ~503 500 dots, enough to cross one frame and present).
        machine.advance_devices(500_000);
        let raster = machine.vga_raster().expect("a frame presented");
        assert_eq!(raster.width, 320);
        assert_eq!(raster.height, 449, "mode-13h vertical total");
        // Column 6 of row 0 scans out the written 0xC2, as the 8-bit DAC index
        // directly.
        assert_eq!(
            raster.pixels[6], 0xC2,
            "mode-13h pixel scans out at its column with its full 8-bit value"
        );
    }

    #[test]
    fn mode13h_pel_pan_smooth_scroll_through_the_machine() {
        let mut machine = test_machine();
        machine.video_mut().set_mode13h();
        assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
        // Chain-4 writes the byte at A0000 offset p straight to plane p at plane
        // offset 0, so four writes at offsets 0..3 mark one distinct byte per plane
        // there (values above 0x3F prove the 8-bit-direct DAC index is scanned out,
        // not masked to 6 bits).
        let plane_byte: [u8; 4] = [0x40, 0x50, 0x60, 0x70];
        for (plane, &val) in plane_byte.iter().enumerate() {
            machine.write_physical_u8(0x000A_0000 + plane as u32, val);
        }
        // For each pel-pan 1..3, reset the attribute flip-flop, write AC index 0x13
        // then the pan value, run two frame periods, and assert the leftmost column
        // scans out plane `pan` at plane offset 0: the fine-shifted pixel.
        for pan in 1u8..=3 {
            machine.video_mut().read_status1(); // reset attr flip-flop to index mode
            machine.video_mut().write_port(0x3C0, 0x13); // attr index 0x13 (pixel pan)
            machine.video_mut().write_port(0x3C0, pan); // pel-pan value
            // Pel-pan is live (not latched): it takes effect at the scanline of the
            // write, so the in-progress frame's early rows still hold the prior pan.
            // Two frame periods flush that frame and then render a clean one whose row
            // zero is scanned after the write.
            machine.advance_devices(500_000); // flush the in-progress (mixed-pan) frame
            machine.advance_devices(500_000); // render a full frame with the new pan
            let raster = machine.vga_raster().expect("a frame presented");
            assert_eq!(
                raster.pixels[0], plane_byte[pan as usize],
                "pel-pan {pan} scans out plane {pan} at the leftmost column"
            );
        }
    }

    #[test]
    fn mode13h_line_compare_split_through_the_machine() {
        let mut machine = test_machine();
        machine.video_mut().set_mode13h();
        assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
        // A split at scan-counter line 200, well inside the 400 active scanlines.
        // The line-compare bits are mode-agnostic (CRTC 18h + 07h.4 + 09h.6), and
        // mode 13h does not honor guest vertical-CRTC bangs, so writing 07h/09h
        // here clears only their line-compare bits, leaving the fixed 320x200
        // timing intact. The default line_compare 0x3FF holds bits 8 and 9 set, so
        // they must be cleared or the low byte alone yields 0x3C8 (no split).
        machine.video_mut().write_port(0x3D4, 0x07);
        machine.video_mut().write_port(0x3D5, 0x00); // clear line-compare bit 8
        machine.video_mut().write_port(0x3D4, 0x09);
        machine.video_mut().write_port(0x3D5, 0x00); // clear line-compare bit 9
        machine.video_mut().write_port(0x3D4, 0x18);
        machine.video_mut().write_port(0x3D5, 200); // line compare low byte = 200
        // Mark plane 0, offset 0 (pixel 0 of any scanline reading offset 0). 0xC2
        // has bits above 0x3F, proving the 8-bit DAC index is read directly.
        machine.write_physical_u8(0x000A_0000, 0xC2); // chain-4: plane 0, offset 0
        // Scroll the top region to cleared VRAM, buffered until the next vertical
        // retrace. Two frame periods: the first latches the start address, the second
        // renders with it.
        machine.video_mut().write_port(0x3D4, 0x0C);
        machine.video_mut().write_port(0x3D5, 0x40); // start address high = 0x40
        machine.video_mut().write_port(0x3D4, 0x0D);
        machine.video_mut().write_port(0x3D5, 0x00); // start address low -> 0x4000
        machine.advance_devices(500_000);
        machine.advance_devices(500_000);
        let raster = machine.vga_raster().expect("a frame presented");
        assert_eq!(raster.width, 320, "mode-13h width");
        let w = raster.width as usize;
        // A top scanline (50 < 200) reads the scrolled, cleared region: 0.
        assert_eq!(
            raster.pixels[50 * w],
            0,
            "top region is scrolled to cleared VRAM"
        );
        // The first split scanline (201 = line_compare + 1) reads offset 0 (the
        // marked byte), as the full 8-bit DAC index.
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
            Machine::new_dos_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), com)
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
            Machine::new_dos_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), com)
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

    // The EXEC integration fixtures are nasm-assembled .COM programs (nasm 3.01,
    // -f bin, org 0x100). Their source is in the comment above each const so the
    // bytes are auditable without re-running the assembler.

    // child.asm: write "CHILD\n" to stdout, exit 7.
    //   mov ah,0x40; mov bx,1; mov cx,6; mov dx,msg; int 0x21
    //   mov ax,0x4c07; int 0x21
    //   msg: db "CHILD",0x0a
    const CHILD_COM: &[u8] = &[
        0xb4, 0x40, 0xbb, 0x01, 0x00, 0xb9, 0x06, 0x00, 0xba, 0x12, 0x01, 0xcd, 0x21, 0xb8, 0x07,
        0x4c, 0xcd, 0x21, 0x43, 0x48, 0x49, 0x4c, 0x44, 0x0a,
    ];

    // parent.asm: EXEC C:\CHILD.COM, read AH=4Dh, print the code digit, exit 0
    // (on EXEC failure print '!' and exit 1).
    //   mov dx,name; mov bx,epb; mov ax,0x4b00; int 0x21; jc fail
    //   mov ah,0x4d; int 0x21; add al,0x30; mov dl,al; mov ah,0x02; int 0x21
    //   mov ax,0x4c00; int 0x21
    //   fail: mov dl,'!'; mov ah,0x02; int 0x21; mov ax,0x4c01; int 0x21
    //   name: db "C:\CHILD.COM",0
    //   epb: dw 0,0,0,0,0,0,0
    const PARENT_COM: &[u8] = &[
        0xba, 0x29, 0x01, 0xbb, 0x36, 0x01, 0xb8, 0x00, 0x4b, 0xcd, 0x21, 0x72, 0x11, 0xb4, 0x4d,
        0xcd, 0x21, 0x04, 0x30, 0x88, 0xc2, 0xb4, 0x02, 0xcd, 0x21, 0xb8, 0x00, 0x4c, 0xcd, 0x21,
        0xb2, 0x21, 0xb4, 0x02, 0xcd, 0x21, 0xb8, 0x01, 0x4c, 0xcd, 0x21, 0x43, 0x3a, 0x5c, 0x43,
        0x48, 0x49, 0x4c, 0x44, 0x2e, 0x43, 0x4f, 0x4d, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    // child20.asm: write "Z" to stdout, then INT 20h (terminate, exit 0).
    //   mov ah,0x40; mov bx,1; mov cx,1; mov dx,msg; int 0x21; int 0x20
    //   msg: db "Z"
    const CHILD20_COM: &[u8] = &[
        0xb4, 0x40, 0xbb, 0x01, 0x00, 0xb9, 0x01, 0x00, 0xba, 0x0f, 0x01, 0xcd, 0x21, 0xcd, 0x20,
        0x5a,
    ];

    // failparent.asm: EXEC a missing C:\NOPE.COM; on CF print 'F' and exit 0.
    //   mov dx,name; mov bx,epb; mov ax,0x4b00; int 0x21; jnc bad
    //   mov dl,'F'; mov ah,0x02; int 0x21; mov ax,0x4c00; int 0x21
    //   bad: mov ax,0x4c02; int 0x21
    //   name: db "C:\NOPE.COM",0
    //   epb: dw 0,0,0,0,0,0,0
    const FAILPARENT_COM: &[u8] = &[
        0xba, 0x1d, 0x01, 0xbb, 0x29, 0x01, 0xb8, 0x00, 0x4b, 0xcd, 0x21, 0x73, 0x0b, 0xb2, 0x46,
        0xb4, 0x02, 0xcd, 0x21, 0xb8, 0x00, 0x4c, 0xcd, 0x21, 0xb8, 0x02, 0x4c, 0xcd, 0x21, 0x43,
        0x3a, 0x5c, 0x4e, 0x4f, 0x50, 0x45, 0x2e, 0x43, 0x4f, 0x4d, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    // ovparent.asm: allocate 16 paragraphs, EXEC AL=3 to load C:\OV.BIN at that
    // segment, read [es:di] (di=0) and print it (on any failure print '?' and
    // exit 1). Uses the ModR/M mov r8,[reg] form (0x8a), not the direct-address
    // moffs8 form (0xa0) which the 80386 core does not implement yet.
    //   mov ah,0x48; mov bx,16; int 0x21; jc fail
    //   mov bx,epb; mov [bx],ax; mov dx,name; mov ax,0x4b03; int 0x21; jc fail
    //   mov bx,epb; mov es,[bx]; xor di,di; mov al,[es:di]
    //   mov dl,al; mov ah,0x02; int 0x21; mov ax,0x4c00; int 0x21
    //   fail: mov dl,'?'; mov ah,0x02; int 0x21; mov ax,0x4c01; int 0x21
    //   name: db "C:\OV.BIN",0
    //   epb: dw 0,0
    const OVPARENT_COM: &[u8] = &[
        0xb4, 0x48, 0xbb, 0x10, 0x00, 0xcd, 0x21, 0x72, 0x24, 0xbb, 0x42, 0x01, 0x89, 0x07, 0xba,
        0x38, 0x01, 0xb8, 0x03, 0x4b, 0xcd, 0x21, 0x72, 0x15, 0xbb, 0x42, 0x01, 0x8e, 0x07, 0x31,
        0xff, 0x26, 0x8a, 0x05, 0x88, 0xc2, 0xb4, 0x02, 0xcd, 0x21, 0xb8, 0x00, 0x4c, 0xcd, 0x21,
        0xb2, 0x3f, 0xb4, 0x02, 0xcd, 0x21, 0xb8, 0x01, 0x4c, 0xcd, 0x21, 0x43, 0x3a, 0x5c, 0x4f,
        0x56, 0x2e, 0x42, 0x49, 0x4e, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    // envparent.asm: EXEC C:\CHILD.COM with env_source=0 (inherit) and exit 0 on
    // success (on EXEC failure print '!' and exit 1). The EPB env word is 0, so the
    // child inherits the parent's environment instead of receiving an empty one.
    //   mov dx,name; mov bx,epb; mov ax,0x4b00; int 0x21; jc fail
    //   mov ax,0x4c00; int 0x21
    //   fail: mov dl,'!'; mov ah,0x02; int 0x21; mov ax,0x4c01; int 0x21
    //   name: db "C:\CHILD.COM",0
    //   epb: dw 0,0,0,0,0,0,0
    const ENV_PARENT_COM: &[u8] = &[
        0xba, 0x1d, 0x01, 0xbb, 0x2a, 0x01, 0xb8, 0x00, 0x4b, 0xcd, 0x21, 0x72, 0x05, 0xb8, 0x00,
        0x4c, 0xcd, 0x21, 0xb2, 0x21, 0xb4, 0x02, 0xcd, 0x21, 0xb8, 0x01, 0x4c, 0xcd, 0x21, 0x43,
        0x3a, 0x5c, 0x43, 0x48, 0x49, 0x4c, 0x44, 0x2e, 0x43, 0x4f, 0x4d, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    // envchild.asm: read the env segment from PSP:0x2C (DS=PSP), point DS at the
    // inherited env block, AH=40h-write the first 24 bytes (exactly
    // "BLASTER=A220 I5 D1 H5 T6"), exit 0. Identical bytes to the top-level
    // BLASTER reader, but reached via EXEC so it observes only the inherited env.
    //   mov ax,[0x2c]; mov ds,ax; xor dx,dx; mov cx,24; mov bx,1
    //   mov ah,0x40; int 0x21; mov ax,0x4c00; int 0x21
    const ENV_CHILD_COM: &[u8] = &[
        0x8b, 0x06, 0x2c, 0x00, 0x8e, 0xd8, 0x31, 0xd2, 0xb9, 0x18, 0x00, 0xbb, 0x01, 0x00, 0xb4,
        0x40, 0xcd, 0x21, 0xb8, 0x00, 0x4c, 0xcd, 0x21,
    ];

    #[test]
    fn dos_program_execs_a_child_and_reads_its_return_code() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CHILD.COM"), CHILD_COM).unwrap();
        let mut machine =
            Machine::new_dos_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), PARENT_COM)
                .unwrap();
        machine.mount_c_drive(izarravm_dos::HostDrive::mount_c(dir.path()).unwrap());
        let reason = machine.run_until_halt_or_cycles(2_000_000).unwrap();
        assert_eq!(reason, StopReason::DosExit { code: 0 });
        let out = machine.dos_output();
        assert!(
            out.windows(5).any(|w| w == b"CHILD"),
            "child output missing"
        );
        assert!(out.contains(&b'7'), "return-code digit missing");
    }

    #[test]
    fn dos_child_terminating_via_int20_resumes_parent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CHILD.COM"), CHILD20_COM).unwrap();
        let mut machine =
            Machine::new_dos_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), PARENT_COM)
                .unwrap();
        machine.mount_c_drive(izarravm_dos::HostDrive::mount_c(dir.path()).unwrap());
        let reason = machine.run_until_halt_or_cycles(2_000_000).unwrap();
        assert_eq!(reason, StopReason::DosExit { code: 0 });
        let out = machine.dos_output();
        assert!(out.contains(&b'Z'), "child marker missing");
        assert!(out.contains(&b'0'), "INT 20h exit-code digit missing");
    }

    #[test]
    fn dos_failed_exec_leaves_parent_running() {
        let dir = tempfile::tempdir().unwrap();
        let mut machine = Machine::new_dos_program(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            FAILPARENT_COM,
        )
        .unwrap();
        machine.mount_c_drive(izarravm_dos::HostDrive::mount_c(dir.path()).unwrap());
        let reason = machine.run_until_halt_or_cycles(2_000_000).unwrap();
        assert_eq!(reason, StopReason::DosExit { code: 0 });
        assert_eq!(machine.dos_output(), b"F");
    }

    #[test]
    fn dos_program_loads_an_overlay() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("OV.BIN"), [b'Z']).unwrap();
        let mut machine = Machine::new_dos_program(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            OVPARENT_COM,
        )
        .unwrap();
        machine.mount_c_drive(izarravm_dos::HostDrive::mount_c(dir.path()).unwrap());
        let reason = machine.run_until_halt_or_cycles(2_000_000).unwrap();
        assert_eq!(reason, StopReason::DosExit { code: 0 });
        assert_eq!(machine.dos_output(), b"Z");
    }

    // --- BLASTER environment seeding ---

    /// Walk the env block at `seg` back into (KEY, VALUE) pairs, the way a DOS
    /// game scans the segment named by PSP:0x2C.
    fn parse_env_block(machine: &Machine, seg: u16) -> Vec<(String, String)> {
        let mem = machine.memory();
        let base = usize::from(seg) * 16;
        let mut entries = Vec::new();
        let mut offset = 0usize;
        loop {
            let mut bytes = Vec::new();
            loop {
                let byte = mem.read_u8(base + offset).unwrap();
                offset += 1;
                if byte == 0 {
                    break;
                }
                bytes.push(byte);
            }
            if bytes.is_empty() {
                break; // the terminating empty string
            }
            let entry = String::from_utf8(bytes).unwrap();
            let (key, value) = entry.split_once('=').expect("KEY=VALUE");
            entries.push((key.to_string(), value.to_string()));
        }
        entries
    }

    /// The env-segment pointer the loader wrote into PSP:0x2C, or 0 if unset.
    fn psp_env_segment(machine: &Machine) -> u16 {
        machine
            .memory()
            .read_u16(usize::from(DOS_LOAD_SEGMENT) * 16 + 0x2c)
            .unwrap()
    }

    #[test]
    fn sound_blaster_env_entries_default_config() {
        let entries = sound_blaster_env_entries(&SoundBlasterConfig::default());
        assert_eq!(
            entries,
            vec![
                ("BLASTER".to_string(), "A220 I5 D1 H5 T6".to_string()),
                ("SETSOUND".to_string(), "A220 I5 D1 H5 T6".to_string()),
            ]
        );
    }

    #[test]
    fn sound_blaster_env_entries_non_default_routing() {
        let config = SoundBlasterConfig {
            enabled: true,
            irq: SbIrq::I7,
            dma: SbDma8::D3,
            high_dma: SbDma16::D5,
        };
        assert_eq!(
            sound_blaster_env_entries(&config),
            vec![
                ("BLASTER".to_string(), "A220 I7 D3 H5 T6".to_string()),
                ("SETSOUND".to_string(), "A220 I7 D3 H5 T6".to_string()),
            ]
        );
    }

    #[test]
    fn sound_blaster_env_entries_disabled_omits_the_string() {
        let config = SoundBlasterConfig {
            enabled: false,
            ..SoundBlasterConfig::default()
        };
        assert!(sound_blaster_env_entries(&config).is_empty());
    }

    #[test]
    fn new_dos_program_seeds_psp_env_pointer_with_blaster() {
        // A trivial exit-only program is enough: the env is seeded at load.
        let com: &[u8] = &[0xb8, 0x00, 0x4c, 0xcd, 0x21];
        let machine =
            Machine::new_dos_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), com)
                .unwrap();
        let env_seg = psp_env_segment(&machine);
        assert_ne!(env_seg, 0, "PSP:0x2C must name the env segment");
        // The env data sits one paragraph above the 64 KiB .COM program block
        // (PSP:0x02), past the env block's reserved MCB header.
        let prog_top = machine
            .memory()
            .read_u16(usize::from(DOS_LOAD_SEGMENT) * 16 + 2)
            .unwrap();
        assert_eq!(env_seg, prog_top + 1);
        assert_eq!(
            parse_env_block(&machine, env_seg),
            vec![
                ("BLASTER".to_string(), "A220 I5 D1 H5 T6".to_string()),
                ("SETSOUND".to_string(), "A220 I5 D1 H5 T6".to_string()),
            ]
        );
    }

    #[test]
    fn dos_env_blaster_is_visible_to_a_guest_program() {
        // org 0x100: load the env segment from PSP:0x2C into DS, then AH=40h-write
        // the first 24 bytes of the env block to stdout. Those bytes are exactly
        // "BLASTER=A220 I5 D1 H5 T6" (the first env entry), proving a guest that
        // reads PSP:0x2C and scans the env finds the card exactly as a game would.
        //   mov ax,[0x2c] ; mov ds,ax ; xor dx,dx ; mov cx,24 ; mov bx,1
        //   mov ah,0x40   ; int 0x21  ; mov ax,0x4c00 ; int 0x21
        let com: &[u8] = &[
            0x8b, 0x06, 0x2c, 0x00, 0x8e, 0xd8, 0x31, 0xd2, 0xb9, 0x18, 0x00, 0xbb, 0x01, 0x00,
            0xb4, 0x40, 0xcd, 0x21, 0xb8, 0x00, 0x4c, 0xcd, 0x21,
        ];
        let mut machine =
            Machine::new_dos_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), com)
                .unwrap();
        let reason = machine.run_until_halt_or_cycles(100_000).unwrap();
        assert_eq!(reason, StopReason::DosExit { code: 0 });
        assert_eq!(machine.dos_output(), b"BLASTER=A220 I5 D1 H5 T6");
    }

    #[test]
    fn dos_env_block_carries_the_configured_routing() {
        // A non-default routing (IRQ7 / DMA3) flows from the host config through
        // the loader into the env block a guest scans via PSP:0x2C.
        let mut profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
        profile.sound_blaster = SoundBlasterConfig {
            enabled: true,
            irq: SbIrq::I7,
            dma: SbDma8::D3,
            high_dma: SbDma16::D5,
        };
        let machine = Machine::new_dos_program(profile, &[0xb8, 0x00, 0x4c, 0xcd, 0x21]).unwrap();
        let env_seg = psp_env_segment(&machine);
        assert_ne!(env_seg, 0, "PSP:0x2C must name the env segment");
        assert_eq!(
            parse_env_block(&machine, env_seg),
            vec![
                ("BLASTER".to_string(), "A220 I7 D3 H5 T6".to_string()),
                ("SETSOUND".to_string(), "A220 I7 D3 H5 T6".to_string()),
            ]
        );
    }

    #[test]
    fn dos_child_inherits_the_parent_blaster_environment() {
        // The parent is loaded via new_dos_program, so it has a seeded BLASTER env.
        // It EXECs a child with env_source=0 (inherit). The child reads its own
        // PSP:0x2C, points DS at the inherited env, and writes the first entry,
        // proving BLASTER propagated through EXEC to the child process.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CHILD.COM"), ENV_CHILD_COM).unwrap();
        let mut machine = Machine::new_dos_program(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            ENV_PARENT_COM,
        )
        .unwrap();
        machine.mount_c_drive(izarravm_dos::HostDrive::mount_c(dir.path()).unwrap());
        let reason = machine.run_until_halt_or_cycles(2_000_000).unwrap();
        assert_eq!(reason, StopReason::DosExit { code: 0 });
        assert_eq!(machine.dos_output(), b"BLASTER=A220 I5 D1 H5 T6");
    }

    #[test]
    fn keyboard_rom_echoes_injected_keys_to_the_screen() {
        let profile = MachineProfile::gsw_386(1, izarravm_core::VideoCard::Et4000Ax);
        let mut machine = Machine::new(profile, izarravm_firmware::kbd_bios()).unwrap();
        // Let the ROM run its init (install vectors, unmask IRQ1, STI, enter loop).
        machine.run_until_halt_or_cycles(200_000).unwrap();
        // Inject 'h' then 'i' (Set 1 make+break for H=0x23, I=0x17).
        machine.inject_key_scancodes(&[0x23, 0xa3, 0x17, 0x97]);
        machine.run_until_halt_or_cycles(2_000_000).unwrap();
        let screen = machine.screen_text();
        assert!(
            screen.line_string(0).starts_with("hi"),
            "screen line 0 was {:?}",
            screen.line_string(0)
        );
    }

    #[test]
    fn dos_machine_routes_irq1_to_the_keyboard_isr() {
        // A do-nothing program that just spins (jmp $) so the machine keeps running.
        // org 0x100: jmp $  (EB FE)
        let com: &[u8] = &[0xeb, 0xfe];
        let mut machine =
            Machine::new_dos_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), com)
                .unwrap();
        machine.inject_key_scancodes(&[0x1e, 0x9e]); // 'a' make + break
        machine.run_until_halt_or_cycles(200_000).unwrap();
        // The real INT 09h ISR should have moved 'a' into the BDA ring.
        let head = machine.memory_read_u16_for_test(0x41a);
        let tail = machine.memory_read_u16_for_test(0x41c);
        assert_ne!(head, tail, "ISR enqueued a key into the BDA ring");
    }

    #[test]
    fn dos_program_reads_typed_keys_through_int21() {
        // org 0x100: read two chars with AH=01 (each echoes to stdout), then exit.
        //   mov ah,1 / int 21h / mov ah,1 / int 21h / mov ax,4c00h / int 21h
        let com: &[u8] = &[
            0xb4, 0x01, 0xcd, 0x21, 0xb4, 0x01, 0xcd, 0x21, 0xb8, 0x00, 0x4c, 0xcd, 0x21,
        ];
        let mut machine =
            Machine::new_dos_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), com)
                .unwrap();
        // Type 'h' then 'i' as Set 1 make+break (H=0x23, I=0x17).
        machine.inject_key_scancodes(&[0x23, 0xa3, 0x17, 0x97]);
        let reason = machine.run_until_halt_or_cycles(2_000_000).unwrap();
        assert_eq!(reason, StopReason::DosExit { code: 0 });
        assert_eq!(machine.dos_output(), b"hi");
    }

    #[test]
    fn lotura_reports_id_and_switches_mode_live() {
        // org 0x100: mov al,2; out 0xe1,al; mov ax,4c00h; int 21h
        let com: &[u8] = &[0xb0, 0x02, 0xe6, 0xe1, 0xb8, 0x00, 0x4c, 0xcd, 0x21];
        let mut machine = Machine::new_dos_program(
            MachineProfile::gsw_386(16, izarravm_core::VideoCard::Et4000Ax),
            com,
        )
        .unwrap();
        assert_eq!(machine.active_mode(), GswMode::Gsw386); // boot mode
        let id = with_bus(&mut machine, |bus| {
            bus.read_io(0x00e0, BusWidth::Byte).unwrap() as u8
        });
        assert_eq!(id, LOTURA_ID_VALUE);
        let code = with_bus(&mut machine, |bus| {
            bus.read_io(0x00e1, BusWidth::Byte).unwrap() as u8
        });
        assert_eq!(code, 0);
        // An out-of-range write records no pending switch.
        with_bus(&mut machine, |bus| {
            bus.write_io(0x00e1, BusWidth::Byte, 9).unwrap()
        });
        assert!(machine.pending_mode.is_none());
        assert_eq!(machine.active_mode(), GswMode::Gsw386);
        // Running the program writes 2 to 0xE1; the run loop applies the live switch.
        machine.run_until_halt_or_cycles(100_000).unwrap();
        assert_eq!(machine.active_mode(), GswMode::Gsw586);
        let code = with_bus(&mut machine, |bus| {
            bus.read_io(0x00e1, BusWidth::Byte).unwrap() as u8
        });
        assert_eq!(code, 2);
    }

    #[test]
    fn toka_service_port_formats_drive_and_loads_boot_record() {
        let dir = tempfile::tempdir().unwrap();
        let mut machine = test_machine();
        machine.set_toka_c_root(dir.path().to_path_buf());
        machine.mount_c_drive(izarravm_dos::HostDrive::mount_c(dir.path()).unwrap());

        // A write to Lotura port 0xE3 records the command for the run loop.
        with_bus(&mut machine, |bus| {
            bus.write_io(0x00e3, BusWidth::Byte, 0x02).unwrap();
        });
        assert_eq!(machine.pending_toka_service, Some(0x02));

        // Format installs the Toka-DOS system files onto C:.
        machine.perform_toka_service(0x02);
        assert_eq!(machine.toka_service_status, 0);
        assert!(dir.path().join("ICOMMAND.COM").exists());
        let status = with_bus(&mut machine, |bus| {
            bus.read_io(0x00e3, BusWidth::Byte).unwrap() as u8
        });
        assert_eq!(status, 0);

        // LoadBootRecord places TOKABOOT at 0x7C00 and wires the DOS return path.
        machine.perform_toka_service(0x10);
        assert_eq!(machine.toka_service_status, 0);
        let boot = izarravm_firmware::toka_boot_record().unwrap();
        let placed: Vec<u8> = (0..boot.len())
            .map(|i| machine.read_physical_u8((BOOT_SECTOR_ADDRESS + i) as u32))
            .collect();
        assert_eq!(placed, boot, "boot record sits at 0x7C00");
        // INT 21h now returns through the RAM IRET stub at 0:0x0600.
        assert_eq!(
            machine.memory_read_u16_for_test(0x21 * 4),
            BIOS_IRET_STUB_ADDRESS as u16
        );
        assert_eq!(machine.memory_read_u16_for_test(0x21 * 4 + 2), 0);
        assert_eq!(
            machine.read_physical_u8(BIOS_IRET_STUB_ADDRESS as u32),
            0xcf
        );
    }

    #[test]
    fn toka_dos_boots_through_the_bios_to_the_prompt() {
        let dir = tempfile::tempdir().unwrap();
        // Lay Toka-DOS down on the temp C: the way first boot does.
        let files = izarravm_firmware::toka_dos_system_files();
        izarravm_dos::toka_dos_install(dir.path(), &files, izarravm_dos::InstallMode::Format)
            .unwrap();

        let mut machine = Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            izarravm_firmware::izarra_bios(),
        )
        .unwrap();
        machine.mount_c_drive(izarravm_dos::HostDrive::mount_c(dir.path()).unwrap());
        machine.set_toka_c_root(dir.path().to_path_buf());

        // POST, fall through the (absent) floppy to the disk boot, TOKABOOT, and
        // into ICOMMAND. One clock-second of cycles is ample with fast POST.
        machine.run_until_halt_or_cycles(22_000_000).unwrap();

        let screen = machine.screen_text();
        let text = screen.as_text();
        assert!(
            text.contains("Toka-DOS v3.0"),
            "startup banner on the VGA screen; got:\n{text}"
        );
        assert!(
            text.contains("C:\\>"),
            "ICOMMAND prompt on the VGA screen; got:\n{text}"
        );
    }

    #[test]
    fn toka_md_and_cd_update_the_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let files = izarravm_firmware::toka_dos_system_files();
        izarravm_dos::toka_dos_install(dir.path(), &files, izarravm_dos::InstallMode::Format)
            .unwrap();
        let mut machine = Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            izarravm_firmware::izarra_bios(),
        )
        .unwrap();
        machine.mount_c_drive(izarravm_dos::HostDrive::mount_c(dir.path()).unwrap());
        machine.set_toka_c_root(dir.path().to_path_buf());
        machine.run_until_halt_or_cycles(22_000_000).unwrap();

        // Minimal ASCII to Set 1 for the characters this test types, with Shift
        // for uppercase letters.
        fn key_codes(ch: char) -> Vec<u8> {
            let make: u8 = match ch.to_ascii_lowercase() {
                'm' => 0x32,
                'd' => 0x20,
                's' => 0x1f,
                'u' => 0x16,
                'b' => 0x30,
                'c' => 0x2e,
                ' ' => 0x39,
                '\r' => 0x1c,
                _ => return Vec::new(),
            };
            let mut codes = Vec::new();
            if ch.is_ascii_uppercase() {
                codes.push(0x2a);
            }
            codes.push(make);
            codes.push(make | 0x80);
            if ch.is_ascii_uppercase() {
                codes.push(0xaa);
            }
            codes
        }
        let type_str = |machine: &mut Machine, text: &str| {
            for ch in text.chars() {
                for code in key_codes(ch) {
                    machine.inject_key_scancodes(&[code]);
                }
                machine.run_until_halt_or_cycles(400_000).unwrap();
            }
        };

        type_str(&mut machine, "MD SUB\r");
        type_str(&mut machine, "CD SUB\r");

        assert!(dir.path().join("SUB").is_dir(), "MD created the directory");
        let text = machine.screen_text().as_text();
        assert!(
            text.contains("C:\\SUB>"),
            "the prompt shows the new directory; got:\n{text}"
        );
    }

    #[test]
    fn toka_path_command_shows_the_default_path() {
        let dir = tempfile::tempdir().unwrap();
        let files = izarravm_firmware::toka_dos_system_files();
        izarravm_dos::toka_dos_install(dir.path(), &files, izarravm_dos::InstallMode::Format)
            .unwrap();
        let mut machine = Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            izarravm_firmware::izarra_bios(),
        )
        .unwrap();
        machine.mount_c_drive(izarravm_dos::HostDrive::mount_c(dir.path()).unwrap());
        machine.set_toka_c_root(dir.path().to_path_buf());
        machine.run_until_halt_or_cycles(22_000_000).unwrap();

        // Type "path" (ICOMMAND uppercases the verb) then Enter, lowercase so no
        // Shift is needed.
        fn key_codes(ch: char) -> Vec<u8> {
            let make: u8 = match ch {
                'p' => 0x19,
                'a' => 0x1e,
                't' => 0x14,
                'h' => 0x23,
                '\r' => 0x1c,
                _ => return Vec::new(),
            };
            vec![make, make | 0x80]
        }
        for ch in "path\r".chars() {
            for code in key_codes(ch) {
                machine.inject_key_scancodes(&[code]);
            }
            machine.run_until_halt_or_cycles(400_000).unwrap();
        }

        let text = machine.screen_text().as_text();
        assert!(
            text.contains("C:\\;C:\\DOS"),
            "PATH prints the default search path; got:\n{text}"
        );
    }

    #[test]
    fn toka_runs_a_relocated_tool_from_dos_through_the_path() {
        // The install lays the external tools under C:\DOS; the shell's PATH
        // search must find one by its bare name. MEM lives only in C:\DOS now.
        let dir = tempfile::tempdir().unwrap();
        let files = izarravm_firmware::toka_dos_system_files();
        izarravm_dos::toka_dos_install(dir.path(), &files, izarravm_dos::InstallMode::Format)
            .unwrap();
        assert!(
            dir.path().join("DOS").join("MEM.COM").exists(),
            "MEM installed under C:\\DOS"
        );
        assert!(
            !dir.path().join("MEM.COM").exists(),
            "MEM is not at the root"
        );

        let mut machine = Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            izarravm_firmware::izarra_bios(),
        )
        .unwrap();
        machine.mount_c_drive(izarravm_dos::HostDrive::mount_c(dir.path()).unwrap());
        machine.set_toka_c_root(dir.path().to_path_buf());
        machine.run_until_halt_or_cycles(22_000_000).unwrap();

        fn key_codes(ch: char) -> Vec<u8> {
            let make: u8 = match ch.to_ascii_lowercase() {
                'm' => 0x32,
                'e' => 0x12,
                '\r' => 0x1c,
                _ => return Vec::new(),
            };
            vec![make, make | 0x80]
        }
        for ch in "mem\r".chars() {
            for code in key_codes(ch) {
                machine.inject_key_scancodes(&[code]);
            }
            machine.run_until_halt_or_cycles(400_000).unwrap();
        }

        let text = machine.screen_text().as_text();
        assert!(
            text.contains("conventional memory"),
            "MEM (in C:\\DOS) ran via the PATH search; got:\n{text}"
        );
    }

    #[test]
    fn toka_runs_a_batch_file_with_goto() {
        let dir = tempfile::tempdir().unwrap();
        let files = izarravm_firmware::toka_dos_system_files();
        izarravm_dos::toka_dos_install(dir.path(), &files, izarravm_dos::InstallMode::Format)
            .unwrap();
        // A batch that echoes, jumps over a line with GOTO, and resumes at a label.
        std::fs::write(
            dir.path().join("TEST.BAT"),
            "@ECHO OFF\r\nECHO alpha\r\nGOTO skip\r\nECHO beta\r\n:skip\r\nECHO gamma\r\n",
        )
        .unwrap();

        let mut machine = Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            izarravm_firmware::izarra_bios(),
        )
        .unwrap();
        machine.mount_c_drive(izarravm_dos::HostDrive::mount_c(dir.path()).unwrap());
        machine.set_toka_c_root(dir.path().to_path_buf());
        machine.run_until_halt_or_cycles(22_000_000).unwrap();

        fn key_codes(ch: char) -> Vec<u8> {
            let make: u8 = match ch {
                't' => 0x14,
                'e' => 0x12,
                's' => 0x1f,
                '\r' => 0x1c,
                _ => return Vec::new(),
            };
            vec![make, make | 0x80]
        }
        for ch in "test\r".chars() {
            for code in key_codes(ch) {
                machine.inject_key_scancodes(&[code]);
            }
            machine.run_until_halt_or_cycles(600_000).unwrap();
        }

        let text = machine.screen_text().as_text();
        assert!(text.contains("alpha"), "ECHO ran; got:\n{text}");
        assert!(
            text.contains("gamma"),
            "the label after GOTO ran; got:\n{text}"
        );
        assert!(
            !text.contains("beta"),
            "GOTO skipped the line in between; got:\n{text}"
        );
    }

    /// Set 1 scancodes for an ASCII character (letters, digits, space, dot,
    /// quote, slash, backslash, colon), with Shift for uppercase and quote.
    fn toka_key_codes(ch: char) -> Vec<u8> {
        const LETTER: [u8; 26] = [
            0x1e, 0x30, 0x2e, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31,
            0x18, 0x19, 0x10, 0x13, 0x1f, 0x14, 0x16, 0x2f, 0x11, 0x2d, 0x15, 0x2c,
        ];
        let (make, shift) = match ch {
            'a'..='z' => (LETTER[ch as usize - 'a' as usize], false),
            'A'..='Z' => (LETTER[ch as usize - 'A' as usize], true),
            ' ' => (0x39, false),
            '.' => (0x34, false),
            '\\' => (0x2b, false),
            ':' => (0x27, true),
            '"' => (0x28, true),
            '\r' | '\n' => (0x1c, false),
            _ => return Vec::new(),
        };
        let mut codes = Vec::new();
        if shift {
            codes.push(0x2a);
        }
        codes.push(make);
        codes.push(make | 0x80);
        if shift {
            codes.push(0xaa);
        }
        codes
    }

    #[test]
    fn toka_external_tools_move_and_find() {
        let dir = tempfile::tempdir().unwrap();
        let files = izarravm_firmware::toka_dos_system_files();
        izarravm_dos::toka_dos_install(dir.path(), &files, izarravm_dos::InstallMode::Format)
            .unwrap();
        std::fs::write(
            dir.path().join("POEM.TXT"),
            "roses are red\r\nsky is blue\r\n",
        )
        .unwrap();

        let mut machine = Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            izarravm_firmware::izarra_bios(),
        )
        .unwrap();
        machine.mount_c_drive(izarravm_dos::HostDrive::mount_c(dir.path()).unwrap());
        machine.set_toka_c_root(dir.path().to_path_buf());
        machine.run_until_halt_or_cycles(22_000_000).unwrap();

        let type_line = |machine: &mut Machine, text: &str| {
            for ch in text.chars() {
                for code in toka_key_codes(ch) {
                    machine.inject_key_scancodes(&[code]);
                }
                machine.run_until_halt_or_cycles(400_000).unwrap();
            }
            for code in toka_key_codes('\r') {
                machine.inject_key_scancodes(&[code]);
            }
            machine.run_until_halt_or_cycles(4_000_000).unwrap();
        };

        // MOVE renames the file (checked on the host filesystem).
        type_line(&mut machine, "MOVE POEM.TXT VERSE.TXT");
        assert!(
            dir.path().join("VERSE.TXT").exists(),
            "MOVE created VERSE.TXT"
        );
        assert!(
            !dir.path().join("POEM.TXT").exists(),
            "MOVE removed POEM.TXT"
        );

        // FIND launches via EXEC and prints the matching line on the screen.
        type_line(&mut machine, "FIND \"roses\" VERSE.TXT");
        let text = machine.screen_text().as_text();
        assert!(
            text.contains("roses are red"),
            "FIND printed the matching line; got:\n{text}"
        );
    }

    #[test]
    fn toka_runs_a_system_tool_and_an_alias() {
        let dir = tempfile::tempdir().unwrap();
        let files = izarravm_firmware::toka_dos_system_files();
        izarravm_dos::toka_dos_install(dir.path(), &files, izarravm_dos::InstallMode::Format)
            .unwrap();
        let mut machine = Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            izarravm_firmware::izarra_bios(),
        )
        .unwrap();
        machine.mount_c_drive(izarravm_dos::HostDrive::mount_c(dir.path()).unwrap());
        machine.set_toka_c_root(dir.path().to_path_buf());
        machine.run_until_halt_or_cycles(22_000_000).unwrap();

        let type_line = |machine: &mut Machine, text: &str| {
            for ch in text.chars() {
                for code in toka_key_codes(ch) {
                    machine.inject_key_scancodes(&[code]);
                }
                machine.run_until_halt_or_cycles(400_000).unwrap();
            }
            for code in toka_key_codes('\r') {
                machine.inject_key_scancodes(&[code]);
            }
            machine.run_until_halt_or_cycles(4_000_000).unwrap();
        };

        // BASIC is the install-time alias for IBASIC, so running it proves both
        // the alias file and the EXEC path for a P3 tool.
        type_line(&mut machine, "BASIC");
        let text = machine.screen_text().as_text();
        assert!(
            text.contains("Izarra BASIC"),
            "the BASIC alias ran IBASIC; got:\n{text}"
        );
    }

    #[test]
    fn toka_editor_opens_edits_and_saves_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let files = izarravm_firmware::toka_dos_system_files();
        izarravm_dos::toka_dos_install(dir.path(), &files, izarravm_dos::InstallMode::Format)
            .unwrap();
        std::fs::write(dir.path().join("NOTE.TXT"), "hello\r\n").unwrap();

        let mut machine = Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            izarravm_firmware::izarra_bios(),
        )
        .unwrap();
        machine.mount_c_drive(izarravm_dos::HostDrive::mount_c(dir.path()).unwrap());
        machine.set_toka_c_root(dir.path().to_path_buf());
        machine.run_until_halt_or_cycles(22_000_000).unwrap();

        // Type printable text through the existing US-layout helper.
        let type_text = |machine: &mut Machine, text: &str| {
            for ch in text.chars() {
                for code in toka_key_codes(ch) {
                    machine.inject_key_scancodes(&[code]);
                }
                machine.run_until_halt_or_cycles(400_000).unwrap();
            }
        };
        // Press one key by make scancode (make + break) and let it settle.
        let press = |machine: &mut Machine, make: u8| {
            machine.inject_key_scancodes(&[make, make | 0x80]);
            machine.run_until_halt_or_cycles(800_000).unwrap();
        };

        // Launch the editor on NOTE.TXT and give it time to load and redraw.
        type_text(&mut machine, "EDITOR NOTE.TXT");
        press(&mut machine, 0x1c); // Enter runs the command
        machine.run_until_halt_or_cycles(10_000_000).unwrap();

        let opened = machine.screen_text().as_text();
        assert!(
            opened.contains("hello"),
            "the editor opened the file on screen; got:\n{opened}"
        );

        // Move to end of line with End, then append " world". The editor redraws
        // the whole screen per key, so let it drain the ring before reading back.
        press(&mut machine, 0x4f); // End
        type_text(&mut machine, " world");
        machine.run_until_halt_or_cycles(6_000_000).unwrap();
        let edited = machine.screen_text().as_text();
        assert!(
            edited.contains("hello world"),
            "the typed edit shows on screen; got:\n{edited}"
        );

        // Left arrow moves the cursor back one cell; typing there proves arrow
        // navigation drives the edit point. "hello world" -> "hello worlXd".
        press(&mut machine, 0x4b); // Left
        type_text(&mut machine, "X");
        machine.run_until_halt_or_cycles(3_000_000).unwrap();
        let arrowed = machine.screen_text().as_text();
        assert!(
            arrowed.contains("hello worlXd"),
            "Left arrow positioned the cursor for the insert; got:\n{arrowed}"
        );

        // Ctrl-S saves, then Esc quits (no longer dirty, so it exits at once).
        machine.inject_key_scancodes(&[0x1d, 0x1f, 0x9f, 0x9d]);
        machine.run_until_halt_or_cycles(6_000_000).unwrap();
        press(&mut machine, 0x01); // Esc
        machine.run_until_halt_or_cycles(6_000_000).unwrap();

        let saved = std::fs::read_to_string(dir.path().join("NOTE.TXT")).unwrap();
        assert!(
            saved.contains("hello worlXd"),
            "the file was saved with the edit; got: {saved:?}"
        );
    }

    // --- Izarra 3000 BIOS foundation ---------------------------------------

    #[test]
    fn izarra_bios_post_publishes_result_block() {
        let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
        let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
        let reason = machine.run_until_halt_or_cycles(5_000_000).unwrap();
        // POST completes and the BIOS idles (it keeps running, not halting).
        assert!(matches!(reason, StopReason::CycleLimit { .. }));
        let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();
        // The live result builder owns the header: declared count must match the
        // parsed records and the additive checksum must validate (parse succeeded).
        assert_eq!(
            usize::from(results.declared_record_count),
            results.records.len()
        );
        // The suite opens with a BEGIN record and the foundation reference step.
        assert_eq!(
            results.records[0].status,
            izarravm_firmware::SuiteRecordStatus::Begin
        );
        assert_eq!(results.records[0].name, "suite.izarra");
        assert!(results.records.iter().any(|record| {
            record.status == izarravm_firmware::SuiteRecordStatus::Pass
                && record.name == "self.framework"
        }));
        // self.extaccess proves the unreal-mode >1 MiB helpers work in the live BIOS.
        assert!(results.records.iter().any(|record| {
            record.status == izarravm_firmware::SuiteRecordStatus::Pass
                && record.name == "self.extaccess"
        }));
    }

    #[test]
    fn izarra_bios_draws_graceful_post_screen() {
        // The graceful screen is a white field (DAC index GFX_WHITE = 0) with the
        // red "Izarra 3000" wordmark (index GFX_RED = 4) across the top and a red
        // progress-bar frame lower down. The raster carries DAC indices, not RGB,
        // so the palette remap to white/red does not change the index values here.
        let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
        let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
        machine.run_until_halt_or_cycles(5_000_000).unwrap();
        // The BIOS is idling with the screen drawn; advance the beam to scan a frame.
        machine.advance_devices(600_000);
        let raster = machine.vga_raster().expect("mode 13h presents a VgaRaster");
        assert_eq!(raster.width, 320);
        let w = raster.width as usize;
        // A clear spot (no logo, no text, no bar) is the white field, index 0.
        // Logical y 64 -> physical 128 under the mode-13h double scan.
        assert_eq!(
            raster.pixels[128 * w + 12],
            0,
            "the background field cleared to white (index 0)"
        );
        // The red wordmark sits at logical y 8..29, x 62..257. Mode 13h double-
        // scans, so that band lands at physical rows 16..58. Count index-4 pixels.
        let logo_pixels = (16..58)
            .flat_map(|y| (62..257).map(move |x| (x, y)))
            .filter(|&(x, y)| raster.pixels[y * w + x] == 0x04)
            .count();
        assert!(
            logo_pixels > 200,
            "expected the red Izarra 3000 wordmark, found {logo_pixels} red pixels"
        );
        // The progress-bar frame is red too. Its top edge is logical y 128 ->
        // physical 256, spanning x 32..288. Find red pixels along that row band.
        let bar_pixels = (256..260)
            .flat_map(|y| (32..288).map(move |x| (x, y)))
            .filter(|&(x, y)| raster.pixels[y * w + x] == 0x04)
            .count();
        assert!(
            bar_pixels > 50,
            "expected the red progress-bar frame, found {bar_pixels} red pixels"
        );
    }

    #[test]
    fn izarra_bios_plays_the_power_on_chime() {
        // POST opens with the four-note PC-speaker chime. The note delay is skipped
        // under the default fast POST, but each note still programs PIT channel 2
        // and drives port 0x61 bit 1 high, so the speaker enable latch must be set
        // by the time POST has run.
        let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
        let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
        machine.run_until_halt_or_cycles(5_000_000).unwrap();
        assert!(
            machine.speaker_ever_enabled(),
            "the power-on chime should enable the PC speaker during POST"
        );
    }

    #[test]
    fn serial_tx_is_captured_and_lsr_reports_empty() {
        // A write to the COM1 transmit register (0x3F8) with DLAB clear appends to
        // the text serial_text() surfaces, and the line status register (0x3FD)
        // always reports transmitter empty (THRE|TEMT) so a poll loop never stalls.
        let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
        let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
        with_bus(&mut machine, |bus| {
            bus.write_io(0x03f8, BusWidth::Byte, u32::from(b'H'))
                .unwrap();
            bus.write_io(0x03f8, BusWidth::Byte, u32::from(b'i'))
                .unwrap();
        });
        assert!(machine.serial_text().ends_with("Hi"));
        let lsr = machine.read_io_port_u8(0x03fd);
        assert_ne!(lsr & 0x20, 0, "THRE set");
        assert_ne!(lsr & 0x40, 0, "TEMT set");
    }

    #[test]
    fn izarra_bios_mirrors_post_log_to_com1() {
        // POST initializes COM1 and writes each step's status and name to 0x3F8.
        // After a full POST run the serial log carries the header and the
        // foundation reference step, proving the mirror is live.
        let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
        let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
        machine.run_until_halt_or_cycles(5_000_000).unwrap();
        let serial = machine.serial_text();
        assert!(
            serial.contains("Izarra 3000 POST"),
            "COM1 log missing the POST header: {serial:?}"
        );
        assert!(
            serial.contains("PASS self.framework"),
            "COM1 log missing the framework step line: {serial:?}"
        );
        // MEASURE steps must carry their value: this 16 MB machine reports 16384 KiB
        // detected, so the COM1 line ends with the eight-digit value, not a bare name.
        assert!(
            serial.contains("MEASURE memory.detected_kib 00016384"),
            "COM1 MEASURE line missing its value: {serial:?}"
        );
    }

    #[test]
    fn fast_post_port_reflects_the_flag() {
        // Port 0xE2 is the Lotura POST-pacing flag the BIOS reads before the
        // cosmetic RAM count-up. It defaults to fast (1) so headless runs and
        // tests skip the ~8 s pacing; the GUI clears it for the full experience.
        let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
        let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
        let fast = with_bus(&mut machine, |bus| {
            bus.read_io(0x00e2, BusWidth::Byte).unwrap() as u8
        });
        assert_eq!(fast, 1, "fast POST is the default");
        machine.set_fast_post(false);
        let full = with_bus(&mut machine, |bus| {
            bus.read_io(0x00e2, BusWidth::Byte).unwrap() as u8
        });
        assert_eq!(full, 0, "clearing the flag selects the full-pacing path");
    }

    #[test]
    fn izarra_bios_int19_boots_floppy_sector_zero() {
        // INT 19h must load sector 0 of the mounted floppy to 0000:7C00 and far
        // jump there with no signature check. The boot sector writes a sentinel
        // and halts; if the sentinel lands, the bootstrap loaded and jumped.
        let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
        let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();

        let mut img = vec![0u8; 737_280];
        // Boot sector at 0000:7C00: mov bx,0x0500; mov al,0x99; mov [bx],al; hlt.
        // boot_entry enters with DS=0, so [bx] addresses 0000:0500.
        let boot = [0xBB, 0x00, 0x05, 0xB0, 0x99, 0x88, 0x07, 0xF4];
        img[..boot.len()].copy_from_slice(&boot);
        machine.mount_floppy(img).unwrap();

        machine.run_until_halt_or_cycles(50_000_000).unwrap();
        assert_eq!(
            machine.read_physical_u8(0x0500),
            0x99,
            "the boot sector ran from 0000:7C00, so INT 19h loaded and jumped"
        );
    }

    #[test]
    fn int13_through_ff00_0000_returns_to_caller() {
        // Period PC booters (e.g. Wizardry III) repoint IVT[0x13] to FF00:0000 to
        // chain disk calls through the ROM-BIOS handler, then issue INT 13h. The
        // host intercepts the INT 13h instruction by vector number regardless of
        // the IVT target, so it still services the read; the redirected vector at
        // FF00:0000 only needs a valid IRET to land on. This test proves control
        // returns to the caller (no reset, no runaway) and the disk read happened.
        let mut img = vec![0u8; 737_280];
        img[0] = 0xEB;
        img[1] = 0x55;
        let rom = rom_with_code(&[
            // Point IVT[0x13] (at 0000:004C) to FF00:0000.
            0x31, 0xC0, // xor ax, ax
            0x8E, 0xD8, // mov ds, ax
            0xC7, 0x06, 0x4C, 0x00, 0x00, 0x00, // mov word [0x004C], 0x0000 (offset)
            0xC7, 0x06, 0x4E, 0x00, 0x00, 0xFF, // mov word [0x004E], 0xFF00 (segment)
            // Read 1 sector at CHS(0,0,1) of drive 0 into ES:BX = 0000:2000.
            0x8E, 0xC0, // mov es, ax
            0xBB, 0x00, 0x20, // mov bx, 0x2000
            0xB8, 0x01, 0x02, // mov ax, 0x0201
            0xB9, 0x01, 0x00, // mov cx, 0x0001
            0xBA, 0x00, 0x00, // mov dx, 0x0000
            0xCD, 0x13, // int 13h  -> vector now targets FF00:0000
            // If the IRET at FF00:0000 returned cleanly, we reach this marker.
            0xBB, 0x00, 0x05, // mov bx, 0x0500
            0xB0, 0x42, // mov al, 0x42
            0x88, 0x07, // mov [bx], al   (DS=0, so writes 0000:0500)
            0xF4, // hlt
        ]);
        // The Izarra BIOS emits an IRET at ROM offset 0xF000 (FF00:0000); the
        // synthetic test ROM gets the same stub so the redirected vector lands on
        // a clean return point.
        let mut rom = rom;
        rom[0xF000] = 0xCF; // iret
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
        machine.mount_floppy(img).unwrap();

        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        // The INT 13h read still placed the sector bytes at 0x2000.
        assert_eq!(machine.read_physical_u8(0x2000), 0xEB);
        assert_eq!(machine.read_physical_u8(0x2001), 0x55);
        // The IRET at FF00:0000 returned to the caller, which ran the marker store.
        assert_eq!(
            machine.read_physical_u8(0x0500),
            0x42,
            "control returned past the redirected INT 13h vector"
        );
        let flags = machine.cpu().registers.eflags;
        assert_eq!(flags & 0x0001, 0, "CF must be clear after a good read");
    }

    #[test]
    fn int13_ah01_returns_last_status() {
        // A failed read (drive B:, unbacked) sets the last status; AH=01h reads it back.
        let rom = rom_with_code(&[
            0xB4, 0x02, 0xB0, 0x01, // AH=02h read, AL=1 sector
            0xB5, 0x00, 0xB1, 0x01, // CH=0 cyl, CL=1 sector
            0xB6, 0x00, 0xB2, 0x01, // DH=0 head, DL=1 (drive B:, unbacked)
            0xCD, 0x13, 0xB4, 0x01, 0xCD, 0x13, // AH=01h get last status
            0xF4,
        ]);
        let mut machine =
            Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
        // Mount media in A: so handle_int13 runs; the read targets B:, which is unbacked.
        machine.mount_floppy(vec![0u8; 737_280]).unwrap();
        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
        // Drive B: is unbacked: the transfer reported AH=0x80 (timeout); AH=01h returns it
        // in AH (the documented register) and mirrors it into AL for PS/2 compatibility.
        let ax = machine.cpu().registers.eax() as u16;
        assert_eq!(ax as u8, 0x80, "AL = last disk status");
        assert_eq!((ax >> 8) as u8, 0x80, "AH = last disk status");
    }

    #[test]
    fn izarra_bios_isr_enqueues_injected_key() {
        let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
        let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
        // Run POST so the BIOS reaches its idle loop (past the setup hotkey window,
        // which would otherwise drain the key). Then inject a key: IRQ1 reaches the
        // installed INT 09h, which enqueues it into the BDA ring. The idle loop does
        // not consume keys, so it stays there.
        machine.run_until_halt_or_cycles(5_000_000).unwrap();
        machine.inject_key_scancodes(&[0x1e, 0x9e]);
        machine.run_until_halt_or_cycles(2_000_000).unwrap();
        let head = machine.memory_read_u16_for_test(0x41a);
        let tail = machine.memory_read_u16_for_test(0x41c);
        assert_ne!(head, tail, "the installed INT 09h enqueued the key");
    }

    #[test]
    fn izarra_setup_saves_a_changed_value_to_cmos() {
        // Drive the Del setup page end to end: enter it during POST, change the
        // keyboard layout (CMOS 0x10, default 0 = en-US) to the next entry, save,
        // and confirm the persisted CMOS byte changed. The setup menu blocks on a
        // keyboard read between keystrokes, so each key is injected then run.
        let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
        let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
        assert_eq!(
            machine.cmos_byte(0x10),
            0,
            "the keyboard-layout NVRAM byte starts at en-US (0)"
        );

        // Queue Del before POST reaches the hotkey window so the window finds it.
        // Make + break; only the make enqueues into the BDA ring (0x53 = Del).
        machine.inject_key_scancodes(&[0x53, 0xd3]);
        // Run past POST. The window consumes Del and enters the menu, which then
        // blocks on a keyboard read, so the rest of the budget just spins there.
        machine.run_until_halt_or_cycles(5_000_000).unwrap();

        // Down moves the highlight from Time (row 0) to Keyboard (row 1).
        machine.inject_key_scancodes(&[0x50, 0xd0]); // Down
        machine.run_until_halt_or_cycles(1_000_000).unwrap();
        // Right cycles the keyboard layout forward (en-US -> UK).
        machine.inject_key_scancodes(&[0x4d, 0xcd]); // Right
        machine.run_until_halt_or_cycles(1_000_000).unwrap();
        // F10 saves: writes CMOS 0x10/0x12, refreshes the checksum, and exits.
        machine.inject_key_scancodes(&[0x44, 0xc4]); // F10
        machine.run_until_halt_or_cycles(2_000_000).unwrap();

        assert_eq!(
            machine.cmos_byte(0x10),
            1,
            "saving the setup page persisted the new keyboard layout to CMOS 0x10"
        );
        // The save also refreshes the NVRAM checksum, so a reload validates.
        let saved = machine.cmos_bytes();
        let mut reloaded = Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            izarravm_firmware::izarra_bios(),
        )
        .unwrap();
        assert!(
            reloaded.load_cmos(&saved),
            "the saved CMOS image carries a valid checksum"
        );
        assert_eq!(reloaded.cmos_byte(0x10), 1);
    }

    #[test]
    fn boot_menu_removes_the_old_speed_marker() {
        fn marker_pixels(machine: &Machine, y: u32) -> Vec<u8> {
            (y * 2..(y + 8) * 2)
                .step_by(2)
                .flat_map(|row| machine.video().render_256color_row(row)[296..304].to_vec())
                .collect()
        }

        let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
        let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();

        machine.inject_key_scancodes(&[0x0f, 0x8f]); // Tab opens the boot menu.
        machine.run_until_halt_or_cycles(5_000_000).unwrap();
        assert!(
            marker_pixels(&machine, 80).contains(&1),
            "the initial 386 row has a black diamond"
        );

        for key in [[0x4d, 0xcd], [0x48, 0xc8]] {
            machine.inject_key_scancodes(&key); // Right, Up focuses 586.
            machine.run_until_halt_or_cycles(1_000_000).unwrap();
        }
        machine.inject_key_scancodes(&[0x1c, 0x9c]); // Enter selects 586.
        machine.run_until_halt_or_cycles(5_000_000).unwrap();

        let old_marker = marker_pixels(&machine, 80);
        assert!(
            old_marker.iter().all(|&pixel| pixel == 0),
            "the old 386 diamond is erased: {old_marker:?}"
        );
        assert!(
            marker_pixels(&machine, 48).contains(&0),
            "the focused 586 row has a white diamond"
        );
    }

    #[test]
    fn int1b_vector_points_at_a_valid_iret_handler() {
        // Use a ROM that carries the IRET byte at FF00:0000, the way the real BIOS
        // does, so the seeded vector lands on a genuine IRET.
        let mut m = Machine::new(
            MachineProfile::gsw_386(4, VideoCard::Et4000Ax),
            rom_with_code(&[]),
        )
        .unwrap();
        // IVT[0x1B] is the Ctrl-Break vector: offset word then segment word.
        let off = read_u16(&mut m, 0x1b * 4);
        let seg = read_u16(&mut m, 0x1b * 4 + 2);
        assert_eq!(
            seg, BIOS_ROM_IRET_SEG,
            "INT 1Bh targets the ROM IRET segment"
        );
        let target = (u32::from(seg) << 4) + u32::from(off);
        assert_eq!(
            m.read_physical_u8(target),
            0xcf,
            "INT 1Bh target is an IRET"
        );
    }

    #[test]
    fn int70_vector_points_at_the_rtc_isr_stub() {
        let mut m = int15_machine(4);
        let off = read_u16(&mut m, 0x70 * 4);
        let seg = read_u16(&mut m, 0x70 * 4 + 2);
        assert_eq!(seg, 0);
        assert_eq!(off, BIOS_RTC_ISR_ADDRESS as u16);
        // The stub starts with PUSH AX and ends with IRET.
        assert_eq!(m.read_physical_u8(BIOS_RTC_ISR_ADDRESS as u32), 0x50);
        assert_eq!(m.read_physical_u8(BIOS_RTC_ISR_ADDRESS as u32 + 14), 0xcf);
    }

    #[test]
    fn enabled_rtc_periodic_interrupt_requests_irq8() {
        let mut m = int15_machine(4);
        // Enable the periodic interrupt (select Reg B, set PIE bit 6).
        m.rtc.write_port(0x70, 0x0b);
        m.rtc.write_port(0x71, 0x40);
        // Advance enough clocks for at least one whole RTC second to elapse.
        let one_second = m.active_mode.clock_hz();
        m.advance_devices(one_second + 1);
        assert!(m.pic.irr_bit(8), "IRQ8 became pending");
    }

    #[test]
    fn c207_stores_the_mouse_handler_far_pointer_in_the_ebda() {
        let mut m = int15_machine(4);
        // ES:BX = 1234:5678, the handler the guest installs.
        m.cpu
            .registers
            .set_segment(SegmentIndex::Es, SegmentRegister::real(0x1234));
        m.cpu.registers.set_ebx(0x5678);
        m.cpu.registers.set_eax(0xC207);
        m.handle_int15();
        // CF clear, AH=0: success.
        let flags_carry = {
            let ss = m.cpu.registers.segment(SegmentIndex::Ss).base;
            let sp = m.cpu.registers.esp() as u16;
            read_u16(&mut m, ss + u32::from(sp.wrapping_add(4))) & 1
        };
        assert_eq!(flags_carry, 0, "C207 returns CF clear");
        assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00);
        // The EBDA holds the far pointer: offset word then segment word.
        let base = (u32::from(EBDA_SEGMENT) << 4) + EBDA_MOUSE_HANDLER_OFF;
        assert_eq!(read_u16(&mut m, base), 0x5678, "offset stored");
        assert_eq!(read_u16(&mut m, base + 2), 0x1234, "segment stored");
    }

    #[test]
    fn int19_floppy_boot_loads_sector_and_jumps_to_7c00() {
        let mut m = int15_machine(4);
        // A 360 KB image with a marker byte at the start of sector 0.
        let mut image = vec![0u8; 368_640];
        image[0] = 0xeb; // a plausible boot-sector first byte (JMP short)
        image[1] = 0x3c;
        m.mount_floppy(image).unwrap();
        m.handle_int19();
        // Boot sector copied to 0000:7C00, DL = 0 (floppy), CS:IP = 0000:7C00.
        assert_eq!(m.read_physical_u8(BOOT_SECTOR_ADDRESS as u32), 0xeb);
        assert_eq!(m.read_physical_u8(BOOT_SECTOR_ADDRESS as u32 + 1), 0x3c);
        assert_eq!(m.cpu.registers.edx() as u8, 0x00);
        assert_eq!(m.cpu.registers.segment(SegmentIndex::Cs).selector, 0x0000);
        assert_eq!(m.cpu.registers.eip, BOOT_SECTOR_ADDRESS as u32);
    }

    #[test]
    fn int19_without_bootable_media_falls_to_int18_halt_stub() {
        let mut m = int15_machine(4);
        // No floppy and no Toka-DOS install: INT 19h must reach the INT 18h halt.
        m.handle_int19();
        assert_eq!(
            m.cpu.registers.segment(SegmentIndex::Cs).selector,
            0x0000,
            "CS points at the low-RAM halt stub"
        );
        assert_eq!(m.cpu.registers.eip, BIOS_HALT_STUB_ADDRESS as u32);
        // The stub is CLI;HLT, which halts the machine for good.
        assert_eq!(m.read_physical_u8(BIOS_HALT_STUB_ADDRESS as u32), 0xfa);
        assert_eq!(m.read_physical_u8(BIOS_HALT_STUB_ADDRESS as u32 + 1), 0xf4);
    }

    #[test]
    fn int18_halt_stub_actually_stops_the_machine() {
        let mut m = int15_machine(4);
        m.handle_int18();
        // Run from the halt stub: CLI then HLT, with IF cleared, gives a genuine stop.
        let reason = m.run_until_halt_or_cycles(10_000).unwrap();
        assert_eq!(reason, StopReason::Halted);
    }
}
