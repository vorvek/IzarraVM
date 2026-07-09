pub use fat32::{
    FAT_ATTR_DIRECTORY, FAT32_EOC, Fat32Geometry, Fat32Table, fat32_boot_sector, fat32_dir_entry,
    fat32_dot_entries, fat32_fsinfo_sector, fat32_geometry, fat32_is_eoc,
};
pub use fat32_volume::{Fat32Volume, build_fat32};
use izarravm_audio::{Ad1848, Ad1848Config, OplChip, Resampler, SbDsp, SbMixer};
use izarravm_bus::{
    BusAccessKind, BusError, BusTrace, BusWidth, CpuBus, DirectMemoryRead, DirectMemoryWrite,
    DirectPage, Memory, TracingMode,
};
use izarravm_core::{
    GswMode, HardwareProfile, SoundBlasterConfig, TimingClass, VideoCard, WssConfig,
};
pub use izarravm_cpu::PerfCounters;
use izarravm_cpu::{
    CpuError, CpuGsw, CpuLevel, CycleOutcome, SegmentIndex, SegmentRegister, bus_timing,
};
pub use izarravm_video::MARGO_ID_VALUE;
use izarravm_video::{
    CGA_FB_SIZE, DAC_ENTRIES, DISTIRA_FB_SIZE, DISTIRA_MMIO_SIZE, Distira, HGC_FB_SIZE,
    MARGO_MMIO_SIZE, MARGO_VBE_MODES, MARGO_VRAM_SIZE, Margo, TextFrame, VGA_MODE13H_BASE,
    VGA_MONO_TEXT_BASE, VGA_PLANAR_WINDOW_SIZE, VGA_TEXT_MEMORY_SIZE, VGA_TEXT_PAGE_STRIDE, Vga,
    VgaRaster, VideoMode, bytes_per_pixel, font, pixel_format, vbe_mode,
};
use thiserror::Error;

mod ata;
mod atapi;
mod cdimage;
mod dma;
mod pci;

pub(crate) use pci::PciConfig;
mod cache_config;
mod ram_lookup;
mod timing;
mod video_params;

pub(crate) use ram_lookup::RamPageLookup;
pub(crate) use timing::{DAC_HZ, OPL_NATIVE_HZ, PIT_INPUT_HZ, WSS_AUTOCAL_FALLBACK_HZ};

pub(crate) use cache_config::{
    CACHE_L1_MAX_LINES, CACHE_L2_MAX_LINES, CACHE_TIER_DISABLED_MASK, CacheLevelConfig, TierCost,
    cache_level_config, code_fetch_ws, tier_cost,
};

#[allow(unused_imports)]
pub(crate) use video_params::{
    BDA_VIDEO_SAVE_POINTER, BIOS_FONT_8X8_HIGH_ROM_OFFSET, BIOS_FONT_8X8_ROM_OFFSET,
    BIOS_FONT_8X14_ROM_OFFSET, BIOS_FONT_8X16_ROM_OFFSET, DISTIRA_PCI_BAR_SIZE,
    DISTIRA_PCI_CMDFIFO_OFFSET, DISTIRA_PCI_DEVICE_ID, DISTIRA_PCI_LFB_OFFSET,
    DISTIRA_PCI_REVISION, DISTIRA_PCI_SLOT, DISTIRA_PCI_TEX_OFFSET, DISTIRA_PCI_VENDOR_ID,
    INT10_STATE_BDA_LEN, INT10_STATE_CGA_LATCH_MARKER, INT10_STATE_CGA_LATCH_OFFSET,
    INT10_STATE_DAC_LEN, INT10_STATE_HARDWARE_LEN, INT10_STATIC_FUNCTIONALITY,
    INT10_VIDEO_PARAM_ENTRIES, INT10_VIDEO_PARAM_ENTRY_LEN, INT10_VIDEO_PARAM_TABLE_ENTRIES,
    INT10_VIDEO_PARAM_TABLE_OFFSET, INT10_VIDEO_SAVE_POINTER_TABLE_OFFSET,
    INT10_VIDEO_SAVE_POINTER_TABLE_PTRS, PCI_CONFIG_ADDRESS_PORT, PCI_CONFIG_DATA_END,
    PCI_CONFIG_DATA_PORT, RAM_LOOKUP_PAGE_BITS, RAM_LOOKUP_PAGE_MASK, RAM_LOOKUP_PAGE_SIZE,
    RAM_LOOKUP_SLOW,
};
mod fat32;
mod fat32_volume;
mod fat_name;
mod fdc;
mod floppy;
mod ide;
mod iso9660;
mod katea_names;
mod katea_tree;
mod katea_volume;
mod katea_write;
mod keyboard;
mod lpt;
mod memmap;
mod mouse;
mod pic;
mod pit;
mod raw_program;
mod rtc;
mod speaker;
mod uart;
mod unittester;

pub use cdimage::CdImage;
pub use iso9660::{MAX_IMAGE_BYTES as CD_FOLDER_MAX_BYTES, build as build_cd_folder};
pub use memmap::{
    CONVENTIONAL_TOP, HMA_BASE, HMA_TOP, MemRegion, SYSTEM_ROM_BASE, UPPER_MEMORY_BASE,
    VIDEO_RAM_BASE, classify, is_hma, is_umb_window,
};

/// The video BIOS ROM sits in the first 32 KiB of the upper-memory window on a
/// VGA machine (0xC0000-0xC7FFF), matching where a real adapter's option ROM
/// lives even though this machine does not yet map a BIOS image into that span.
const VGA_BIOS_BASE: u32 = UPPER_MEMORY_BASE; // 0xC0000
const VGA_BIOS_SEGMENT: u16 = (VGA_BIOS_BASE >> 4) as u16; // 0xC000
const VGA_BIOS_INT1D_VIDEO_TABLE_OFF: u16 = 0x1000;
const VGA_BIOS_INT1D_VIDEO_TABLE_ADDR: u32 = VGA_BIOS_BASE + VGA_BIOS_INT1D_VIDEO_TABLE_OFF as u32;
const VGA_BIOS_FONT_TABLE_OFF: u16 = 0x2000;
const VGA_BIOS_INT43_FONT_ADDR: u32 = VGA_BIOS_BASE + VGA_BIOS_FONT_TABLE_OFF as u32;
const VGA_BIOS_INT44_FONT_OFF: u16 = 0x3000;
const VGA_BIOS_INT44_FONT_ADDR: u32 = VGA_BIOS_BASE + VGA_BIOS_INT44_FONT_OFF as u32;
const VGA_BIOS_INT1F_FONT_OFF: u16 = 0x3800;
const VGA_BIOS_INT1F_FONT_ADDR: u32 = VGA_BIOS_BASE + VGA_BIOS_INT1F_FONT_OFF as u32;
/// Lotura port 0xE7 banks one code-page font page here. The address sits in
/// free space inside the VGA BIOS span (0xC0000-0xC7FFF); the VGA BIOS only
/// uses through ~0xC3C00, so 0xC4000 is available without a new UMA reservation.
const CODEPAGE_FONT_WINDOW: u32 = 0xC4000;
/// Size of the video option ROM span this machine backs with flat RAM
/// (INT 10h/1D/43/44/1F tables plus the code-page font bank): 32 KiB, matching
/// where a real VGA adapter's option ROM lives. One past this, at 0xC8000, is
/// the first byte of open, unoccupied upper memory.
const VGA_BIOS_SPAN_SIZE: u32 = 0x8000;

pub const HIGH_ROM_BASE: u32 = 0xffff_0000;
pub const MARGO_LFB_BASE: u32 = 0xE000_0000;
pub const MARGO_MMIO_BASE: u32 = 0xE040_0000;
pub const DISTIRA_MMIO_BASE: u32 = 0xE100_0000;
pub const DISTIRA_LFB_BASE: u32 = 0xE140_0000;

pub const LOW_BIOS_BASE: u32 = 0x000f_0000;
pub const BIOS_ROM_SIZE: usize = 64 * 1024;
const BIOS_ROM_SEGMENT: u16 = (LOW_BIOS_BASE >> 4) as u16;

pub const BOOT_IMAGE_SIZE: usize = 1440 * 1024;
pub const BOOT_SECTOR_ADDRESS: usize = 0x7c00;
pub const BOOT_STAGE2_ADDRESS: usize = 0x8000;
pub const BIOS_IRET_STUB_ADDRESS: usize = 0x0600;
pub const RESULT_BLOCK_ADDRESS: usize = 0x9000;
/// Fixed load segment for a .COM: PSP at linear 0x2000, clear of the IVT, BIOS
/// data area, BIOS RAM stubs, and the worst-case Toka-DOS SysVars/SDA layout.
const DOS_LOAD_SEGMENT: u16 = 0x0200;

/// Lotura system-controller identifier, mirroring the Margo card's MARGO_ID_VALUE
/// convention (a fixed nonzero byte the guest can probe).
pub const LOTURA_ID_VALUE: u8 = 0x5a;

/// Default drive number the IZCDEX HLE exposes the CD-ROM at (0 = A:). With no
/// CONFIG.SYS block drivers, the CD is D:, after A: floppy and C: host drive.
///
/// IZCDEX = Izarra CD-ROM Extensions, the Toka-DOS CD redirector. Its INT 2Fh
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
    Program(#[from] raw_program::ProgramLoadError),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveDisplay {
    VgaRaster,
    MargoLfb,
    Distira,
}

/// Execution quantum a wall-pacing caller grants the CPU after
/// `advance_wall_shortfall` stops at a VGA vertical-retrace start edge, so a
/// guest polling port 0x3DA observes the window before the top-up resumes.
///
/// Sizing: one 0x3DA poll iteration (IN + TEST + Jcc) costs roughly 3-30 clocks
/// across the Approximate modes, so 2000 clocks is 60-600 poll iterations,
/// ample to see bit 3 set. It is also negligible against a frame: 0.2 percent
/// of the ~941k clocks per 14.4 ms mode-13h frame at 66 MHz (486), less at 586,
/// and smaller than the ~4200-clock retrace window itself at 486, so the beam
/// is still inside the window for the whole peek. At most ~70 edge-stops per
/// wall second, so a guest that never polls pays ~140k clocks/s (~0.2 percent).
pub const VRETRACE_PEEK_CLOCKS: u64 = 2_000;

/// Per-clock conversion factors, recomputed once whenever the active mode (clock)
/// changes, so the per-instruction device pacing multiplies instead of dividing.
#[derive(Debug, Clone, Copy)]
struct TimingFactors {
    micros_per_clock: f64,   // 1e6 / clock_hz (OPL and DSP settle)
    pit_per_clock: f64,      // PIT_INPUT_HZ / clock_hz
    margo_ns_per_clock: f64, // 1e9 / clock_hz
    inv_clock: f64,          // 1 / clock_hz (DSP sample phase)
    // CPU clocks in one 44.1 kHz DAC sample. The run loop batches instructions
    // up to this many clocks before servicing devices once, so the per-clock
    // fine-samplers (the DSP/CD producers step at the DAC rate) still see at
    // most one sample of time per call and never alias. >=1 in every mode
    // (clock_hz >> 44100).
    clocks_per_audio_sample: u64,
}

impl TimingFactors {
    fn for_clock(clock_hz: u64) -> Self {
        let c = clock_hz as f64;
        Self {
            micros_per_clock: 1_000_000.0 / c,
            pit_per_clock: PIT_INPUT_HZ as f64 / c,
            margo_ns_per_clock: 1_000_000_000.0 / c,
            inv_clock: 1.0 / c,
            clocks_per_audio_sample: (clock_hz / u64::from(DAC_HZ)).max(1),
        }
    }
}

/// Bytes per modeled cache line: 64 bytes on every tier. (A real Pentium MMX uses
/// 32-byte lines; the line size is kept as-is -- out of scope for the P55C timing
/// retarget, which changed clock / L1 size / dials, not the line geometry.)
/// Per-mode VIDEO-window wait-states for the Approximate class (486/586), the
/// FOURTH timing lever, calibrated (2026-07-05, retuned 2026-07-06) against the
/// owner's real-hardware Doom `-timedemo demo3` fps targets (486 DX2-66 max
/// detail 29-30 fps, P55C-200 ~82 fps; cross-checked vs the Ertl doombench
/// archive). Why it exists: a real VGA card sits across an expansion bus whose
/// per-access latency does NOT scale with CPU speed, but the flat
/// `WaitStateProfile.video = 1` rode `scale_bus` (486 x1/3, 586 x7/30), pricing a
/// VRAM byte write at ~15 ns / ~3.5 ns where real VLB / PCI writes cost
/// ~100-450 ns. Doom is framebuffer-bound (measured: ~61,500 VRAM data accesses
/// per frame at max detail), so the 486/586 personas ran demo3 1.27x / 1.56x too
/// fast while every synthetic bench (no VRAM traffic) sat era-exact. These values
/// are calibrated POST-`scale_bus`: the charged clocks are `(2 + ws) *
/// bus_num/bus_den`.
///
/// 586 retune (2026-07-06): the narrow-SMC fix (PR #431) lifted Doom demo3 from
/// 907 to 773 realtics (96.6 fps), faster than the ~82 fps P55C target. The owner
/// kept the SMC win and retuned this dial to restore era-apparent speed. ws=88 ->
/// 913 realtics -> 81.8 fps (was ws=62 -> 773 realtics -> 96.6 fps). This is the
/// Doom-isolated lever: a sweep confirmed the synthetic bench cyc/iter columns
/// (sieve 120503.05, dhrystone 663.62, all four modes) are byte-identical across
/// the sweep, because the benches do no VRAM traffic. The dial is not perfectly
/// Quake-decoupled (Quake's software renderer does enough VRAM traffic to feel
/// it): measured nosound demo1 went 42.4 fps (ws=62) -> 41.5 fps (ws=88), a -2.1%
/// shift vs Doom's -15.3%, so Doom is ~16x more sensitive. The two era targets
/// (Doom ~82, Quake ~43) are not simultaneously reachable with this single dial;
/// Doom is the priority (it was 18% too fast; Quake shifted 2%). x87 timing work,
/// which shifts Quake more than Doom, is deferred to a later round.
///
/// The shipped 486 value stays at the flat 1 (see the arm comment: with honest
/// tick delivery the DX2-66 persona hits its target with no surcharge, so no
/// VLB-class value ships). If `bus_timing` is ever retuned, recalibrate these with
/// it. The Accurate class (286/386) keeps the frozen `WaitStateProfile.video`
/// path bit-for-bit (byte-identity gate).
const fn video_wait_states_approx(level: CpuLevel) -> u8 {
    match level {
        // Unreachable in practice (Accurate class takes the profile path), but
        // keep the frozen classes on the profile default should routing change.
        CpuLevel::I286 | CpuLevel::I386 => 1,
        // The 486 keeps the flat profile value: once the batch cap counts bus
        // clocks (no more coalesced IRQ0 ticks), the DX2-66 persona lands the
        // owner's 29-30 fps demo3 target with NO video surcharge - its
        // Dhrystone-pinned bus dial already prices every access fat enough that
        // the real VLB video cost is absorbed. Charging the physical ~130 ns on
        // top would undershoot the target (~27 fps). Composition infidelity
        // accepted and recorded: the 486's Doom time leans more on ordinary bus
        // than a real DX2-66's (which leans on the video bus); the NET frame
        // rate is what is calibrated. Revisit alongside any bus_timing retune.
        CpuLevel::I486 => 1,
        // Retuned 2026-07-06: ws=88 -> 913 realtics -> 81.8 fps (era target ~82).
        // See the function-level doc for the sweep data and the isolation proof.
        CpuLevel::I586 => 88,
    }
}

/// Which modeled cache tier a data access resolves to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tier {
    L1,
    L2,
    Ram,
}

/// Cosmetic multi-tier cache. It only sets the guest-perceived TIMING of a memory
/// access; the host always reads real memory. There is no real data cache, no
/// associativity beyond direct-mapped tag matching, no replacement policy, and no
/// coherence (MESI). Two direct-mapped tag arrays (L1, L2) each sized to the
/// LARGEST geometry across modes; the live level's geometry picks how many of those
/// lines are in play via a power-of-two mask. The full line number is stored as the
/// tag so a smaller per-mode mask still disambiguates aliased lines.
///
/// Wired into `read_memory`/`write_memory`: every data access warms the modeled
/// cache, and the resolved tier already DRIVES the charged data-access cost via
/// `data_wait_states` -> `tier_cost` (L1/L2/RAM each have their own calibrated
/// per-mode wait-state).
#[derive(Debug)]
struct CacheModel {
    l1_tags: Box<[u32]>, // sized to the largest L1 line count (586: 512)
    l2_tags: Box<[u32]>, // sized to the largest L2 line count (586: 8192)
    config: CacheLevelConfig,
    cost: TierCost,
    code_fetch_ws: u8,
    lookups: u64,
}

/// Sentinel tag that cannot be a valid line number, so a freshly filled array is
/// all-miss. `line = phys >> 6`; with a 32-bit phys the top line number is
/// `0xFFFF_FFFF >> 6 < u32::MAX`, so `u32::MAX` is never a real tag.
const CACHE_EMPTY_TAG: u32 = u32::MAX;

impl CacheModel {
    fn new(level: CpuLevel) -> Self {
        Self {
            l1_tags: vec![CACHE_EMPTY_TAG; CACHE_L1_MAX_LINES].into_boxed_slice(),
            l2_tags: vec![CACHE_EMPTY_TAG; CACHE_L2_MAX_LINES].into_boxed_slice(),
            config: cache_level_config(level),
            cost: tier_cost(level),
            code_fetch_ws: code_fetch_ws(level),
            lookups: 0,
        }
    }

    fn set_level(&mut self, level: CpuLevel) {
        self.config = cache_level_config(level);
        self.cost = tier_cost(level);
        self.code_fetch_ws = code_fetch_ws(level);
        self.reset();
    }

    /// Resolve a DATA access at `phys` to a tier for the live `level`, installing the
    /// line into the cheaper tiers on a miss (modeling an inclusive fill). A 0-size
    /// tier is skipped: the 286 has neither tier (always RAM); the 386 has no L1.
    ///
    #[cfg(test)]
    fn data_tier(&mut self, level: CpuLevel, phys: u32) -> Tier {
        self.data_tier_with_config(cache_level_config(level), phys)
    }

    #[inline(always)]
    fn data_tier_with_config(&mut self, config: CacheLevelConfig, phys: u32) -> Tier {
        self.lookups += 1;
        let line = phys >> 6;
        if config.l1_mask != CACHE_TIER_DISABLED_MASK {
            let idx = (line & config.l1_mask) as usize;
            if self.l1_tags[idx] == line {
                return Tier::L1;
            }
        }
        if config.l2_mask != CACHE_TIER_DISABLED_MASK {
            let idx = (line & config.l2_mask) as usize;
            if self.l2_tags[idx] == line {
                // L2 hit: pull the line up into L1 (if L1 exists) for next time.
                self.install_l1(config, line);
                return Tier::L2;
            }
        }
        // Miss in both: serve from RAM and fill the existing tiers.
        self.install_l1(config, line);
        self.install_l2(config, line);
        Tier::Ram
    }

    #[inline(always)]
    fn install_l1(&mut self, config: CacheLevelConfig, line: u32) {
        if config.l1_mask != CACHE_TIER_DISABLED_MASK {
            self.l1_tags[(line & config.l1_mask) as usize] = line;
        }
    }

    #[inline(always)]
    fn install_l2(&mut self, config: CacheLevelConfig, line: u32) {
        if config.l2_mask != CACHE_TIER_DISABLED_MASK {
            self.l2_tags[(line & config.l2_mask) as usize] = line;
        }
    }

    /// Wait-states to charge for a DATA access at `phys`. Warms the modeled cache
    /// (so tier state is live) and returns the per-tier cost for the resolved tier.
    /// `_width` is accepted for a future wide-access straddle model but does not
    /// affect the current single-line model. The RAM cost is `tier_cost(level).ram`,
    /// not the device `memory_wait_states`: the device-window gate in
    /// `data_access_wait_states` already routed ROM/MMIO accesses to
    /// `memory_wait_states` before reaching here, so this only ever sees cacheable
    /// RAM.
    #[inline(always)]
    fn data_wait_states(&mut self, phys: u32, _width: BusWidth) -> u8 {
        match self.data_tier_with_config(self.config, phys) {
            Tier::L1 => self.cost.l1,
            Tier::L2 => self.cost.l2,
            Tier::Ram => self.cost.ram,
        }
    }

    /// Wait-states for a code fetch: code is assumed L1-resident, so this is a
    /// per-mode constant with no tag check. Routed through here by the bus
    /// `code_fetch_wait_states` for cacheable RAM (ROM/device code keeps
    /// `memory_wait_states`). DECOUPLED from the data L1 wait-state (`tier_cost.l1`):
    /// the data L1 cost is sized for the bandwidth-l1 MB/s band, but a CPU's I-cache
    /// fetch is pipelined and far cheaper per byte, so charging the data L1 cost on
    /// every fetched byte would crush the compute benchmarks (Dhrystone, fp-mandel)
    /// well below their bands. The code-fetch constant is its own per-mode dial.
    fn code_fetch_wait_states(&self) -> u8 {
        self.code_fetch_ws
    }

    /// Drop all cached lines (mode change). Both arrays go back to the sentinel so
    /// the next access to any line is a cold miss.
    fn reset(&mut self) {
        self.l1_tags.fill(CACHE_EMPTY_TAG);
        self.l2_tags.fill(CACHE_EMPTY_TAG);
    }

    fn lookups(&self) -> u64 {
        self.lookups
    }
}

/// The result of a host-driven memory-bandwidth pass: the total bytes moved and
/// the bus clocks they took. The caller turns this into MB/s with the live mode's
/// clock: `bytes / (clocks / clock_hz)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BandwidthSample {
    pub bytes: u64,
    pub clocks: u64,
}

const MACHINE_PROFILE_PHASES: usize = 6;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MachineProfilePhase {
    pub name: &'static str,
    pub wall_ns: u64,
    pub count: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MachineHostProfileSnapshot {
    pub phases: Vec<MachineProfilePhase>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MachineProfilePhaseKind {
    CpuBatch,
    AdvanceDevices,
    SoftInt,
    ConsoleFlush,
    HaltFastForward,
    CdStall,
}

impl MachineProfilePhaseKind {
    const ALL: [Self; MACHINE_PROFILE_PHASES] = [
        Self::CpuBatch,
        Self::AdvanceDevices,
        Self::SoftInt,
        Self::ConsoleFlush,
        Self::HaltFastForward,
        Self::CdStall,
    ];

    const fn index(self) -> usize {
        match self {
            Self::CpuBatch => 0,
            Self::AdvanceDevices => 1,
            Self::SoftInt => 2,
            Self::ConsoleFlush => 3,
            Self::HaltFastForward => 4,
            Self::CdStall => 5,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::CpuBatch => "cpu_batch",
            Self::AdvanceDevices => "advance_devices",
            Self::SoftInt => "soft_int",
            Self::ConsoleFlush => "console_flush",
            Self::HaltFastForward => "halt_fast_forward",
            Self::CdStall => "cd_stall",
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct MachineProfilePhaseState {
    wall_ns: u64,
    count: u64,
}

#[derive(Clone)]
struct MachineHostProfile {
    enabled: bool,
    phases: [MachineProfilePhaseState; MACHINE_PROFILE_PHASES],
}

impl Default for MachineHostProfile {
    fn default() -> Self {
        Self {
            enabled: false,
            phases: [MachineProfilePhaseState::default(); MACHINE_PROFILE_PHASES],
        }
    }
}

impl PartialEq for MachineHostProfile {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for MachineHostProfile {}

impl std::fmt::Debug for MachineHostProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MachineHostProfile")
    }
}

impl MachineHostProfile {
    fn enable(&mut self) {
        *self = Self {
            enabled: true,
            phases: [MachineProfilePhaseState::default(); MACHINE_PROFILE_PHASES],
        };
    }

    fn disable(&mut self) {
        *self = Self::default();
    }

    #[inline]
    fn start(&self) -> Option<std::time::Instant> {
        self.enabled.then(std::time::Instant::now)
    }

    #[inline]
    fn record(&mut self, phase: MachineProfilePhaseKind, start: Option<std::time::Instant>) {
        let Some(start) = start else {
            return;
        };
        let bucket = &mut self.phases[phase.index()];
        bucket.count += 1;
        bucket.wall_ns = bucket
            .wall_ns
            .saturating_add(duration_ns_u64(start.elapsed()));
    }

    fn snapshot(&self) -> MachineHostProfileSnapshot {
        MachineHostProfileSnapshot {
            phases: MachineProfilePhaseKind::ALL
                .iter()
                .map(|&phase| {
                    let bucket = self.phases[phase.index()];
                    MachineProfilePhase {
                        name: phase.name(),
                        wall_ns: bucket.wall_ns,
                        count: bucket.count,
                    }
                })
                .collect(),
        }
    }
}

fn duration_ns_u64(duration: std::time::Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}

/// Gate for the opt-in per-fault / diagnostic-port trace (see `Machine::log_fault_trace`
/// and the unit-tester `CMD_EXIT` trace in `perform_unittester`). Default off: checked
/// only on the cold paths (a fatal CPU error, or a guest OUT to the unit-tester exit
/// port), never per-instruction or per-cycle, so leaving it unset costs one env lookup
/// on those rare events and nothing anywhere else.
fn fault_trace_enabled() -> bool {
    std::env::var_os("IZARRAVM_FAULT_TRACE").is_some()
}

#[derive(Debug)]
pub struct Machine {
    profile: MachineProfile,
    active_mode: GswMode,
    pending_mode: Option<GswMode>,
    timing: TimingFactors,
    cpu: CpuGsw,
    // Per-mode cache model. A data access warms its tag state and the resolved tier
    // drives the charged wait-state (its per-mode tier costs are calibrated). Reset
    // on a mode switch (its contents are per-mode).
    cache_model: CacheModel,
    // Fractional-remainder carry for the per-mode bus-clock scaler (B-T10). The bus
    // portion of a step (fetch + tiered data access) is scaled by `bus_timing(level)`
    // num/den in `scale_bus`; this holds the leftover so a cheap access in a fast
    // mode is not rounded to zero (mirrors the CPU's `timing_rem` for instruction
    // clocks). Reset on a mode switch (the per-mode ratio changes).
    bus_rem: u64,
    memory: Memory,
    ram_lookup: RamPageLookup,
    // Boxed: Vga is ~99 KB. Inline, the Machine value (and its Result wrapper)
    // got copied through the constructors enough times in debug builds to
    // overflow the main-thread stack before the binary did any work. On the heap
    // it costs one pointer and the copies stay cheap.
    video: Box<Vga>,
    paradise_non_vga: bool,
    paradise_regs: [u8; 6],
    margo: Margo,
    distira: Distira,
    pci: PciConfig,
    margo_active: bool,
    text_scanline_override: Option<u16>,
    pending_soft_int: Option<u8>, // software-INT vector awaiting deferred dispatch
    // The vector of the last host-intercepted `INT n` opcode, stashed so the
    // legacy shared FF00:0000 chain target can attribute a landing there (that
    // address is shared by every vector, so the fetch seam cannot key it the
    // way it keys the per-vector stub table). Consumed by `note_stub_fetch`.
    last_int_vector: Option<u8>,
    // Set by MachineBus on any port I/O; the run loop's instruction batch reads
    // it to know when to stop and service devices (see run_until_clock). A field
    // rather than a loop local so make_bus's one-off host accesses share it.
    io_touched: bool,
    // Fixed ISA-bus time (in CPU clocks) accrued this batch by the OPL status poll,
    // added to the batch's device advance in the Approximate class so a fast-CPU
    // poll cannot outrun the 80 us OPL timer. See the batch-end use in
    // run_until_clock and the accrual in read_io. Consumed (zeroed) each batch via
    // mem::take.
    isa_io_batch_clocks: u64,
    // Set by MachineBus when a device (DMA disk/floppy transfer, DMA block copy) writes guest RAM,
    // which bypasses the CPU's self-modifying-code tracking. The run loop tells the CPU to drop its
    // prefetch + decode cache at end of step so staged code is never replayed stale.
    device_wrote_memory: bool,
    // Set when the RAM direct-map table changes, so cached host pointers in the CPU are dropped
    // before any later guest access can use a stale RAM page classification.
    direct_map_changed: bool,
    host_profile: MachineHostProfile,
    // Toka-DOS service (Lotura port 0xE3): a write records the command here, the
    // run loop performs it after the cycle (it needs &mut self for host I/O), and
    // the resulting status is read back at 0xE3.
    pending_toka_service: Option<u8>,
    toka_service_status: u8,
    /// The host folder backing the Katea C: drive (set by `mount_hdd_folder`), so
    /// the BIOS "Repair Toka-DOS" service can reset CONFIG.SYS/AUTOEXEC.BAT on it.
    katea_root: Option<std::path::PathBuf>,
    // How many bytes of the DOS console output have already been teletyped onto
    // the VGA text screen. DOS CON output goes to the kernel's stdout buffer; the
    // machine mirrors the new bytes onto the framebuffer so the screen shows them.
    dos_screen_shown: usize,
    /// True only for a `new_raw_program` machine: routes INT 20h/21h/27h to
    /// `handle_raw_program_int`. See
    /// `dev_docs/2026-06-30-katea-sp3-program-runtime-design.md` section 3a.
    program_runtime: bool,
    /// Accumulated console output for a `new_raw_program` machine, read back
    /// through `program_output()` / seeded through `set_program_stdin()`.
    program_output: Vec<u8>,
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
    speaker_transitions: Vec<pit::OutTransition>,
    dma: dma::DmaController,
    // 8272A floppy disk controller (ports 0x3F0-0x3F7). A guest that programs the
    // FDC directly drives it here; the INT 13h path stays HLE and does not use it.
    // READ/WRITE DATA move sector bytes over DMA channel 2 against `floppy`.
    fdc: fdc::Fdc,
    opl: OplChip,
    resampler: Resampler,
    /// ReSonique 2 analog output-stage gain (host-tunable "amp gain"), applied in
    /// render_audio to the card's sources but not the PC speaker. 1.0 = unity (the
    /// default; the GUI sets it from the config). Host-side loudness only, not
    /// guest-visible, so it never affects timing or the guest audio model.
    card_amp: f32,
    /// PC speaker output volume (host-tunable), a linear attenuation applied in
    /// render_audio to the speaker only. 1.0 = full (default), 0.0 = muted. Like
    /// card_amp it is host-side loudness only, never guest-visible.
    speaker_volume: f32,
    opl_micros: f64, // fractional microseconds owed to the OPL timers
    dsp: SbDsp,
    /// DSP PCM resampler (rate_hz -> 44100), rebuilt when the programmed rate
    /// changes. Summed with the OPL stream in render_audio.
    dsp_resampler: Resampler,
    dsp_rate_hz: u32, // input rate the dsp_resampler is currently configured for
    dsp_micros: f64,  // fractional microseconds owed to the DSP reset-settle clock
    dsp_sample_phase: f64, // fractional DSP samples owed to the DMA playback clock
    last_audio_clocks: u64, // for HLE guest-time driven sample counts in render (Phase 4)
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
    cd_sample_phase: f64, // fractional CD Red Book samples (44.1 kHz) owed to guest time for HLE timing (Phase 4)
    wss_base: u16,        // I/O base of the 4-port config region (codec sits at base+4)
    wss_irq: u8,          // PIC line the codec's terminal-count interrupt forwards to
    wss_dma: usize,       // byte-wide DMA channel the codec pulls playback bytes from
    wss_enabled: bool,    // false drops all WSS work (port decode, tick, IRQ, render)
    margo_ns: f64,        // fractional nanoseconds owed to the Margo busy countdown
    vga_dots: f64,        // fractional VGA dot clocks owed to the beam advance
    trace: BusTrace,
    elapsed_clocks: u64,
    // Of elapsed_clocks, the clocks consumed by device I/O stalls (floppy seek/
    // read, later ATA) rather than executed instructions. A realtime host can
    // subtract these so blocking on a drive does not read as running over 100%.
    io_stall_clocks: u64,
    // INT 2Fh AH=13h DOS disk-driver hook state: DS:DX live handler and ES:BX
    // restore target. Initial value is the seeded BIOS INT 13h IRET vector.
    dos_disk_handler: (u16, u16),
    dos_disk_restore: (u16, u16),
    // INT 2Fh AX=B803h/B804h network post address. Null when no network TSR has
    // published one.
    network_post_address: (u16, u16),
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
    // MSCDEX/IZCDEX volume-descriptor preference. The default selects the primary
    // volume descriptor.
    icdex_vd_preference: u16,
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
    // Booter-inert mode: when set, the Toka-DOS HLE and IZEMM stand down so a
    // self-booting disk owns the DOS/memory-manager interrupts through the IVT
    // (the BIOS services stay intercepted). INT 19h sets it by boot source: a
    // floppy boot turns it on (the disk's sector-0 code is the OS), a C: Toka-DOS
    // boot turns it off (the HLE is the OS), re-evaluated on every boot so a warm
    // reboot flips it. It starts off and stays off until the first boot.
    booter_inert: bool,
    // Last absolute pointer the GUI reported, in the guest virtual-screen space
    // (0..639 x 0..199). set_mouse_absolute diffs against this to synthesize the
    // relative deltas the aux device and the guest driver expect. Mutated only on
    // the emulation thread, so it needs no synchronization.
    last_abs: (i32, i32),
    // Guest-visible regression-test device (Lotura ports 0xE4-0xE6). A command
    // write records the request here; the run loop performs it after the cycle
    // (it needs &mut self for the framebuffer, host I/O, and the stop).
    unittester: unittester::UnitTester,
    // Where the unit tester's Snapshot command writes PPM frames, set by the
    // host. None disables snapshots (the command becomes a no-op).
    test_snapshot_path: Option<std::path::PathBuf>,
    // Test-only observation seam for the batch loop's prior_runs_core_clocks
    // updates (P4a Slice 1 Task 1.2 review finding 1). One inner Vec per batch
    // (pushed at batch entry); each element is the value the loop pushed into
    // `MachineBus::prior_runs_core_clocks` before one `run_straight_line` call.
    // Compiled out of release builds entirely, so the hot loop pays nothing.
    #[cfg(test)]
    test_prior_core_pushes: Vec<Vec<u64>>,
    // Test-only: the final batch core total (`outcome.core_clocks`, the core
    // component of the batch-end `step`) per batch, parallel to
    // `test_prior_core_pushes`. Lets the pin test check every per-run push stayed
    // within the total that later fed advance_devices.
    #[cfg(test)]
    test_batch_core_totals: Vec<u64>,
}

/// The GUI virtual pointer space the relative-delta synthesis spans: x 0..639,
/// y 0..199, matching the GUI's own MOUSE_GUEST_MAX_X/Y. The center is where a
/// fresh capture seeds last_abs so the first delta is not a large jump.
const MOUSE_GUEST_MAX_X: i32 = 639;
const MOUSE_GUEST_MAX_Y: i32 = 199;
const MOUSE_GUEST_CENTER_X: i32 = MOUSE_GUEST_MAX_X / 2;
const MOUSE_GUEST_CENTER_Y: i32 = MOUSE_GUEST_MAX_Y / 2;

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

/// Files that are NOT overlaid in user-folder mode: the demo file and the two
/// config files the user owns on C:.
const USER_OWNED_OR_DEMO: &[&str] = &["HELLO.TXT", "CONFIG.SYS", "AUTOEXEC.BAT"];

/// The payload files overlaid in user-folder mode: the binaries (KERNEL.SYS,
/// COMMAND.COM, LICENSE.TXT, TOKAMOUS.COM, TOKAEMM.SYS) but not the demo file
/// or the user's CONFIG.SYS/AUTOEXEC.BAT.
fn user_folder_overlay(files: Vec<(String, Vec<u8>)>) -> Vec<(String, Vec<u8>)> {
    files
        .into_iter()
        .filter(|(name, _)| {
            !USER_OWNED_OR_DEMO
                .iter()
                .any(|d| name.eq_ignore_ascii_case(d))
        })
        .collect()
}

/// Seed `CONFIG.SYS`/`AUTOEXEC.BAT` into a host folder if they are absent, so the
/// user always has real, editable copies. Existing files are left untouched (the
/// user owns them). Case-insensitive on Windows, the supported host.
fn ensure_user_config(
    dir: &std::path::Path,
    config: &[u8],
    autoexec: &[u8],
) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    if !dir.join("CONFIG.SYS").exists() {
        std::fs::write(dir.join("CONFIG.SYS"), config)?;
    }
    if !dir.join("AUTOEXEC.BAT").exists() {
        std::fs::write(dir.join("AUTOEXEC.BAT"), autoexec)?;
    }
    Ok(())
}

/// A payload file's bytes by 8.3 name. Panics if absent: the image is a committed
/// compile-time binary, so a missing system file is a build defect, not a runtime
/// condition (matches `extract_system_payload`'s panic-on-corrupt style).
fn payload_file(payload: &katea_volume::SystemPayload, name: &str) -> Vec<u8> {
    payload
        .files
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, b)| b.clone())
        .unwrap_or_else(|| panic!("katea: {name} missing from the committed image payload"))
}

/// The default `(CONFIG.SYS, AUTOEXEC.BAT)` bytes from the committed image payload
/// — used by Repair, which has no `payload` already in scope.
fn default_config_pair() -> (Vec<u8>, Vec<u8>) {
    let payload = katea_volume::extract_system_payload(izarravm_firmware::tokados_hdd_img());
    (
        payload_file(&payload, "CONFIG.SYS"),
        payload_file(&payload, "AUTOEXEC.BAT"),
    )
}

/// Merge `overrides` into a system-file list: replace an existing entry whose name
/// matches case-insensitively, else append. Used to overlay a runner AUTOEXEC.BAT +
/// extra tools onto the standard Katea payload.
fn apply_overrides(base: &mut Vec<(String, Vec<u8>)>, overrides: Vec<(String, Vec<u8>)>) {
    for (name, bytes) in overrides {
        match base.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(&name)) {
            Some(slot) => slot.1 = bytes,
            None => base.push((name, bytes)),
        }
    }
}

impl Machine {
    /// Shared field initialization for the public constructors. They differ only
    /// in the CPU entry state and the ROM image, so each hands those in and
    /// shares the rest (devices, audio chips, timing accumulators). The caller
    /// installs the BIOS stubs and any boot/program image afterwards, where the
    /// ordering relative to those memory writes matters.
    fn base(profile: MachineProfile, cpu: CpuGsw, mut rom: Vec<u8>) -> Result<Self, MachineError> {
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
        let distira = Distira::new();
        let pci = PciConfig::new(profile.video == VideoCard::Distira);
        let memory = Memory::from_mib(profile.memory_mib)?;
        let ram_lookup = RamPageLookup::new(memory.len(), &pci);
        let timing = TimingFactors::for_clock(active_mode.clock_hz());
        // Lay the HLE entry stubs into ROM. PSP:0005 reaches the CALL 5 adapter
        // through the low-memory DOS entry at 0000:00C0.
        install_bios_font_mirror(&mut rom);
        rom[DOS_CALL5_ROM_OFFSET..DOS_CALL5_ROM_OFFSET + DOS_CALL5_ENTRY_STUB.len()]
            .copy_from_slice(&DOS_CALL5_ENTRY_STUB);
        rom[BIOS_TIMER_ISR_ROM_OFFSET..BIOS_TIMER_ISR_ROM_OFFSET + BIOS_TIMER_ISR_STUB.len()]
            .copy_from_slice(&BIOS_TIMER_ISR_STUB);
        rom[BIOS_MASTER_IRQ_ISR_ROM_OFFSET
            ..BIOS_MASTER_IRQ_ISR_ROM_OFFSET + BIOS_MASTER_IRQ_ISR_STUB.len()]
            .copy_from_slice(&BIOS_MASTER_IRQ_ISR_STUB);
        write_bios_int_stub_table(&mut rom);
        let mut machine = Self {
            memory,
            ram_lookup,
            profile,
            active_mode,
            pending_mode: None,
            timing,
            cpu,
            cache_model: CacheModel::new(cpu_level_for_mode(active_mode)),
            bus_rem: 0,
            video: Box::new(Vga::default()),
            paradise_non_vga: false,
            paradise_regs: [0; 6],
            margo: Margo::default(),
            distira,
            pci,
            margo_active: false,
            text_scanline_override: None,
            pending_soft_int: None,
            last_int_vector: None,
            io_touched: false,
            isa_io_batch_clocks: 0,
            device_wrote_memory: false,
            direct_map_changed: false,
            host_profile: MachineHostProfile::default(),
            pending_toka_service: None,
            toka_service_status: 0,
            katea_root: None,
            dos_screen_shown: 0,
            program_runtime: false,
            program_output: Vec::new(),
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
            speaker_transitions: Vec::new(),
            dma: dma::DmaController::default(),
            fdc: fdc::Fdc::default(),
            opl: OplChip::default(),
            resampler: Resampler::new(OPL_NATIVE_HZ, DAC_HZ),
            card_amp: 1.0,
            speaker_volume: 1.0,
            opl_micros: 0.0,
            dsp: SbDsp::default(),
            // Placeholder; sync_dsp_resampler rebuilds this for the live rate on
            // first use, so the value here never reaches the DAC as-is.
            dsp_resampler: Resampler::new(OPL_NATIVE_HZ, DAC_HZ),
            dsp_rate_hz: 0,
            dsp_micros: 0.0,
            dsp_sample_phase: 0.0,
            last_audio_clocks: 0,
            mixer,
            wss,
            // Placeholder; sync_wss_resampler rebuilds this for the live rate on
            // first use, so the value here never reaches the DAC as-is.
            wss_resampler: Resampler::new(OPL_NATIVE_HZ, DAC_HZ),
            wss_rate_hz: 0,
            wss_sample_phase: 0.0,
            cd_sample_phase: 0.0,
            wss_base,
            wss_irq,
            wss_dma,
            wss_enabled,
            margo_ns: 0.0,
            vga_dots: 0.0,
            trace: {
                let mut trace = BusTrace::default();
                trace.set_tracing_mode(TracingMode::Off);
                trace
            },
            elapsed_clocks: 0,
            io_stall_clocks: 0,
            // The INT 2Fh AH=13h "previous handler" default is INT 13h's own
            // per-vector stub, so a guest invoking it via pushf+call far is
            // serviced by address on every arrival route (the legacy shared
            // FF00:0000 would need an armed stash the call path never sets).
            dos_disk_handler: (BIOS_ROM_IRET_SEG, bios_int_stub_off(0x13)),
            dos_disk_restore: (BIOS_ROM_IRET_SEG, bios_int_stub_off(0x13)),
            network_post_address: (0, 0),
            floppy: None,
            floppy_accesses: 0,
            c_accesses: 0,
            ide: ide::IdeChannel::new(),
            icdex_vd_preference: 0x0100,
            ata: None,
            fat32_c: None,
            cd_accesses: 0,
            cd_audio_frac: 0.0,
            rtc: rtc::Rtc::new(),
            rtc_seconds: 0.0,
            fast_post: true,
            booter_inert: false,
            last_abs: (MOUSE_GUEST_CENTER_X, MOUSE_GUEST_CENTER_Y),
            unittester: unittester::UnitTester::default(),
            test_snapshot_path: None,
            #[cfg(test)]
            test_prior_core_pushes: Vec::new(),
            #[cfg(test)]
            test_batch_core_totals: Vec::new(),
        };
        // The Margo LFB aperture is decoded before RAM, so system memory must
        // stay below it. Validated config caps memory far under this bound.
        debug_assert!(
            machine.memory.len() as u64 <= u64::from(MARGO_LFB_BASE),
            "system RAM overlaps the Margo LFB aperture at 0xE0000000"
        );
        // Seed NVRAM 0x12 (the GSW code the BIOS applies at POST) from the boot
        // profile so a fresh CMOS reproduces the profile's speed; a loaded
        // cmos.bin then overwrites it with the user's saved choice.
        machine.set_cmos_byte(0x12, gsw_mode_code(machine.active_mode));
        Ok(machine)
    }

    pub fn new(profile: MachineProfile, rom: impl AsRef<[u8]>) -> Result<Self, MachineError> {
        let rom = rom.as_ref();
        // Accept either a bare 64 KiB BIOS or a larger flash image; in both cases
        // the CPU sees the top BIOS_ROM_SIZE bytes shadowed at 0xF0000.
        if rom.len() < BIOS_ROM_SIZE {
            return Err(MachineError::InvalidRomSize(rom.len()));
        }
        let shadow = &rom[rom.len() - BIOS_ROM_SIZE..];

        let mut machine = Self::base(profile, CpuGsw::default(), shadow.to_vec())?;
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

    pub fn distira_render_threads(&self) -> u8 {
        self.distira.render_threads()
    }

    pub fn set_distira_render_threads(&mut self, threads: u8) {
        self.distira.set_render_threads(threads);
    }

    pub fn drain_distira_fifo(&mut self) {
        self.distira.drain_fifo();
    }

    /// Enter or leave booter-inert mode. When set, the Toka-DOS HLE and IZEMM stop
    /// intercepting the DOS/memory-manager interrupts (0x20/0x21/0x25/0x26/0x29/
    /// 0x2F), so a self-booting disk's own handlers run through the IVT;
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
        let _ = self.publish_fixed_disk_parameter_table();
        let _ = self.memory.write_u8(0x475, 1); // BDA fixed-disk count
    }

    /// Mount a host folder as C: through Katea with extra InMemory system files
    /// overlaid on top of the standard payload. Each entry in `overrides` replaces
    /// an existing payload file of the same name (case-insensitive, e.g. a custom
    /// AUTOEXEC.BAT) or is appended (e.g. a runner tool). Overlaid files win 8.3
    /// collisions and are never written to the host folder.
    pub fn mount_hdd_folder_with(
        &mut self,
        dir: &std::path::Path,
        overrides: Vec<(String, Vec<u8>)>,
    ) -> std::io::Result<()> {
        // The system payload comes from the committed image; HELLO.TXT is the
        // static demo file, dropped here so the host folder supplies the user files.
        let payload = katea_volume::extract_system_payload(izarravm_firmware::tokados_hdd_img());
        let mut system_files: Vec<(String, Vec<u8>)> = payload
            .files
            .into_iter()
            .filter(|(name, _)| !name.eq_ignore_ascii_case("HELLO.TXT"))
            .collect();
        apply_overrides(&mut system_files, overrides);

        // The recursive tree volume walks `dir` (metadata only) overlaying the
        // system files at the root, and serves FAT/dir sectors on demand + file
        // data lazily. The boot sectors carry the dynamic geometry derived from
        // the folder.
        let volume =
            katea_tree::KateaTreeVolume::new(&payload.mbr, &payload.vbr, dir, &system_files)?;
        self.ata = Some(ata::AtaDisk::from_host_folder(volume));
        let _ = self.publish_fixed_disk_parameter_table();
        let _ = self.memory.write_u8(0x475, 1); // BDA fixed-disk count
        Ok(())
    }

    /// Mount a host folder as C: through Katea in "user-folder mode": seed the
    /// default CONFIG.SYS/AUTOEXEC.BAT into `dir` if missing, then overlay only the
    /// OS binaries so the host folder's config files are authoritative (the user
    /// owns them). The GUI and `--hdd-folder` use this. For the override mode (a
    /// throwaway runner disk) see [`mount_hdd_folder_with`](Self::mount_hdd_folder_with).
    pub fn mount_hdd_folder(&mut self, dir: &std::path::Path) -> std::io::Result<()> {
        let payload = katea_volume::extract_system_payload(izarravm_firmware::tokados_hdd_img());
        // Seed the user-owned config from the payload we already hold (parse the
        // image once), before `user_folder_overlay` below consumes `payload.files`.
        ensure_user_config(
            dir,
            &payload_file(&payload, "CONFIG.SYS"),
            &payload_file(&payload, "AUTOEXEC.BAT"),
        )?;
        let system_files = user_folder_overlay(payload.files);
        let volume =
            katea_tree::KateaTreeVolume::new(&payload.mbr, &payload.vbr, dir, &system_files)?;
        self.ata = Some(ata::AtaDisk::from_host_folder(volume));
        self.katea_root = Some(dir.to_path_buf());
        let _ = self.publish_fixed_disk_parameter_table();
        let _ = self.memory.write_u8(0x475, 1); // BDA fixed-disk count
        Ok(())
    }

    /// Mount a synthesized FAT32 volume as drive C: for the DOS absolute-disk
    /// interface. INT 25h reads its sectors; INT 26h writes are write-protected
    /// (the volume is read-only). Build one with `build_fat32`.
    pub fn mount_fat32(&mut self, volume: Fat32Volume) {
        self.fat32_c = Some(volume);
    }

    /// Eject the hard disk, returning its current image bytes (including any
    /// in-session writes) so the caller can flush them back. None when no disk is
    /// mounted OR when the disk is a read-only host-folder facade (which has no
    /// flushable image — returning its empty `bytes()` would persist a 0-byte file).
    /// Clears the BDA fixed-disk count.
    pub fn eject_hdd(&mut self) -> Option<Vec<u8>> {
        if let Some(disk) = self.ata.as_mut() {
            disk.reconcile_host_folder(); // final pass for a host folder; no-op for images
        }
        let bytes = self
            .ata
            .take()
            .filter(ata::AtaDisk::is_image)
            .map(|d| d.bytes().to_vec());
        let _ = self.clear_fixed_disk_parameter_table();
        let _ = self.memory.write_u8(0x475, 0);
        bytes
    }

    /// Force a final reconcile of a mounted Katea host folder, materializing any
    /// overlay-held files to the host. Safe to call when no folder is mounted (a
    /// no-op for an image-backed disk or no disk). Used by the headless `--hdd-
    /// folder` path and the e2e test before reading the host folder back.
    pub fn flush_hdd_folder(&mut self) {
        if let Some(disk) = self.ata.as_mut() {
            disk.reconcile_host_folder();
        }
    }

    /// Whether the mounted hard disk took a guest write this session, so the host
    /// flushes the image back only when it changed. False when no disk is mounted.
    pub fn hdd_dirty(&self) -> bool {
        self.ata.as_ref().is_some_and(|d| d.dirty)
    }

    fn publish_fixed_disk_parameter_table(&mut self) -> Result<(), BusError> {
        let Some(disk) = self.ata.as_ref() else {
            return self.clear_fixed_disk_parameter_table();
        };
        let cylinders = disk.cylinders().min(u32::from(u16::MAX)) as u16;
        let heads = disk.heads().min(u32::from(u8::MAX)) as u8;
        let spt = disk.sectors_per_track().min(u32::from(u8::MAX)) as u8;
        let base = BIOS_FIXED_DISK_PARAMETER_TABLE_ADDR as usize;
        self.memory.write_u16(base, cylinders)?;
        self.memory.write_u8(base + 2, heads)?;
        self.memory.write_u16(base + 3, 0)?; // reduced write current, XT only
        self.memory.write_u16(base + 5, 0)?; // write precompensation
        self.memory.write_u8(base + 7, 0)?; // ECC burst length, XT only
        self.memory
            .write_u8(base + 8, if heads > 8 { 0x08 } else { 0x00 })?;
        self.memory.write_u8(base + 9, 0)?; // standard timeout, XT only
        self.memory.write_u8(base + 10, 0)?; // formatting timeout, XT only
        self.memory.write_u8(base + 11, 0)?; // drive-check timeout, XT only
        self.memory.write_u16(base + 12, cylinders)?;
        self.memory.write_u8(base + 14, spt)?;
        self.memory.write_u8(base + 15, 0)?;
        self.memory.write_u16(
            0x41 * 4,
            (BIOS_FIXED_DISK_PARAMETER_TABLE_ADDR & 0x0F) as u16,
        )?;
        self.memory.write_u16(
            0x41 * 4 + 2,
            (BIOS_FIXED_DISK_PARAMETER_TABLE_ADDR >> 4) as u16,
        )?;
        Ok(())
    }

    fn clear_fixed_disk_parameter_table(&mut self) -> Result<(), BusError> {
        for i in 0..16 {
            self.memory
                .write_u8(BIOS_FIXED_DISK_PARAMETER_TABLE_ADDR as usize + i, 0)?;
        }
        self.memory.write_u16(0x41 * 4, 0)?;
        self.memory.write_u16(0x41 * 4 + 2, 0)
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
        let valid = self.rtc.load_nvram(bytes);
        if let Some(mode) = gsw_mode_from_code(self.rtc.nvram_byte(0x12)) {
            self.set_mode(mode);
        }
        valid
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

        // Machine::base lays the FF00:0000 nop;iret and the per-vector stub
        // table into every ROM, including this synthetic boot ROM.
        let rom = vec![0u8; BIOS_ROM_SIZE];
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

    /// Build a machine with a DOS-format program loaded and ready to run, with
    /// no DOS kernel behind it — only `handle_raw_program_int`'s minimal
    /// terminate/console-I/O surface services interrupts. For tests and
    /// benchmarks that need a quick runnable machine, not C: drive access.
    /// See `dev_docs/2026-06-30-katea-sp3-program-runtime-design.md`.
    pub fn new_raw_program(profile: MachineProfile, image: &[u8]) -> Result<Self, MachineError> {
        let env_entries = sound_blaster_env_entries(&profile.sound_blaster);
        let mut rom = vec![0u8; BIOS_ROM_SIZE];
        let kb = izarravm_firmware::kbd_resident_bios();
        rom[..kb.len()].copy_from_slice(kb);
        let mut machine = Self::base(profile, CpuGsw::default(), rom)?;
        install_boot_bios_stubs(&mut machine.memory)?;
        machine.install_keyboard_bios()?;
        machine.program_runtime = true;

        let entry = raw_program::load_program(image, &mut machine.memory, DOS_LOAD_SEGMENT)?;
        machine.apply_raw_program_entry(entry);
        let prog_top = machine
            .memory
            .read_u16(usize::from(DOS_LOAD_SEGMENT) * 16 + 2)?;
        let entries: Vec<(&str, &str)> = env_entries
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        raw_program::place_environment(&mut machine.memory, DOS_LOAD_SEGMENT, prog_top, &entries)?;

        // Seed PIT counter 0 the way the BIOS POST leaves it, so a program that
        // polls the timer for a delay doesn't spin forever.
        {
            let mut bus = machine.make_bus();
            let _ = bus.write_io(0x43, BusWidth::Byte, 0x34, false);
            let _ = bus.write_io(0x40, BusWidth::Byte, 0x00, false);
            let _ = bus.write_io(0x40, BusWidth::Byte, 0x00, false);
        }
        Ok(machine)
    }

    /// Accumulated console output for a `new_raw_program` machine.
    pub fn program_output(&self) -> &[u8] {
        &self.program_output
    }

    /// Seed the BDA keyboard ring with input bytes for a `new_raw_program`
    /// machine's character-input calls. Holds up to 15 bytes.
    pub fn set_program_stdin(&mut self, bytes: &[u8]) {
        const KBD_BDA_BASE: usize = 0x400;
        const KBD_HEAD: usize = 0x1a;
        const KBD_TAIL: usize = 0x1c;
        const KBD_RING_START: u16 = 0x1e;
        const KBD_RING_END: u16 = 0x3e;
        debug_assert!(bytes.len() < 16, "keyboard ring holds 15 entries");
        let _ = self
            .memory
            .write_u16(KBD_BDA_BASE + KBD_HEAD, KBD_RING_START);
        let mut off = KBD_RING_START;
        for &b in bytes {
            let _ = self
                .memory
                .write_u16(KBD_BDA_BASE + off as usize, u16::from(b));
            off += 2;
            if off >= KBD_RING_END {
                off = KBD_RING_START;
            }
        }
        let _ = self.memory.write_u16(KBD_BDA_BASE + KBD_TAIL, off);
    }

    /// Set the CPU to a loaded raw program's entry from its six-field
    /// `raw_program::ProgramEntry` (CS/DS/ES/SS + IP/SP).
    fn apply_raw_program_entry(&mut self, entry: raw_program::ProgramEntry) {
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
                bus.write_io(port, BusWidth::Byte, u32::from(value), false)?;
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

    pub fn cpu(&self) -> &CpuGsw {
        &self.cpu
    }

    /// Turn on the JIT's hotness auto-admission (feature `jit`; a no-op unless the CPU was built
    /// with it). Lets a headless run compile hot loops so the game anchors can be measured with the
    /// JIT active. Off by default; the CLI flips it from the `IZARRAVM_JIT` env.
    pub fn set_jit_auto_admit(&mut self, on: bool) {
        #[cfg(feature = "jit")]
        self.cpu.set_jit_auto_admit(on);
        #[cfg(not(feature = "jit"))]
        let _ = on;
    }

    pub fn enable_host_profiling(&mut self, sample_stride: u64) {
        self.host_profile.enable();
        self.cpu.enable_profiling(sample_stride);
    }

    pub fn disable_host_profiling(&mut self) {
        self.host_profile.disable();
        self.cpu.disable_profiling();
    }

    pub fn host_profile_snapshot(&self) -> MachineHostProfileSnapshot {
        self.host_profile.snapshot()
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
    /// reporting is enabled, this requests IRQ12 so a guest ISR runs.
    pub fn inject_mouse(&mut self, dx: i32, dy: i32, buttons: u8) {
        if self.keyboard.inject_mouse(dx, dy, buttons) {
            self.pic.request(12);
        }
    }

    /// Inject a scroll-wheel detent as a PS/2 packet (IntelliMouse 4-byte mode).
    pub fn inject_mouse_wheel(&mut self, dz: i32) {
        if self.keyboard.inject_mouse_wheel(dz) {
            self.pic.request(12);
        }
    }

    /// Map the GUI's absolute captured pointer onto relative aux-device motion.
    /// The host pointer is clamped to the virtual space; the delta against the
    /// previous position drives a PS/2 packet through the 8042, so the guest
    /// INT 74h ISR and the mouse driver see real hardware motion. A button-only
    /// change (zero delta) still injects a packet so the button edge reaches the
    /// guest.
    pub fn set_mouse_absolute(&mut self, x: i32, y: i32, buttons: u8) {
        let x = x.clamp(0, MOUSE_GUEST_MAX_X);
        let y = y.clamp(0, MOUSE_GUEST_MAX_Y);
        let dx = x - self.last_abs.0;
        let dy = y - self.last_abs.1;
        self.last_abs = (x, y);
        self.inject_mouse_relative(dx, dy, buttons);
    }

    /// Inject a relative mouse motion (the host's per-flush coalesced delta) as one
    /// PS/2 packet. A real PS/2 mouse only ever conveys one packet's worth of motion
    /// (the 9-bit signed range, +-255) however far it physically travelled between
    /// samples; it does not retroactively split an extreme delta into a train of
    /// catch-up packets, so a clamp here matches real hardware (and a low-resolution
    /// DOS game's cursor has no use for more precision than that anyway). Splitting
    /// instead of clamping was tried first and made a violent host flick queue
    /// dozens of packets at once -- far more than any real mouse could transmit --
    /// which starved the guest's other interrupts (timer, keyboard) for long enough
    /// to look like a freeze while it drained the backlog.
    pub fn inject_mouse_relative(&mut self, dx: i32, dy: i32, buttons: u8) {
        let sx = dx.clamp(-255, 255);
        let sy = dy.clamp(-255, 255);
        self.inject_mouse(sx, sy, buttons);
    }

    /// Seed the absolute-pointer origin without injecting motion, called when the
    /// GUI enters capture and re-centres its pointer. Without this the first
    /// post-capture update would diff against a stale position and synthesize a
    /// large bogus delta.
    pub fn seed_mouse_origin(&mut self, x: i32, y: i32) {
        self.last_abs = (x.clamp(0, MOUSE_GUEST_MAX_X), y.clamp(0, MOUSE_GUEST_MAX_Y));
    }

    /// Test seam: register a mouse packet handler (the INT 15h C207 effect),
    /// enable aux reporting, and enable IRQ12 in the 8042 command byte, so a guest
    /// self-test can install a handler without a full driver. `seg:off` is the far
    /// pointer the BIOS INT 74h ISR will call.
    pub fn register_mouse_handler_for_test(&mut self, seg: u16, off: u16) {
        let base = (u32::from(EBDA_SEGMENT) << 4) + EBDA_MOUSE_HANDLER_OFF;
        self.write_physical_u16(base, off);
        self.write_physical_u16(base + 2, seg);
        // Ensure the 8042 command byte has IRQ12 (bit1) enabled, then turn on aux
        // reporting. Without bit1, a latched aux byte never arms IRQ12.
        self.keyboard.write_port(0x64, 0x60); // write-command-byte
        self.keyboard.write_port(0x60, 0x03); // IRQ1 + IRQ12 enabled
        self.keyboard.set_mouse_reporting(true);
    }

    #[cfg(test)]
    fn read_io_port_u8(&mut self, port: u16) -> u8 {
        let mut bus = self.make_bus();
        bus.read_io(port, BusWidth::Byte, 0, false).unwrap_or(0) as u8
    }

    #[cfg(test)]
    fn irq1_pending(&self) -> bool {
        self.pic.irr_bit(1)
    }

    #[cfg(test)]
    fn irq12_pending(&self) -> bool {
        self.pic.irr_bit(12)
    }

    /// Set the 8042 command byte's IRQ1+IRQ12 enable bits the way the keyboard
    /// BIOS does, so a latched aux byte arms IRQ12. `int15_machine` boots a zeroed
    /// ROM that never runs that BIOS, so a test that wants IRQ12 must do this
    /// first.
    #[cfg(test)]
    fn enable_8042_irq12(&mut self) {
        let mut bus = self.make_bus();
        bus.write_io(0x64, BusWidth::Byte, 0x20, false).unwrap();
        let ccb = bus.read_io(0x60, BusWidth::Byte, 0, false).unwrap() as u8;
        bus.write_io(0x64, BusWidth::Byte, 0x60, false).unwrap();
        bus.write_io(0x60, BusWidth::Byte, u32::from(ccb | 0x01 | 0x02), false)
            .unwrap();
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
        // Captured before the struct literal below since video/trace are also
        // mutably borrowed by other fields in that same literal.
        let beam_at_batch_start = self.video.beam_dots();
        let trace_elapsed_at_batch_start = self.trace.elapsed_clocks();
        // Read from self.cpu.level(), the same source scale_bus reads from, not
        // cpu_level_for_mode(self.active_mode) -- see run_until_clock's matching
        // capture for why the two can diverge.
        let (bus_num_at_batch_start, bus_den_at_batch_start) = bus_timing(self.cpu.level());
        MachineBus {
            memory: &mut self.memory,
            ram_lookup: &mut self.ram_lookup,
            video: &mut self.video,
            margo: &mut self.margo,
            distira: &mut self.distira,
            pci: &mut self.pci,
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
            last_int_vector: &mut self.last_int_vector,
            active_mode: self.active_mode,
            pending_mode: &mut self.pending_mode,
            fast_post: self.fast_post,
            booter_inert: self.booter_inert,
            program_runtime: self.program_runtime,
            pending_toka_service: &mut self.pending_toka_service,
            toka_service_status: self.toka_service_status,
            unittester: &mut self.unittester,
            wait_states: self.profile.wait_states,
            cache: &mut self.cache_model,
            flat_data_cost: matches!(self.active_mode.timing_class(), TimingClass::Approximate),
            lazy_port_reads: matches!(self.active_mode.timing_class(), TimingClass::Approximate),
            io_touched: &mut self.io_touched,
            isa_io_clocks: &mut self.isa_io_batch_clocks,
            device_wrote_memory: &mut self.device_wrote_memory,
            direct_map_changed: &mut self.direct_map_changed,
            core_clocks_so_far: 0,
            prior_runs_core_clocks: 0,
            elapsed_clocks_at_batch_start: self.elapsed_clocks,
            vga_dots_at_batch_start: self.vga_dots,
            beam_at_batch_start,
            trace_elapsed_at_batch_start,
            bus_rem_at_batch_start: self.bus_rem,
            inv_clock_at_batch_start: self.timing.inv_clock,
            bus_num_at_batch_start,
            bus_den_at_batch_start,
            pit_clocks_at_batch_start: self.pit_clocks,
            pit_per_clock_at_batch_start: self.timing.pit_per_clock,
        }
    }

    pub fn read_physical_u8(&mut self, address: u32) -> u8 {
        let mut bus = self.make_bus();
        bus.read_phys_u8(address).unwrap_or(0)
    }

    pub fn read_physical_u16(&mut self, address: u32) -> u16 {
        let mut bus = self.make_bus();
        bus.read_memory(address, BusWidth::Word, BusAccessKind::DataRead)
            .map(|value| value as u16)
            .unwrap_or(0)
    }

    pub fn read_physical_u32(&mut self, address: u32) -> u32 {
        let mut bus = self.make_bus();
        bus.read_memory(address, BusWidth::Dword, BusAccessKind::DataRead)
            .unwrap_or(0)
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

    pub fn write_physical_u16(&mut self, address: u32, value: u16) {
        let mut bus = self.make_bus();
        let _ = bus.write_memory(
            address,
            BusWidth::Word,
            u32::from(value),
            BusAccessKind::DataWrite,
        );
    }

    pub fn write_physical_u32(&mut self, address: u32, value: u32) {
        let mut bus = self.make_bus();
        let _ = bus.write_memory(address, BusWidth::Dword, value, BusAccessKind::DataWrite);
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

    /// Select a VGA graphics mode by its INT 10h number from the host side. Returns
    /// false for an unimplemented number. On success it hands the display back to
    /// the VGA core by clearing the Margo latch.
    pub fn set_vga_mode(&mut self, mode: u8) -> bool {
        self.set_vga_mode_with_clear(mode, false)
    }

    fn set_vga_mode_with_clear(&mut self, mode: u8, clear: bool) -> bool {
        let ok = self.video.set_mode_with_clear(mode, clear);
        if ok {
            self.margo_active = false;
            self.distira.disable_display();
        }
        ok
    }

    /// Whether the Margo linear-framebuffer display is the active output (the GUI
    /// renders it instead of the VGA text/graphics core). A VGA mode set via INT
    /// 10h clears this latch. Exposed so a test can assert the BIOS hands the
    /// display back to VGA text before booting an OS.
    pub fn margo_active(&self) -> bool {
        self.margo_active
    }

    /// Whether the guest is executing in virtual-8086 mode (under the TOKAEMM
    /// ring-0 monitor). Exposed so the SP-4b M4 default-boot e2e can assert the
    /// default CONFIG.SYS really put the system in V86.
    pub fn in_v86(&self) -> bool {
        self.cpu.is_v86_mode()
    }

    fn int10_set_mode_number(&mut self, requested_mode: u8) -> bool {
        let mode = requested_mode & 0x7F;
        let clear = requested_mode & 0x80 == 0;
        match mode {
            0x0D..=0x13 => {
                if !self.set_vga_mode_with_clear(mode, clear) {
                    return false;
                }
                let cols = if matches!(mode, 0x0D | 0x13) { 40 } else { 80 };
                self.set_bda_video_mode(requested_mode, cols, Self::video_text_rows(mode));
            }
            0x04..=0x06 => {
                self.video.set_cga_mode_with_clear(mode, clear);
                self.margo_active = false;
                self.distira.disable_display();
                let cols = if mode == 0x06 { 80 } else { 40 };
                self.set_bda_video_mode(requested_mode, cols, Self::video_text_rows(mode));
            }
            0x00..=0x03 | 0x07 => {
                self.margo_active = false;
                self.distira.disable_display();
                let cols: u16 = if mode <= 0x01 { 40 } else { 80 };
                if mode == 0x07 {
                    self.video.set_mono_text_mode();
                } else if let Some(scanlines) = self.text_scanline_override {
                    let _ = self
                        .video
                        .set_color_text_mode_scanlines(mode, scanlines, clear);
                } else if mode <= 0x02 {
                    let _ = self.video.set_cga_text_mode_with_clear(mode, clear);
                } else {
                    self.video.set_text_mode_columns(usize::from(cols));
                }
                self.set_bda_video_mode(requested_mode, cols, Self::video_text_rows(mode));
            }
            _ => return false,
        }
        self.set_eax_al(Self::video_mode_set_return_al(mode));
        true
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
        if matches!(ax, 0x0070 | 0x6F05) {
            return;
        }
        if matches!(ax, 0x6A00..=0x6A02) {
            self.int10_dgis(ax);
            return;
        }
        if ah == 0x00 {
            if al == 0x7e {
                self.int10_paradise_set_special_mode();
                return;
            }
            if al == 0x7f {
                self.int10_paradise_extended(bh, bl);
                return;
            }
            if self.int10_set_mode_number(al) {
                return;
            }
        }
        if ah == 0x05 {
            // INT 10h AH=05h SELECT ACTIVE DISPLAY PAGE (RBIL INTERRUP.A:2162).
            // AL is the page number. CGA graphics modes have only page 0; text
            // modes page by moving the CRTC start address in character cells.
            // EGA planar graphics modes page in byte-address units.
            let mode = self.read_physical_u8(0x449) & 0x7F;
            if matches!(mode, 0x04..=0x06) {
                let _ = self.memory.write_u8(0x462, 0);
                let _ = self.memory.write_u16(0x44e, 0);
                return;
            }
            if let Some((page, page_start)) = self.ega_graphics_page_start(mode, al) {
                self.video.set_start_address(page_start);
                let _ = self.memory.write_u8(0x462, page);
                let _ = self.memory.write_u16(0x44e, page_start as u16);
                return;
            }
            let page = self.normalize_text_page(al);
            let stride = self.text_page_stride();
            let page_start = usize::from(page) * stride;
            self.video.set_start_address((page_start / 2) as u32);
            let _ = self.memory.write_u8(0x462, page);
            let _ = self.memory.write_u16(0x44e, page_start as u16);
            let pos = self.cursor_pos(page);
            self.set_hardware_cursor_for_page(page, pos);
            return;
        }
        if ah == 0x0b {
            match bh {
                // BH=0: BL is the border/overscan color. In CGA graphics it also
                // sets the 3D9h background/foreground nibble plus intensity.
                0x00 => {
                    self.video.set_overscan(bl);
                    if self.video.active_mode() == VideoMode::Cga {
                        let current = self.video.cga_color_select();
                        let _ = self
                            .video
                            .write_port(0x3D9, (current & !0x1F) | (bl & 0x1F));
                    }
                }
                // BH=1: BL bit0 selects CGA palette 0 vs 1 for 320x200x4.
                0x01 => {
                    let current = self.video.cga_color_select();
                    let _ = self
                        .video
                        .write_port(0x3D9, (current & !0x20) | ((bl & 1) << 5));
                }
                _ => {}
            }
            if self.video.is_cga_personality() {
                self.sync_bda_cga_latches();
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
        if ah == 0x04 {
            self.int10_read_light_pen();
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
        if ah == 0x12 {
            self.handle_int10_alternate(al, bl);
            return;
        }
        if ah == 0x13 {
            self.int10_write_string();
            return;
        }
        if ah == 0x15 {
            // Convertible display parameters: no alternate physical display.
            self.set_ax(0x0000);
            return;
        }
        if ah == 0x1c {
            self.int10_save_restore_state(al);
            return;
        }
        if matches!(ah, 0x70 | 0x71) {
            // Tandy 1000 RAM address queries. This VGA profile has no Tandy planes.
            self.set_ax(0x0000);
            self.set_bx(0x0000);
            self.set_cx(0x0000);
            self.set_dx(0x0000);
            return;
        }
        if ah == 0xbf {
            // Compaq switchable display extensions. AL=03 reports no switchable VDU;
            // the other subfunctions preserve registers as absent hardware.
            if al == 0x03 {
                self.set_bx(0x0000);
                self.set_cx(0x0000);
                self.set_dx(0x0000);
            }
            return;
        }
        if ah == 0xfa {
            // Microsoft mouse EGA register interface installation check.
            self.set_bx(0x0000);
            return;
        }
        if matches!(
            ah,
            0x14 | 0x40..=0x4e | 0x72 | 0x73 | 0x80..=0x82 | 0xf0..=0xf7 | 0xfe | 0xff
        ) {
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
            let page = self.read_physical_u8(0x462);
            let ebx = (self.cpu.registers.ebx() & !0xFF00) | (u32::from(page) << 8);
            self.cpu.registers.set_ebx(ebx);
            return;
        }
        if ah == 0x1a {
            // AH=1Ah display combination code. AL=00h reads and AL=01h writes
            // the BDA DCC byte, the same storage AH=1Bh reports.
            self.set_eax_al(0x1A);
            match al {
                0x00 => {
                    let dcc = self.read_physical_u8(0x48A);
                    self.set_bx(u16::from(dcc));
                }
                0x01 => {
                    let _ = self.memory.write_u8(0x48A, bl);
                }
                _ => {}
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

    fn int10_dgis(&mut self, ax: u16) {
        match ax {
            // DGIS inquire: no DGIS devices installed.
            0x6A00 => {
                self.set_bx(0x0000);
                self.set_cx(0x0000);
            }
            // DGIS redirect output: cannot redirect to a non-DGIS device.
            0x6A01 => self.set_cx(0x0000),
            // DGIS current output device: the current display is the BIOS VGA.
            0x6A02 => {
                self.cpu
                    .registers
                    .set_segment(SegmentIndex::Es, SegmentRegister::real(0x0000));
                let edi = self.cpu.registers.edi() & !0xFFFF;
                self.cpu.registers.set_edi(edi);
            }
            _ => {}
        }
    }

    fn uses_mono_crtc_base(&self) -> bool {
        self.memory.read_u16(0x463).unwrap_or(0x03D4) == 0x03B4
    }

    fn active_display_combination_code(&self) -> u8 {
        if self.uses_mono_crtc_base() {
            0x07
        } else {
            0x08
        }
    }

    /// INT 10h AH=1Bh. Writes the 64-byte video state-information block at ES:DI with the
    /// live mode, geometry, CGA latch shadows, and display-combination fields, plus
    /// a static functionality table pointer. Limit: only the commonly-read fields
    /// are populated; the VGA-present check that programs run only tests AL == 0x1B.
    fn int10_state_info(&mut self) {
        let es = self.cpu.registers.segment(SegmentIndex::Es).base;
        let di = self.cpu.registers.edi() as u16;
        let addr = es.wrapping_add(u32::from(di));
        let mode = self.read_physical_u8(0x449);
        let cols = self.read_guest_word(0x44a);
        let page = self.read_physical_u8(0x462);
        let rows_minus_1 = self.read_physical_u8(0x484);
        let page_size = self.read_guest_word(0x44c);
        let page_start = self.read_guest_word(0x44e);
        let cursor_type = self.read_guest_word(0x460);
        let char_height = self.read_guest_word(0x485);
        let mut block = [0u8; 64];
        block[0..2].copy_from_slice(&0u16.to_le_bytes());
        block[2..4].copy_from_slice(&VGA_BIOS_SEGMENT.to_le_bytes());
        block[4] = mode;
        block[5..7].copy_from_slice(&cols.to_le_bytes());
        block[0x07..0x09].copy_from_slice(&page_size.to_le_bytes());
        block[0x09..0x0B].copy_from_slice(&page_start.to_le_bytes());
        for offset in 0..16 {
            block[0x0B + offset] = self.read_physical_u8(0x450 + offset as u32);
        }
        block[0x1B..0x1D].copy_from_slice(&cursor_type.to_le_bytes());
        block[0x1D] = page;
        block[0x1E..0x20].copy_from_slice(&self.read_guest_word(0x463).to_le_bytes());
        block[0x20] = self.read_physical_u8(0x465); // CGA mode-control shadow
        block[0x21] = self.read_physical_u8(0x466); // CGA color-select shadow
        block[0x22] = rows_minus_1.wrapping_add(1); // rows on screen
        block[0x23..0x25].copy_from_slice(&char_height.to_le_bytes());
        block[0x25] = self.read_physical_u8(0x48A);
        block[0x27..0x29].copy_from_slice(&Self::video_color_count(mode).to_le_bytes());
        block[0x29] = self.video_page_count(mode); // pages
        block[0x2A] = self.video_scanline_code(mode);
        self.write_guest_block(addr, &block);
        self.set_eax_al(0x1B);
    }

    /// INT 10h AH=12h BL=30h: record the BIOS's preferred scanline count for
    /// the *next* mode set. This is BDA/mode-set policy bookkeeping only (feeds
    /// `text_scanlines_for_mode`/`video_char_height` below) and is independent
    /// of `Vga::set_char_height`, which reprograms the live CRTC Maximum Scan
    /// Line register from AH=11h font-load calls.
    fn set_selected_text_scanlines(&mut self, al: u8) -> bool {
        let mut flags = self.read_physical_u8(0x489);
        let mut switches = self.read_physical_u8(0x488) & 0xF0;
        flags &= !0x90;
        match al {
            0x00 => {
                flags |= 0x80; // 200 scan lines
                switches |= 0x08;
                self.text_scanline_override = Some(200);
            }
            0x01 => {
                switches |= 0x09;
                self.text_scanline_override = Some(350);
            }
            0x02 => {
                flags |= 0x10; // 400 scan lines
                switches |= 0x09;
                self.text_scanline_override = Some(400);
            }
            _ => return false,
        }
        let _ = self.memory.write_u8(0x488, switches);
        let _ = self.memory.write_u8(0x489, flags);
        true
    }

    fn text_scanlines_for_mode(&self, mode: u8) -> u16 {
        self.text_scanline_override
            .unwrap_or(if (mode & 0x7F) <= 0x02 { 200 } else { 400 })
    }

    fn video_color_count(mode: u8) -> u16 {
        match mode & 0x7F {
            0x04 | 0x05 => 4,
            0x06 | 0x07 | 0x0F | 0x11 => 2,
            0x13 => 256,
            _ => 16,
        }
    }

    fn video_scanline_code(&self, mode: u8) -> u8 {
        match mode & 0x7F {
            0x00..=0x03 => match self.text_scanlines_for_mode(mode) {
                200 => 0,
                350 => 1,
                400 => 2,
                _ => 2,
            },
            0x07 | 0x0F | 0x10 => 1, // 350 active scan lines
            0x11 | 0x12 => 3,        // 480 active scan lines
            0x04..=0x06 | 0x13 => 0, // 200 active scan lines
            _ => 2,                  // VGA text modes default to 400
        }
    }

    /// Record the current video mode in the BDA so apps that read it directly
    /// (and INT 10h AH=0Fh) see a sane state. Columns and rows are the text-cell
    /// geometry the BIOS publishes for the mode.
    fn set_bda_video_mode(&mut self, mode: u8, columns: u16, rows: u8) {
        let _ = self.memory.write_u8(0x449, mode);
        let _ = self.memory.write_u16(0x44a, columns);
        let _ = self.memory.write_u16(0x44c, self.video_page_size(mode));
        let _ = self.memory.write_u8(0x484, rows.saturating_sub(1));
        let _ = self
            .memory
            .write_u16(0x485, u16::from(self.video_char_height(mode)));
        let _ = self.memory.write_u16(0x44e, 0);
        let _ = self.memory.write_u8(0x462, 0);
        for page in 0..8usize {
            let _ = self.memory.write_u16(0x450 + page * 2, 0);
        }
        let _ = self
            .memory
            .write_u16(0x463, Self::video_crtc_base_port(mode));
        let _ = self.memory.write_u8(0x487, 0x60 | (mode & 0x80));
        let _ = self
            .memory
            .write_u8(0x48A, self.active_display_combination_code());
        let _ = seed_bda_video_save_pointer(&mut self.memory);
        if let Some(mode_control) = Self::cga_bda_mode_control(mode) {
            let _ = self.memory.write_u8(0x465, mode_control);
            let _ = self
                .memory
                .write_u8(0x466, Self::cga_bda_color_select(mode));
        } else {
            let _ = self.memory.write_u8(0x465, 0);
            let _ = self.memory.write_u8(0x466, 0);
        }
    }

    fn int10_paradise_set_special_mode(&mut self) {
        let width = self.cpu.registers.ebx() as u16;
        let height = self.cpu.registers.ecx() as u16;
        let colors = self.cpu.registers.edx() as u16;
        let mode = match (width, height, colors) {
            (40, 25, 16) => Some(0x00),
            (80, 25, 16) => Some(0x03),
            (80, 25, 0) => Some(0x07),
            (320, 200, 4) => Some(0x04),
            (640, 200, 0 | 2) => Some(0x06),
            (320, 200, 16) => Some(0x0D),
            (640, 200, 16) => Some(0x0E),
            (640, 350, 0 | 2) => Some(0x0F),
            (640, 350, 16) => Some(0x10),
            (640, 480, 0 | 2) => Some(0x11),
            (640, 480, 16) => Some(0x12),
            (320, 200, 256) => Some(0x13),
            _ => None,
        };

        let ok = match mode {
            Some(mode) => self.int10_set_mode_number(mode),
            None => false,
        };
        if ok {
            self.set_eax_al(0x7E);
            self.set_bh(0x7E);
        } else {
            self.set_bh(0x00);
        }
    }

    fn int10_paradise_extended(&mut self, bh: u8, bl: u8) {
        let ok = match bh {
            0x00 => {
                self.paradise_non_vga = false;
                true
            }
            0x01 => {
                self.paradise_non_vga = true;
                true
            }
            0x02 => {
                self.set_bl(u8::from(self.paradise_non_vga));
                let used = self.int10_current_vram_units();
                self.set_cx((4 << 8) | u16::from(used));
                true
            }
            0x03 | 0x29..=0x2F | 0x60 | 0xA5 | 0xA6 => true,
            0x04 => {
                self.paradise_non_vga = true;
                self.int10_set_mode_number(0x07)
            }
            0x05 => {
                self.paradise_non_vga = true;
                true
            }
            0x06 => {
                self.paradise_non_vga = false;
                self.int10_set_mode_number(0x07)
            }
            0x07 => {
                self.paradise_non_vga = false;
                self.int10_set_mode_number(0x03)
            }
            0x0A..=0x0F => {
                self.paradise_regs[usize::from(bh - 0x0A)] = bl;
                true
            }
            0x1A..=0x1F => {
                self.set_bl(self.paradise_regs[usize::from(bh - 0x1A)]);
                true
            }
            0x61 => {
                let addr = self
                    .cpu
                    .registers
                    .segment(SegmentIndex::Es)
                    .base
                    .wrapping_add(u32::from(self.cpu.registers.edi() as u16));
                self.write_physical_u8(addr, 0);
                true
            }
            _ => false,
        };

        if ok {
            self.set_eax_al(0x7F);
            self.set_bh(0x7F);
        } else {
            self.set_bh(0x00);
        }
    }

    fn int10_current_vram_units(&mut self) -> u8 {
        match self.read_physical_u8(0x449) {
            0x12 => 3,
            0x0F..=0x11 => 2,
            _ => 1,
        }
    }

    /// INT 10h AH=12h alternate function select. The common VGA calls are mostly
    /// BIOS policy latches: report the configured adapter for BL=10h and mirror
    /// supported toggles into the VGA BDA bytes at 0040:0087-0089.
    fn handle_int10_alternate(&mut self, al: u8, bl: u8) {
        match bl {
            // BL=10h: return EGA/VGA configuration information.
            0x10 => {
                let switch_data = self.read_physical_u8(0x488);
                let mode = u8::from(self.uses_mono_crtc_base());
                let memory = 0x03u8; // 256 KiB installed
                let feature = (switch_data >> 4) & 0x0f;
                let switches = switch_data & 0x0f;
                self.set_bx((u16::from(mode) << 8) | u16::from(memory));
                self.set_cx((u16::from(feature) << 8) | u16::from(switches));
            }
            // BL=20h installs the video BIOS print-screen hook. The ROM print-screen
            // body is not modeled; accepting the call matches VGA BIOS probes.
            0x20 => {}
            // BL=30h: select text-mode scanline count for the next mode set.
            0x30 if al <= 0x02 => {
                if self.set_selected_text_scanlines(al) {
                    self.set_eax_al(0x12);
                }
            }
            // BL=31h: default palette loading on mode set.
            0x31 if al <= 0x01 => {
                self.video.set_default_palette_loading_enabled(al == 0x00);
                let mut flags = self.read_physical_u8(0x489);
                if al == 0x01 {
                    flags |= 0x08; // no palette load
                } else {
                    flags &= !0x08;
                }
                self.write_physical_u8(0x489, flags);
                self.set_eax_al(0x12);
            }
            // BL=32h: video memory/register addressing.
            0x32 if al <= 0x01 => {
                let misc = self.video.read_port(0x3CC).unwrap_or(0x67);
                let misc = if al == 0x00 {
                    misc | 0x02
                } else {
                    misc & !0x02
                };
                let _ = self.video.write_port(0x3C2, misc);
                self.set_eax_al(0x12);
            }
            // BL=33h: gray-scale summing policy.
            0x33 if al <= 0x01 => {
                self.video.set_grayscale_summing_enabled(al == 0x00);
                let mut flags = self.read_physical_u8(0x489);
                if al == 0x00 {
                    flags |= 0x02; // gray scaling enabled
                } else {
                    flags &= !0x02;
                }
                self.write_physical_u8(0x489, flags);
                self.set_eax_al(0x12);
            }
            // BL=34h: cursor emulation/scaling policy. This BIOS tracks both the
            // EGA/VGA video-control inhibit bit at 0040:0087 and the mode-set
            // control latch at 0040:0089 used by cursor-shape scaling.
            0x34 if al <= 0x01 => {
                let mut control = self.read_physical_u8(0x487);
                let mut flags = self.read_physical_u8(0x489);
                if al == 0x01 {
                    control |= 0x01;
                    flags &= !0x01;
                } else {
                    control &= !0x01;
                    flags |= 0x01;
                }
                self.write_physical_u8(0x487, control);
                self.write_physical_u8(0x489, flags);
                self.set_eax_al(0x12);
            }
            // BL=35h display switch: no second adapter is modeled, but the VGA BIOS
            // acknowledges the call.
            0x35 if al <= 0x03 => self.set_eax_al(0x12),
            // BL=36h refresh control.
            0x36 if al <= 0x01 => {
                self.video.set_display_refresh_enabled(al == 0x00);
                self.set_eax_al(0x12);
            }
            _ => {}
        }
    }

    fn cga_bda_mode_control(mode: u8) -> Option<u8> {
        match mode & 0x7F {
            0x00 => Some(0x2C),
            0x01 => Some(0x28),
            0x02 => Some(0x2D),
            0x03 => Some(0x29),
            0x04 => Some(0x0A),
            0x05 => Some(0x0E),
            0x06 => Some(0x1A),
            _ => None,
        }
    }

    fn video_crtc_base_port(mode: u8) -> u16 {
        match mode & 0x7F {
            0x07 | 0x0F => 0x03B4,
            _ => 0x03D4,
        }
    }

    fn cga_bda_color_select(mode: u8) -> u8 {
        if mode & 0x7F == 0x06 { 0x0F } else { 0x00 }
    }

    fn video_mode_set_return_al(mode: u8) -> u8 {
        match mode & 0x7F {
            0x06 => 0x3F,
            0x00..=0x05 | 0x07 => 0x30,
            _ => 0x20,
        }
    }

    fn sync_bda_cga_latches(&mut self) {
        let _ = self.memory.write_u8(0x465, self.video.cga_mode_control());
        let _ = self.memory.write_u8(0x466, self.video.cga_color_select());
    }

    fn video_page_size(&self, mode: u8) -> u16 {
        let mode = mode & 0x7F;
        match mode {
            0x00 | 0x01 => 0x0800,
            0x02 | 0x03 | 0x07 => 0x1000,
            0x0D => 0x2000,
            0x0E => 0x4000,
            0x0F | 0x10 => 0x8000,
            0x11 | 0x12 => 0x0000,
            0x04..=0x06 => 0x4000,
            0x13 => 320 * 200,
            _ => 0x1000,
        }
    }

    fn video_text_rows(mode: u8) -> u8 {
        match mode & 0x7F {
            0x11 | 0x12 => 30,
            _ => 25,
        }
    }

    fn video_char_height(&self, mode: u8) -> u8 {
        match mode & 0x7F {
            0x00..=0x03 => match self.text_scanlines_for_mode(mode) {
                200 => 8,
                350 => 14,
                400 => 16,
                _ => 16,
            },
            0x04..=0x06 | 0x0D | 0x0E | 0x13 => 8,
            0x07 | 0x0F | 0x10 => 14,
            _ => 16,
        }
    }

    fn text_columns(&mut self) -> usize {
        self.read_guest_word(0x44a).clamp(1, 80) as usize
    }

    fn text_rows(&mut self) -> usize {
        (usize::from(self.read_physical_u8(0x484)) + 1).clamp(1, 60)
    }

    fn text_page_stride(&mut self) -> usize {
        let size = self.read_guest_word(0x44c) as usize;
        if size != 0 {
            size
        } else if self.text_columns() <= 40 {
            0x0800
        } else {
            VGA_TEXT_PAGE_STRIDE
        }
    }

    fn text_aperture_size(&self) -> usize {
        if self.video.is_cga_personality() {
            CGA_FB_SIZE
        } else {
            VGA_TEXT_MEMORY_SIZE
        }
    }

    fn text_page_count(&mut self) -> u8 {
        (self.text_aperture_size() / self.text_page_stride()).clamp(1, 8) as u8
    }

    fn ega_graphics_page_count(&self, mode: u8) -> Option<u8> {
        let mode = mode & 0x7F;
        match mode {
            0x0D..=0x10 => Some(
                ((VGA_PLANAR_WINDOW_SIZE as usize) / usize::from(self.video_page_size(mode)))
                    .clamp(1, 8) as u8,
            ),
            0x11 | 0x12 => Some(1),
            _ => None,
        }
    }

    fn ega_graphics_page_start(&self, mode: u8, page: u8) -> Option<(u8, u32)> {
        let page = page % self.ega_graphics_page_count(mode)?;
        Some((
            page,
            u32::from(page) * u32::from(self.video_page_size(mode)),
        ))
    }

    fn video_page_count(&mut self, mode: u8) -> u8 {
        self.ega_graphics_page_count(mode)
            .unwrap_or_else(|| self.text_page_count())
    }

    fn normalize_text_page(&mut self, page: u8) -> u8 {
        page % self.text_page_count()
    }

    fn normalize_bios_page(&mut self, page: u8) -> u8 {
        match self.video.active_mode() {
            VideoMode::Cga => 0,
            VideoMode::Planar => {
                let mode = self.read_physical_u8(0x449);
                self.ega_graphics_page_start(mode, page)
                    .map(|(page, _)| page)
                    .unwrap_or(0)
            }
            _ => self.normalize_text_page(page),
        }
    }

    fn active_bios_page(&mut self) -> u8 {
        let page = self.read_physical_u8(0x462);
        self.normalize_bios_page(page)
    }

    fn text_page_base(&mut self, page: u8) -> usize {
        let page = self.normalize_text_page(page);
        usize::from(page) * self.text_page_stride()
    }

    fn text_offset(&mut self, page: u8, row: usize, col: usize) -> usize {
        let page = self.normalize_text_page(page);
        let columns = self.text_columns();
        let stride = self.text_page_stride();
        usize::from(page) * stride + (row * columns + col) * 2
    }

    fn cursor_pos(&mut self, page: u8) -> u16 {
        let page = self.normalize_bios_page(page);
        self.memory
            .read_u16(0x450 + usize::from(page) * 2)
            .unwrap_or(0)
    }

    fn set_cursor_pos(&mut self, page: u8, pos: u16) {
        let page = self.normalize_bios_page(page);
        let _ = self.memory.write_u16(0x450 + usize::from(page) * 2, pos);
        if !self.is_bios_graphics_text_mode() && page == self.active_bios_page() {
            self.set_hardware_cursor_for_page(page, pos);
        }
    }

    fn set_hardware_cursor_for_page(&mut self, page: u8, pos: u16) {
        let columns = self.text_columns();
        let row = usize::from(pos >> 8);
        let col = usize::from(pos & 0x00ff);
        let base_cells = self.text_page_base(page) / 2;
        self.video
            .set_cursor_offset((base_cells + row * columns + col) as u16);
    }

    fn bios_cursor_shape(&mut self, cx: u16) -> (u16, u8, u8) {
        let request_start = ((cx >> 8) as u8) & 0x3F;
        let request_end = (cx as u8) & 0x1F;
        let bda_shape = (u16::from(request_start) << 8) | u16::from(request_end);
        let mut hardware_start = request_start;
        let mut hardware_end = request_end;
        let mode_set_control = self.read_physical_u8(0x489);
        let char_height = self.read_guest_word(0x485);

        if mode_set_control & 0x01 != 0
            && char_height > 8
            && request_end < 8
            && request_start < 0x20
        {
            let scaled_end = ((u16::from(request_end) + 1) * char_height / 8).saturating_sub(1);
            let scaled_start = if u16::from(request_end) != u16::from(request_start) + 1 {
                ((u16::from(request_start) + 1) * char_height / 8).saturating_sub(1)
            } else {
                ((u16::from(request_end) + 1) * char_height / 8).saturating_sub(2)
            };
            hardware_start = scaled_start as u8;
            hardware_end = scaled_end as u8;
        }

        (bda_shape, hardware_start, hardware_end)
    }

    /// INT 10h AH=0Ch WRITE GRAPHICS PIXEL. AL = colour (bit 7 XORs in CGA/EGA
    /// packed-pixel modes), CX = column, DX = row. Mode 13h stores the full byte;
    /// CGA modes write packed raw pixel values into B800's interleaved framebuffer.
    /// EGA/VGA planar modes write the 4-bit colour into the four planes.
    fn int10_write_pixel(&mut self, al: u8) {
        let col = self.cpu.registers.ecx() as u16;
        let row = self.cpu.registers.edx() as u16;
        let page = ((self.cpu.registers.ebx() as u16) >> 8) as u8;
        match self.video.active_mode() {
            VideoMode::Mode13h => {
                let offset = usize::from(row) * 320 + usize::from(col);
                if offset < 320 * 200 {
                    // Mode 13h is a 256-color mode: AL is the full 8-bit pixel
                    // value, bit 7 included, with no XOR.
                    self.video.cpu_write_chain4(offset, al);
                }
            }
            VideoMode::Cga => {
                let _ = self
                    .video
                    .cga_write_pixel(col, row, al & 0x7F, al & 0x80 != 0);
            }
            VideoMode::Planar => {
                let mode = self.read_physical_u8(0x449);
                let start = self
                    .ega_graphics_page_start(mode, page)
                    .map(|(_, start)| start)
                    .unwrap_or(0);
                let _ =
                    self.video
                        .planar_write_pixel_at(start, col, row, al & 0x0F, al & 0x80 != 0);
            }
            _ => {}
        }
    }

    /// INT 10h AH=0Dh READ GRAPHICS PIXEL. CX = column, DX = row; returns AL = the
    /// pixel colour at `row*320 + col`. CGA modes return the raw packed pixel
    /// value (0..3 or 0..1), not the resolved DAC index. Planar modes return the
    /// four plane bits as a 0..15 colour.
    fn int10_read_pixel(&mut self) {
        let col = self.cpu.registers.ecx() as u16;
        let row = self.cpu.registers.edx() as u16;
        let page = ((self.cpu.registers.ebx() as u16) >> 8) as u8;
        let color = match self.video.active_mode() {
            VideoMode::Mode13h => {
                let offset = usize::from(row) * 320 + usize::from(col);
                if offset < 320 * 200 {
                    self.video.cpu_read_chain4(offset)
                } else {
                    0
                }
            }
            VideoMode::Cga => self.video.cga_read_pixel(col, row),
            VideoMode::Planar => {
                let mode = self.read_physical_u8(0x449);
                let start = self
                    .ega_graphics_page_start(mode, page)
                    .map(|(_, start)| start)
                    .unwrap_or(0);
                self.video.planar_read_pixel_at(start, col, row)
            }
            _ => 0,
        };
        self.set_eax_al(color);
    }

    /// INT 10h AH=04h READ LIGHT PEN POSITION. CGA-compatible only; VGA BIOSes
    /// report this as unsupported by leaving the trigger flag clear.
    fn int10_read_light_pen(&mut self) {
        let Some((pixel_col, pixel_row, char_row, char_col)) = self.video.cga_light_pen_report()
        else {
            self.set_eax_ah(0);
            return;
        };
        self.set_eax_ah(1);
        self.set_bx(pixel_col);
        self.set_cx(u16::from(pixel_row) << 8);
        self.set_dx((u16::from(char_row) << 8) | u16::from(char_col));
    }

    /// INT 10h AH=13h WRITE STRING. AL = write mode (bit 0 advance cursor, bit 1
    /// the source carries interleaved attribute bytes), BH = page, BL =
    /// attribute/color when bit 1 is clear, CX = character count, DH/DL = start
    /// row/col, ES:BP = the string. Text and EGA graphics modes write the
    /// requested page; CGA graphics remains single-page.
    /// The cursor is left at the end only when AL bit 0 is set.
    fn int10_write_string(&mut self) {
        let al = self.cpu.registers.eax() as u8;
        let bx = self.cpu.registers.ebx() as u16;
        let page = self.normalize_bios_page((bx >> 8) as u8);
        let bl = bx as u8;
        let count = self.cpu.registers.ecx() as u16;
        let dx = self.cpu.registers.edx() as u16;
        let mut row = usize::from((dx >> 8) as u8);
        let mut col = usize::from(dx as u8);
        let es = self.cpu.registers.segment(SegmentIndex::Es).base;
        let bp = self.cpu.registers.ebp() as u16;
        let mut src = es.wrapping_add(u32::from(bp));
        let with_attr = al & 0x02 != 0;
        let columns = self.text_columns();
        let rows = self.text_rows();
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
                    if row < rows && col < columns {
                        self.write_bios_char_cell(page, row, col, ch, attr);
                    }
                    col += 1;
                    if col >= columns {
                        col = 0;
                        row += 1;
                    }
                }
            }
            while row >= rows {
                self.scroll_text_up(page);
                row -= 1;
            }
        }
        // AL bit 0: leave the cursor at the end of the string; otherwise the caller
        // keeps its prior cursor (the BDA cursor is untouched).
        if al & 0x01 != 0 {
            let row = row.min(rows - 1) as u16;
            let col = col.min(columns - 1) as u16;
            self.set_cursor_pos(page, (row << 8) | col);
        }
    }

    fn int10_state_size_bytes(cx: u16) -> usize {
        let mut size = 0;
        if cx & 0x0001 != 0 {
            size += INT10_STATE_HARDWARE_LEN;
        }
        if cx & 0x0002 != 0 {
            size += INT10_STATE_BDA_LEN;
        }
        if cx & 0x0004 != 0 {
            size += INT10_STATE_DAC_LEN;
        }
        size
    }

    fn int10_state_size_blocks(cx: u16) -> u16 {
        let size = Self::int10_state_size_bytes(cx);
        size.div_ceil(64) as u16
    }

    fn save_video_hardware_state(&mut self, dst: u32) {
        let crtc_addr = self.read_guest_word(0x463);
        let crtc_addr: u16 = if crtc_addr == 0x03B4 { 0x03B4 } else { 0x03D4 };
        let mut block = Vec::with_capacity(INT10_STATE_HARDWARE_LEN);

        block.push(self.video.read_port(0x3C4).unwrap_or(0));
        block.push(self.video.crtc_index_latch());
        block.push(self.video.read_port(0x3CE).unwrap_or(0));
        self.video.read_status1();
        block.push(self.video.read_port(0x3C0).unwrap_or(0x20));
        block.push(self.video.read_port(0x3CA).unwrap_or(0));

        for index in 1..=4 {
            let _ = self.video.write_port(0x3C4, index);
            block.push(self.video.read_port(0x3C5).unwrap_or(0));
        }
        let _ = self.video.write_port(0x3C4, 0);
        block.push(self.video.read_port(0x3C5).unwrap_or(0));

        for index in 0..=0x18 {
            block.push(self.video.crtc_register_latch(index));
        }

        let ar_index = block[3];
        for index in 0..=0x13 {
            self.video.read_status1();
            let _ = self.video.write_port(0x3C0, index | (ar_index & 0x20));
            block.push(self.video.read_port(0x3C1).unwrap_or(0));
        }
        self.video.read_status1();

        for index in 0..=0x08 {
            let _ = self.video.write_port(0x3CE, index);
            block.push(self.video.read_port(0x3CF).unwrap_or(0));
        }

        block.extend_from_slice(&crtc_addr.to_le_bytes());
        if self.video.is_cga_personality() {
            block.extend_from_slice(&INT10_STATE_CGA_LATCH_MARKER);
            block.push(self.video.cga_mode_control());
            block.push(self.video.cga_color_select());
        } else {
            block.extend_from_slice(&[0; 4]); // VGA latches are not CPU-readable.
        }
        debug_assert_eq!(block.len(), INT10_STATE_HARDWARE_LEN);
        self.write_guest_block(dst, &block);
    }

    fn restore_video_hardware_state(&mut self, src: u32) {
        let block = self.read_guest_block(src, INT10_STATE_HARDWARE_LEN);
        if block.len() != INT10_STATE_HARDWARE_LEN {
            return;
        }
        let crtc_addr = u16::from_le_bytes([block[0x40], block[0x41]]);
        let crtc_addr = if crtc_addr == 0x03B4 { 0x03B4 } else { 0x03D4 };
        let misc = self.video.read_port(0x3CC).unwrap_or(0x67);
        let misc = (misc & !0x01) | u8::from(crtc_addr == 0x03D4);
        let _ = self.video.write_port(0x3C2, misc);
        if block[INT10_STATE_CGA_LATCH_OFFSET..INT10_STATE_CGA_LATCH_OFFSET + 2]
            == INT10_STATE_CGA_LATCH_MARKER
        {
            let _ = self
                .video
                .write_port(0x3D8, block[INT10_STATE_CGA_LATCH_OFFSET + 2]);
            let _ = self
                .video
                .write_port(0x3D9, block[INT10_STATE_CGA_LATCH_OFFSET + 3]);
        }

        let mut offset = 5;
        for index in 1..=4 {
            let _ = self.video.write_port(0x3C4, index);
            let _ = self.video.write_port(0x3C5, block[offset]);
            offset += 1;
        }
        let _ = self.video.write_port(0x3C4, 0);
        let _ = self.video.write_port(0x3C5, block[offset]);
        offset += 1;

        let _ = self.video.write_port(crtc_addr, 0x11);
        let _ = self.video.write_port(crtc_addr + 1, 0x00);
        for index in 0..=0x18 {
            let value = block[offset + index as usize];
            if index != 0x11 {
                let _ = self.video.write_port(crtc_addr, index);
                let _ = self.video.write_port(crtc_addr + 1, value);
            }
        }
        let crtc_offset = offset;
        offset += 0x19;
        let _ = self.video.write_port(crtc_addr, 0x11);
        let _ = self
            .video
            .write_port(crtc_addr + 1, block[crtc_offset + 0x11]);

        let ar_index = block[3];
        for index in 0..=0x13 {
            self.video.read_status1();
            let _ = self.video.write_port(0x3C0, index | (ar_index & 0x20));
            let _ = self.video.write_port(0x3C0, block[offset]);
            offset += 1;
        }
        self.video.read_status1();
        let _ = self.video.write_port(0x3C0, ar_index);
        self.video.read_status1();

        for index in 0..=0x08 {
            let _ = self.video.write_port(0x3CE, index);
            let _ = self.video.write_port(0x3CF, block[offset]);
            offset += 1;
        }

        let _ = self.video.write_port(0x3C4, block[0]);
        let _ = self.video.write_port(crtc_addr, block[1]);
        let _ = self.video.write_port(0x3CE, block[2]);
        let _ = self.video.write_port(crtc_addr - 4 + 0x0A, block[4]);
    }

    fn save_video_dac_state(&mut self, dst: u32) {
        let mut block = Vec::with_capacity(INT10_STATE_DAC_LEN);
        block.push(self.video.read_port(0x3C7).unwrap_or(0));
        block.push(self.video.read_port(0x3C8).unwrap_or(0));
        block.push(self.video.read_port(0x3C6).unwrap_or(0xFF));
        block.extend(self.video.dac_block_bytes(0, 256));
        block.push(self.video.attr_register(0x14));
        debug_assert_eq!(block.len(), INT10_STATE_DAC_LEN);
        self.write_guest_block(dst, &block);
    }

    fn restore_video_dac_state(&mut self, src: u32) {
        let block = self.read_guest_block(src, INT10_STATE_DAC_LEN);
        if block.len() != INT10_STATE_DAC_LEN {
            return;
        }
        let _ = self.video.write_port(0x3C6, block[2]);
        let grayscale = self.video.grayscale_summing_enabled();
        self.video.set_grayscale_summing_enabled(false);
        for index in 0..=255usize {
            let base = 3 + index * 3;
            self.video
                .set_dac_entry(index as u8, block[base], block[base + 1], block[base + 2]);
        }
        self.video.set_grayscale_summing_enabled(grayscale);
        self.video
            .set_attr_register(0x14, block[INT10_STATE_DAC_LEN - 1]);
        let _ = self.video.write_port(0x3C8, block[1]);
    }

    /// INT 10h AH=1Ch SAVE/RESTORE VIDEO STATE. AL=00 returns the buffer size in
    /// 64-byte blocks (BX), AL=01 saves the requested state into ES:BX, AL=02 restores
    /// it. CX is the requested-state bitmap: bit 0 hardware registers, bit 1 BDA,
    /// bit 2 DAC/palette.
    fn int10_save_restore_state(&mut self, al: u8) {
        const BDA_VIDEO_START: u32 = 0x449;
        match al {
            0x00 => {
                let cx = self.cpu.registers.ecx() as u16;
                self.set_bx(Self::int10_state_size_blocks(cx));
                self.set_eax_al(0x1c);
                self.set_int_frame_carry(false);
            }
            0x01 => {
                let cx = self.cpu.registers.ecx() as u16;
                let es = self.cpu.registers.segment(SegmentIndex::Es).base;
                let bx = self.cpu.registers.ebx() as u16;
                let mut dst = es.wrapping_add(u32::from(bx));
                if cx & 0x0001 != 0 {
                    self.save_video_hardware_state(dst);
                    dst = dst.wrapping_add(INT10_STATE_HARDWARE_LEN as u32);
                }
                if cx & 0x0002 != 0 {
                    let block = self.read_guest_block(BDA_VIDEO_START, INT10_STATE_BDA_LEN);
                    self.write_guest_block(dst, &block);
                    dst = dst.wrapping_add(INT10_STATE_BDA_LEN as u32);
                }
                if cx & 0x0004 != 0 {
                    self.save_video_dac_state(dst);
                }
                self.set_eax_al(0x1c);
                self.set_int_frame_carry(false);
            }
            0x02 => {
                let cx = self.cpu.registers.ecx() as u16;
                let es = self.cpu.registers.segment(SegmentIndex::Es).base;
                let bx = self.cpu.registers.ebx() as u16;
                let mut from = es.wrapping_add(u32::from(bx));
                if cx & 0x0001 != 0 {
                    self.restore_video_hardware_state(from);
                    from = from.wrapping_add(INT10_STATE_HARDWARE_LEN as u32);
                }
                if cx & 0x0002 != 0 {
                    let block = self.read_guest_block(from, INT10_STATE_BDA_LEN);
                    self.write_guest_block(BDA_VIDEO_START, &block);
                    from = from.wrapping_add(INT10_STATE_BDA_LEN as u32);
                }
                if cx & 0x0004 != 0 {
                    self.restore_video_dac_state(from);
                }
                self.set_eax_al(0x1c);
                self.set_int_frame_carry(false);
            }
            _ => self.set_int_frame_carry(true),
        }
    }

    /// INT 10h text-mode output and cursor services. Text and EGA graphics modes
    /// use BH/page-aware BDA cursor slots; CGA graphics remains single-page.
    fn handle_int10_text(&mut self, ah: u8) {
        let ax = self.cpu.registers.eax() as u16;
        let al = ax as u8;
        let bx = self.cpu.registers.ebx() as u16;
        let page = self.normalize_bios_page((bx >> 8) as u8);
        let bl = bx as u8;
        let cx = self.cpu.registers.ecx() as u16;
        let dx = self.cpu.registers.edx() as u16;
        let dl = dx as u8;
        let dh = (dx >> 8) as u8;
        let columns = self.text_columns();
        let rows = self.text_rows();
        match ah {
            // AH=01h set cursor shape: store the BIOS request in the BDA and
            // program the modeled CRTC cursor shape, including VGA cursor
            // emulation scaling for legacy 8-scanline requests.
            0x01 => {
                let (bda_shape, start, end) = self.bios_cursor_shape(cx);
                let _ = self.memory.write_u16(0x460, bda_shape);
                self.video.set_cursor_shape(start, end);
            }
            // AH=02h set cursor position: DH=row, DL=col.
            0x02 => {
                self.set_cursor_pos(page, (u16::from(dh) << 8) | u16::from(dl));
            }
            // AH=03h get cursor position and shape.
            0x03 => {
                let pos = self.cursor_pos(page);
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
                let pos = self.cursor_pos(page);
                let row = usize::from(pos >> 8);
                let col = usize::from(pos & 0xff);
                let (ch, at) = if self.is_bios_graphics_text_mode() {
                    self.read_graphics_char(page, row, col)
                } else {
                    let off = self.text_offset(page, row, col);
                    (
                        self.video.read_u8(off).unwrap_or(b' '),
                        self.video.read_u8(off + 1).unwrap_or(0x07),
                    )
                };
                let eax =
                    (self.cpu.registers.eax() & !0xFFFF) | (u32::from(at) << 8) | u32::from(ch);
                self.cpu.registers.set_eax(eax);
            }
            // AH=09h write char+attr, AH=0Ah write char only, CX times, no advance.
            0x09 | 0x0A => {
                let pos = self.cursor_pos(page);
                let row = usize::from(pos >> 8);
                let col = usize::from(pos & 0xff);
                for i in 0..usize::from(cx) {
                    let target_col = col + i;
                    if row >= rows || target_col >= columns {
                        break;
                    }
                    if self.is_bios_graphics_text_mode() {
                        self.draw_graphics_char(page, row, target_col, al, bl);
                    } else {
                        let off = self.text_offset(page, row, target_col);
                        let _ = self.video.write_u8(off, al);
                        if ah == 0x09 {
                            let _ = self.video.write_u8(off + 1, bl);
                        }
                    }
                }
            }
            // AH=0Eh teletype.
            0x0E => self.teletype_char_attr(al, bl, page),
            _ => {}
        }
    }

    fn write_bios_char_cell(&mut self, page: u8, row: usize, col: usize, ch: u8, attr: u8) {
        if self.is_bios_graphics_text_mode() {
            self.draw_graphics_char(page, row, col, ch, attr);
        } else {
            let off = self.text_offset(page, row, col);
            let _ = self.video.write_u8(off, ch);
            let _ = self.video.write_u8(off + 1, attr);
        }
    }

    fn is_bios_graphics_text_mode(&self) -> bool {
        matches!(self.video.active_mode(), VideoMode::Cga | VideoMode::Planar)
    }

    fn graphics_text_cell_height(&mut self) -> usize {
        match self.video.active_mode() {
            VideoMode::Cga => 8,
            VideoMode::Planar => usize::from(self.read_physical_u8(0x485)).clamp(1, 32),
            _ => 16,
        }
    }

    fn graphics_page_start(&mut self, page: u8) -> u32 {
        if self.video.active_mode() != VideoMode::Planar {
            return 0;
        }
        let mode = self.read_physical_u8(0x449);
        self.ega_graphics_page_start(mode, page)
            .map(|(_, start)| start)
            .unwrap_or(0)
    }

    fn graphics_write_pixel(&mut self, page: u8, x: u16, y: u16, color: u8, xor: bool) -> bool {
        match self.video.active_mode() {
            VideoMode::Cga => self.video.cga_write_pixel(x, y, color, xor),
            VideoMode::Planar => {
                let start = self.graphics_page_start(page);
                self.video.planar_write_pixel_at(start, x, y, color, xor)
            }
            _ => false,
        }
    }

    fn graphics_read_pixel(&mut self, page: u8, x: u16, y: u16) -> u8 {
        match self.video.active_mode() {
            VideoMode::Cga => self.video.cga_read_pixel(x, y),
            VideoMode::Planar => {
                let start = self.graphics_page_start(page);
                self.video.planar_read_pixel_at(start, x, y)
            }
            _ => 0,
        }
    }

    fn draw_graphics_char(&mut self, page: u8, row: usize, col: usize, ch: u8, color: u8) {
        let x0 = col * 8;
        let cell_height = self.graphics_text_cell_height();
        let y0 = row * cell_height;
        let xor = color & 0x80 != 0;
        let fg = match self.video.active_mode() {
            VideoMode::Cga => color & 0x7F,
            VideoMode::Planar => color & 0x0F,
            _ => color,
        };
        for gy in 0..cell_height {
            let bits = self.graphics_glyph_row(ch, gy);
            for gx in 0..8usize {
                let lit = bits & (0x80 >> gx) != 0;
                if xor {
                    if lit {
                        let _ = self.graphics_write_pixel(
                            page,
                            (x0 + gx) as u16,
                            (y0 + gy) as u16,
                            fg,
                            true,
                        );
                    }
                } else {
                    let _ = self.graphics_write_pixel(
                        page,
                        (x0 + gx) as u16,
                        (y0 + gy) as u16,
                        if lit { fg } else { 0 },
                        false,
                    );
                }
            }
        }
    }

    fn graphics_glyph_row(&mut self, ch: u8, row: usize) -> u8 {
        if self.video.active_mode() != VideoMode::Cga || ch < 0x80 {
            return self.video.active_font_glyph_row(ch, row);
        }
        let offset = self.read_guest_word(0x1F * 4);
        let segment = self.read_guest_word(0x1F * 4 + 2);
        if offset == 0 && segment == 0 {
            return self.video.active_font_glyph_row(ch, row);
        }
        let base = u32::from(segment) * 16 + u32::from(offset);
        self.read_physical_u8(base + u32::from(ch - 0x80) * 8 + row.min(7) as u32)
    }

    fn read_graphics_char(&mut self, page: u8, row: usize, col: usize) -> (u8, u8) {
        let x0 = col * 8;
        let cell_height = self.graphics_text_cell_height();
        let y0 = row * cell_height;
        if row >= self.text_rows() || x0 + 8 > self.video.raster_width() as usize {
            return (0, 0);
        }
        let max_fg = match self.video.active_mode() {
            VideoMode::Cga if self.video.raster_width() >= 640 => 1,
            VideoMode::Cga => 3,
            VideoMode::Planar => 15,
            _ => 0,
        };
        for fg in 1..=max_fg {
            let present = (0..cell_height).any(|gy| {
                (0..8usize).any(|gx| {
                    self.graphics_read_pixel(page, (x0 + gx) as u16, (y0 + gy) as u16) == fg
                })
            });
            if !present {
                continue;
            }
            for ch in 0..=u8::MAX {
                let mut matches = true;
                for gy in 0..cell_height {
                    let mut row_bits = 0u8;
                    for gx in 0..8usize {
                        if self.graphics_read_pixel(page, (x0 + gx) as u16, (y0 + gy) as u16) == fg
                        {
                            row_bits |= 0x80 >> gx;
                        }
                    }
                    if row_bits != self.graphics_glyph_row(ch, gy) {
                        matches = false;
                        break;
                    }
                }
                if matches {
                    return (ch, fg);
                }
            }
        }
        (b' ', 0)
    }

    /// Scroll a text window. `up` selects direction; `lines`==0 blanks the whole
    /// window. `attr` fills the vacated rows; `cx`=top-left (CH row, CL col),
    /// `dx`=bottom-right (DH row, DL col). Clamped to the active text screen.
    fn scroll_window(&mut self, up: bool, lines: u8, attr: u16, cx: u16, dx: u16) {
        let attr = attr as u8;
        let page = self.active_bios_page();
        let columns = self.text_columns();
        let rows = self.text_rows();
        let top = usize::from((cx >> 8) as u8).min(rows - 1);
        let left = usize::from(cx as u8).min(columns - 1);
        let bottom = usize::from((dx >> 8) as u8).min(rows - 1).max(top);
        let right = usize::from(dx as u8).min(columns - 1).max(left);
        let height = bottom - top + 1;
        let n = if lines == 0 {
            height
        } else {
            usize::from(lines)
        };
        if self.is_bios_graphics_text_mode() {
            self.scroll_graphics_window(page, up, n, attr, top, left, bottom, right);
            return;
        }
        if n >= height {
            for row in top..=bottom {
                self.blank_text_row(page, row, left, right, attr);
            }
            return;
        }
        if up {
            for row in top..=(bottom - n) {
                self.copy_text_row(page, row + n, row, left, right, attr);
            }
            for row in (bottom - n + 1)..=bottom {
                self.blank_text_row(page, row, left, right, attr);
            }
        } else {
            for row in ((top + n)..=bottom).rev() {
                self.copy_text_row(page, row - n, row, left, right, attr);
            }
            for row in top..(top + n) {
                self.blank_text_row(page, row, left, right, attr);
            }
        }
    }

    /// Copy a span of text cells from `src_row` to `dst_row` (inclusive columns).
    fn copy_text_row(
        &mut self,
        page: u8,
        src_row: usize,
        dst_row: usize,
        left: usize,
        right: usize,
        attr: u8,
    ) {
        for col in left..=right {
            let src = self.text_offset(page, src_row, col);
            let dst = self.text_offset(page, dst_row, col);
            let b0 = self.video.read_u8(src).unwrap_or(b' ');
            let b1 = self.video.read_u8(src + 1).unwrap_or(attr);
            let _ = self.video.write_u8(dst, b0);
            let _ = self.video.write_u8(dst + 1, b1);
        }
    }

    /// Blank a span of text cells to spaces with `attr` (inclusive columns).
    fn blank_text_row(&mut self, page: u8, row: usize, left: usize, right: usize, attr: u8) {
        for col in left..=right {
            let off = self.text_offset(page, row, col);
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

    /// Service INT 14h over the COM1 UART. DX selects the serial port; only COM1
    /// (DX=0) is wired. The BIOS functions cover AH=00h-05h, and the FOSSIL calls
    /// use the same instant-drain UART plus the BIOS text cursor and keyboard ring.
    fn handle_int14(&mut self) {
        const COM1: u16 = 0x03f8;
        let ax = self.cpu.registers.eax() as u16;
        let ah = (ax >> 8) as u8;
        let al = ax as u8;
        let bx = self.cpu.registers.ebx() as u16;

        match ah {
            0x07 => {
                self.set_ax(0x1208); // INT 08h, about 18 ticks per second.
                self.set_dx(55);
                return;
            }
            0x0D | 0x0E => {
                self.int14_fossil_keyboard_read();
                return;
            }
            0x11 => {
                self.handle_int10_text(0x02);
                return;
            }
            0x12 => {
                self.handle_int10_text(0x03);
                return;
            }
            0x13 | 0x15 => {
                self.teletype_char(al);
                return;
            }
            0x16 => {
                self.set_ax(0x0001);
                return;
            }
            0x17 => return,
            0x7E | 0x7F => {
                self.set_ax(0x1954);
                self.set_bx((bx & 0xff00) | u16::from(al));
                self.set_dx(self.cpu.registers.edx() as u16 & 0x00ff);
                return;
            }
            _ => {}
        }

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
            0x04 if bx == 0x4F50 => {
                let mcr = self.serial.read_port(COM1 + 4).unwrap_or(0) | 0x01;
                self.serial.write_port(COM1 + 4, mcr);
                self.set_ax(0x1954);
                self.set_bx(0x001B);
            }
            0x04 if self.int14_extended_params_valid() => {
                self.uart_extended_init();
                let lsr = self.serial.read_port(COM1 + 5).unwrap_or(0);
                let msr = self.serial.read_port(COM1 + 6).unwrap_or(0);
                self.set_eax_ah(lsr);
                self.set_eax_al(msr);
            }
            0x05 => match al {
                0x00 => {
                    let mcr = self.serial.read_port(COM1 + 4).unwrap_or(0);
                    self.set_bx((self.cpu.registers.ebx() as u16 & 0xff00) | u16::from(mcr));
                    self.set_eax_ah(0x00);
                }
                0x01 => {
                    self.serial
                        .write_port(COM1 + 4, self.cpu.registers.ebx() as u8);
                    let lsr = self.serial.read_port(COM1 + 5).unwrap_or(0);
                    let msr = self.serial.read_port(COM1 + 6).unwrap_or(0);
                    self.set_eax_ah(lsr);
                    self.set_eax_al(msr);
                }
                _ => self.set_eax_ah(0x80),
            },
            0x06 => {
                let mcr = self.serial.read_port(COM1 + 4).unwrap_or(0);
                let mcr = if al == 0 { mcr & !0x01 } else { mcr | 0x01 };
                self.serial.write_port(COM1 + 4, mcr);
            }
            0x08 | 0x09 | 0x0F | 0x10 | 0x14 | 0x1A => {}
            0x0A => {
                while self.serial.read_port(COM1 + 5).unwrap_or(0) & 0x01 != 0 {
                    let _ = self.serial.read_port(COM1);
                }
            }
            0x0B => {
                self.serial.write_port(COM1, al);
                self.set_ax(0x0001);
            }
            0x18 => self.int14_fossil_read_block(),
            0x19 => self.int14_fossil_write_block(),
            0x1B => self.int14_fossil_driver_info(),
            _ => self.set_eax_ah(0x80),
        }
    }

    fn int14_fossil_keyboard_read(&mut self) {
        const KBD_BDA_BASE: usize = 0x400;
        const KBD_HEAD: usize = 0x1a;
        const KBD_TAIL: usize = 0x1c;
        const KBD_RING_START: u16 = 0x1e;
        const KBD_RING_END: u16 = 0x3e;

        let head = self.memory.read_u16(KBD_BDA_BASE + KBD_HEAD).unwrap_or(0);
        let tail = self.memory.read_u16(KBD_BDA_BASE + KBD_TAIL).unwrap_or(0);
        if head == tail {
            self.set_ax(0xFFFF);
            return;
        }
        let word = self
            .memory
            .read_u16(KBD_BDA_BASE + usize::from(head))
            .unwrap_or(0);
        let mut next = head + 2;
        if next >= KBD_RING_END {
            next = KBD_RING_START;
        }
        let _ = self.memory.write_u16(KBD_BDA_BASE + KBD_HEAD, next);
        self.set_ax(word);
    }

    fn int14_fossil_read_block(&mut self) {
        const COM1: u16 = 0x03f8;
        let max = self.cpu.registers.ecx() as u16;
        let es = self.cpu.registers.segment(SegmentIndex::Es).base;
        let di = self.cpu.registers.edi() as u16;
        let mut dst = es + u32::from(di);
        let mut count = 0u16;
        while count < max && self.serial.read_port(COM1 + 5).unwrap_or(0) & 0x01 != 0 {
            let byte = self.serial.read_port(COM1).unwrap_or(0);
            self.write_physical_u8(dst, byte);
            dst = dst.wrapping_add(1);
            count += 1;
        }
        self.set_ax(count);
    }

    fn int14_fossil_write_block(&mut self) {
        const COM1: u16 = 0x03f8;
        let count = self.cpu.registers.ecx() as u16;
        let es = self.cpu.registers.segment(SegmentIndex::Es).base;
        let di = self.cpu.registers.edi() as u16;
        for index in 0..count {
            let byte = self.read_physical_u8(es + u32::from(di.wrapping_add(index)));
            self.serial.write_port(COM1, byte);
        }
        self.set_ax(count);
    }

    fn int14_fossil_driver_info(&mut self) {
        let max = self.cpu.registers.ecx() as usize;
        let es = self.cpu.registers.segment(SegmentIndex::Es).base;
        let di = self.cpu.registers.edi() as u16;
        let mut info = [0u8; 21];
        let info_len = info.len();
        info[0..2].copy_from_slice(&(info_len as u16).to_le_bytes());
        info[2] = 5; // FOSSIL spec level.
        info[16] = 80;
        info[17] = 25;
        let count = max.min(info_len);
        self.write_guest_block(es + u32::from(di), &info[..count]);
        self.set_ax(count as u16);
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

    fn int14_extended_params_valid(&self) -> bool {
        let ax = self.cpu.registers.eax() as u16;
        let bx = self.cpu.registers.ebx() as u16;
        let cx = self.cpu.registers.ecx() as u16;
        let al = ax as u8;
        let bh = (bx >> 8) as u8;
        let bl = bx as u8;
        let ch = (cx >> 8) as u8;
        let cl = cx as u8;
        al <= 1 && bh <= 4 && bl <= 1 && ch <= 3 && cl <= 0x0b
    }

    /// Program the COM1 UART from the PS/2 INT 14h AH=04h extended-configuration
    /// fields: BH parity, BL stop bits, CH word length, CL baud-rate index.
    fn uart_extended_init(&mut self) {
        const COM1: u16 = 0x03f8;
        let bx = self.cpu.registers.ebx() as u16;
        let cx = self.cpu.registers.ecx() as u16;
        let parity = (bx >> 8) as u8;
        let stop = bx as u8;
        let word = (cx >> 8) as u8;
        let baud = cx as u8;
        let divisor: u16 = match baud {
            0 => 1047, // 110
            1 => 768,  // 150
            2 => 384,  // 300
            3 => 192,  // 600
            4 => 96,   // 1200
            5 => 48,   // 2400
            6 => 24,   // 4800
            7 => 12,   // 9600
            8 => 6,    // 19200
            9 => 3,    // 38400
            10 => 2,   // 57600-ish, nearest whole divisor
            _ => 1,    // 115200
        };
        let mut lcr = word & 0x03;
        if stop != 0 {
            lcr |= 0x04;
        }
        match parity {
            1 => lcr |= 0x08,               // odd
            2 => lcr |= 0x08 | 0x10,        // even
            3 => lcr |= 0x08 | 0x20,        // stick odd
            4 => lcr |= 0x08 | 0x10 | 0x20, // stick even
            _ => {}
        }
        self.serial.write_port(COM1 + 3, 0x80);
        self.serial.write_port(COM1, (divisor & 0xff) as u8);
        self.serial.write_port(COM1 + 1, (divisor >> 8) as u8);
        self.serial.write_port(COM1 + 3, lcr);
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
        let ax = self.cpu.registers.eax() as u16;
        let ah = (ax >> 8) as u8;
        let al = ax as u8;
        if matches!(
            ax,
            0x1000..=0x1025 | 0x102B..=0x102D | 0xDE00..=0xDE12
        ) {
            self.int15_report_absent_window_manager();
            return;
        }
        match ah {
            // AH=00h-03h cassette services (PC/PCjr). This profile has no cassette.
            0x00..=0x03 => {
                self.set_eax_ah(0x86);
                self.set_int_frame_carry(true);
            }
            // AH=4Fh keyboard intercept. With no resident hook, keep the scan code.
            0x4F => self.set_int_frame_carry(true),
            // AH=80h-82h OS device hooks. The default BIOS handler succeeds.
            0x80..=0x82 => {
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
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
            // AH=0Fh format-unit periodic interrupt: continue the ESDI format.
            0x0F => {
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            // TopView/DESQview, PRINT.COM, and Convertible profile/power calls.
            0x10..=0x12 | 0x20 | 0x40..=0x44 => {
                self.int15_report_absent_window_manager();
            }
            // AH=21h POST error log.
            0x21 => self.int15_post_error_log(al),
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
            // AH=83h event wait, AH=84h joystick, AH=85h SysReq hook, AH=89h
            // protected-mode switch.
            0x83 => self.int15_event_wait(al),
            0x84 => self.int15_joystick(),
            0x85 => {
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            0x89 => {
                self.set_eax_ah(0x86);
                self.set_int_frame_carry(true);
            }
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
            // AH=C3h/C4h PS/2 watchdog and POS are absent on the base profile.
            0xC3 | 0xC4 => {
                self.set_eax_ah(0x86);
                self.set_int_frame_carry(true);
            }
            _ => self.set_int_frame_carry(true),
        }
    }

    fn int15_report_absent_window_manager(&mut self) {
        self.set_bx(0x0000);
        self.set_eax_ah(0x86);
        self.set_int_frame_carry(true);
    }

    /// INT 15h AH=21h POST error log. AL=00 reads the resident log, AL=01 appends
    /// one device/error pair (BH=device, BL=error).
    fn int15_post_error_log(&mut self, al: u8) {
        match al {
            0x00 => {
                let count = self.read_physical_u8(BIOS_POST_ERROR_LOG_COUNT_ADDR);
                self.set_bx(u16::from(count.min(BIOS_POST_ERROR_LOG_MAX)));
                let seg = (BIOS_POST_ERROR_LOG_ADDR >> 4) as u16;
                let off = (BIOS_POST_ERROR_LOG_ADDR & 0xf) as u16;
                self.cpu
                    .registers
                    .set_segment(SegmentIndex::Es, SegmentRegister::real(seg));
                let edi = (self.cpu.registers.edi() & !0xFFFF) | u32::from(off);
                self.cpu.registers.set_edi(edi);
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            0x01 => {
                let count = self.read_physical_u8(BIOS_POST_ERROR_LOG_COUNT_ADDR);
                if count >= BIOS_POST_ERROR_LOG_MAX {
                    self.set_eax_ah(0x01);
                    self.set_int_frame_carry(true);
                    return;
                }
                let bx = self.cpu.registers.ebx() as u16;
                let device = (bx >> 8) as u8;
                let error = bx as u8;
                let addr = BIOS_POST_ERROR_LOG_ADDR + u32::from(count) * 2;
                let _ = self.memory.write_u8(addr as usize, error);
                let _ = self.memory.write_u8(addr as usize + 1, device);
                let _ = self
                    .memory
                    .write_u8(BIOS_POST_ERROR_LOG_COUNT_ADDR as usize, count + 1);
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            _ => {
                self.set_eax_ah(0x02);
                self.set_int_frame_carry(true);
            }
        }
    }

    /// INT 15h AH=83h event wait. The machine has no async RTC wait queue yet, so
    /// it advances the guest clock, sets the completion byte, and returns.
    fn int15_event_wait(&mut self, al: u8) {
        match al {
            0x00 => {
                let micros = (u64::from(self.cpu.registers.ecx() as u16) << 16)
                    | u64::from(self.cpu.registers.edx() as u16);
                let es = self.cpu.registers.segment(SegmentIndex::Es).base;
                let bx = self.cpu.registers.ebx() as u16;
                let addr = es.wrapping_add(u32::from(bx));
                self.stall_for(micros as f64 / 1_000_000.0);
                let byte = self.read_physical_u8(addr);
                let _ = self.memory.write_u8(addr as usize, byte | 0x80);
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            0x01 => {
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            _ => {
                self.set_eax_ah(0x86);
                self.set_int_frame_carry(true);
            }
        }
    }

    /// INT 15h AH=84h joystick BIOS support. No game port is installed, which the
    /// BIOS reports as open switches and zeroed position counters.
    fn int15_joystick(&mut self) {
        match self.cpu.registers.edx() as u16 {
            0x0000 => {
                self.set_ax(0x0000);
                self.set_int_frame_carry(false);
            }
            0x0001 => {
                self.set_ax(0x0000);
                self.set_bx(0x0000);
                self.set_cx(0x0000);
                self.set_dx(0x0000);
                self.set_int_frame_carry(false);
            }
            _ => {
                self.set_eax_ah(0x80);
                self.set_int_frame_carry(true);
            }
        }
    }

    /// INT 15h AH=C2h PS/2 pointing-device interface (RBIL INTERRUP.C). Handles the
    /// query subset a guest probes the BIOS mouse with: enable/disable (C200), reset
    /// (C201), set sample rate (C202), set resolution (C203), get device type
    /// (C204), initialize (C205), and the extended-command group (C206). The aux
    /// device is the same standard PS/2 mouse INT 33h models, so the reset reports
    /// the self-test-passed/device-id bytes a real mouse returns. C207 (set the
    /// device handler) stores the ES:BX far pointer in the EBDA and returns success;
    /// the BIOS INT 74h ISR (izbios ROM) far-calls that pointer on each completed
    /// 3-byte PS/2 packet. C208/C209 (read/write the raw device port) report
    /// function-not-supported (AH=86h, CF set).
    fn int15_c2_pointing_device(&mut self, al: u8) {
        let bh = (self.cpu.registers.ebx() as u16 >> 8) as u8;
        match al {
            // C200 enable/disable (BH=0 disable, 1 enable). Enable or disable
            // hardware aux data reporting so IRQ12 packets stream to the guest
            // INT 74h ISR. Enabling the pointing device also arms IRQ12 in the
            // 8042 command byte (a real PS/2 BIOS does both); without that, a
            // latched aux byte never raises the interrupt and the ISR never runs.
            0x00 => {
                if bh != 0 {
                    self.enable_pointing_device();
                } else {
                    // C200 disable: stop reporting and mask IRQ12. Leave the wheel
                    // mode and EBDA packet size untouched. Known ceiling: the platform
                    // drives the device to 4-byte at enable and assumes it stays. A
                    // guest that resets the aux device (0xFF) and stays 3-byte would
                    // desync the BIOS int74 ISR (still expecting 4 bytes); no consumer
                    // does this today.
                    self.keyboard.set_mouse_reporting(false);
                    self.keyboard.set_mouse_irq(false);
                }
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            // C201 reset: BH=0x00 (device id, a standard mouse), BL=0xAA (the
            // reset-complete/BAT-passed signature the device returns; drivers probe
            // for AAh here). Acknowledge with the signature.
            0x01 => {
                let ebx = (self.cpu.registers.ebx() & !0xFFFF) | 0x00AA;
                self.cpu.registers.set_ebx(ebx);
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            // C202 set sample rate (BH=rate code 0-6).
            0x02 => {
                if self.keyboard.set_mouse_sample_rate_code(bh) {
                    self.set_eax_ah(0x00);
                    self.set_int_frame_carry(false);
                } else {
                    self.set_eax_ah(0x86);
                    self.set_int_frame_carry(true);
                }
            }
            // C203 set resolution (BH=0-3): no hardware resolution is modeled, so
            // accept and ignore.
            0x03 => {
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
            // C205 initialize (BH=packet size, 3 for a standard mouse): enable
            // hardware aux reporting, arm IRQ12 in the 8042 command byte, and
            // acknowledge. The driver does a C200 enable afterwards too; both
            // leave reporting on and IRQ12 armed without re-centring.
            0x05 => {
                self.enable_pointing_device();
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
                    // resolution code 2. Status byte 3 (DL): current sample rate.
                    let ebx = (self.cpu.registers.ebx() & !0xFF) | 0x20;
                    self.cpu.registers.set_ebx(ebx);
                    let ecx = (self.cpu.registers.ecx() & !0xFF) | 0x02;
                    self.cpu.registers.set_ecx(ecx);
                    let edx = (self.cpu.registers.edx() & !0xFF)
                        | u32::from(self.keyboard.mouse_sample_rate());
                    self.cpu.registers.set_edx(edx);
                }
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            // C207 set device handler: store the ES:BX far pointer the guest is
            // installing into the EBDA (offset word then segment word) and report
            // success. ES=0:BX=0 deregisters. The producer is the BIOS INT 74h ISR
            // in the izbios ROM: it assembles each 3-byte PS/2 packet and far-calls
            // this pointer with the standard 4-word frame. C208/C209 (the raw
            // device-port read/write) stay unsupported.
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

    /// Enable the pointing device the way the INT 15h C200-enable and C205-init
    /// services do: turn on aux data reporting, arm IRQ12 in the 8042 command byte,
    /// and (since our emulated mouse always has a wheel) put it in IntelliMouse
    /// 4-byte mode. The matching EBDA packet-size byte is set to 4 so the BIOS INT
    /// 74h ISR accumulates the wheel byte and delivers it as the frame's Z word.
    fn enable_pointing_device(&mut self) {
        self.keyboard.set_mouse_reporting(true);
        self.keyboard.set_mouse_irq(true);
        self.keyboard.enable_mouse_wheel();
        // Tell the BIOS ISR to assemble 4-byte packets. Same EBDA-base computation
        // the C207 handler uses for the handler pointer, at the packet-size offset.
        let pkt_size = (u32::from(EBDA_SEGMENT) << 4) + EBDA_MOUSE_PKT_SIZE_OFF;
        self.write_guest_block(pkt_size, &[4]);
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
            // AH=09h read RTC alarm time and status. No alarm source is armed, so
            // return zero time with DL=00h (alarm not enabled).
            0x09 => {
                self.set_cx(0x0000);
                self.set_dx(0x0000);
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
            // Limit: power-management and alarm hardware are not modeled; these return
            // success without persisting state. AH=0Eh keeps the default carry since no
            // power-on alarm date is stored.
            0x06 | 0x07 | 0x08 | 0x0C | 0x0D | 0x0F => self.set_int_frame_carry(false),
            // AH=80h PCjr/Tandy sound multiplexor. A Tandy 1000SL/TL BIOS exposes
            // this as a bare IRET; the base profile keeps the caller state intact.
            0x80 => {}
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
    /// a floppy is mounted, otherwise the Katea ATA fixed disk (80h) when it carries
    /// a 0x55AA MBR signature. When neither is bootable, fall through to the INT 18h
    /// path. DL carries the drive the loaded code booted from (00h floppy, 80h fixed
    /// disk), the way a real BIOS leaves it.
    ///
    /// This mirrors the izarra-bios ROM's own INT 19h: a mounted floppy is treated
    /// as bootable and sector 0 is loaded with no 0xAA55 signature check, so a guest
    /// re-invoking INT 19h gets the same outcome the ROM gives at power-on.
    ///
    /// Limit: the floppy boot copies sector 0 and jumps; it does not retry on a
    /// read error. The fixed-disk boot loads the real MBR at LBA 0 (signature-gated)
    /// and lets it chain to the active partition — the Rust Toka-DOS HLE boot record
    /// that used to back a non-bootable C: was retired in SP-3.
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
            // The floppy's own sector-0 code is the OS now, so the HLE Toka-DOS
            // and IZEMM stand down and the disk owns the DOS interrupts through the
            // IVT. Real hardware just runs whatever sector 0 holds; this confines
            // the HLE injection to the C: boot below.
            self.booter_inert = true;
            self.set_cs_ip(0x0000, BOOT_SECTOR_ADDRESS as u16);
            return;
        }
        // Fixed disk (Katea ATA primary master): boot from LBA 0 if it carries a
        // boot signature. Unlike the floppy path, INT 13h stays intercepted so
        // Katea keeps serving disk I/O to the booted OS. DL=80h = first fixed disk.
        if let Some(sector0) = self
            .ata
            .as_ref()
            .and_then(|d| d.read_lba(0))
            .filter(|s| s[510] == 0x55 && s[511] == 0xAA)
        {
            self.write_guest_block(BOOT_SECTOR_ADDRESS as u32, &sector0[..512]);
            self.cpu.registers.set_edx(0x80);
            self.booter_inert = true;
            self.set_cs_ip(0x0000, BOOT_SECTOR_ADDRESS as u16);
            return;
        }
        // Nothing bootable (no signed floppy or ATA MBR): the Rust Toka-DOS HLE
        // boot fallback was retired in SP-3, so hand off to the diskless/no-boot
        // path exactly like the firmware's .disk_absent branch.
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

    /// Replace BH, leaving BL and the upper half intact.
    fn set_bh(&mut self, bh: u8) {
        let ebx = (self.cpu.registers.ebx() & !0xFF00) | (u32::from(bh) << 8);
        self.cpu.registers.set_ebx(ebx);
    }

    /// Replace BL, leaving BH and the upper half intact.
    fn set_bl(&mut self, bl: u8) {
        let ebx = (self.cpu.registers.ebx() & !0xFF) | u32::from(bl);
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

    fn set_cl(&mut self, cl: u8) {
        let ecx = (self.cpu.registers.ecx() & !0xFF) | u32::from(cl);
        self.cpu.registers.set_ecx(ecx);
    }

    fn handle_absent_resident_api(&mut self, vector: u8) {
        match vector {
            0x5C => {
                let ncb = self.cpu.registers.segment(SegmentIndex::Es).base
                    + (self.cpu.registers.ebx() as u16) as u32;
                self.write_physical_u8(ncb + 1, 0xFB);
                self.set_eax_al(0xFB);
            }
            0x60 => self.handle_absent_int60(),
            0x68 => self.handle_absent_int68(),
            0x6F => self.handle_absent_int6f(),
            0x7A => self.handle_absent_int7a(),
            0x86 | 0xE4 => {}
            _ => {}
        }
    }

    fn handle_absent_int60(&mut self) {
        let ah = ((self.cpu.registers.eax() as u16) >> 8) as u8;
        match ah {
            0x01 => {
                self.set_eax_al(0xFF);
                self.set_int_frame_carry(false);
            }
            0x11..=0x13 => {
                self.set_eax_al(0x07);
            }
            _ => {
                let edx = (self.cpu.registers.edx() & !0xFF00) | (0x0B << 8);
                self.cpu.registers.set_edx(edx);
                self.set_int_frame_carry(true);
            }
        }
    }

    fn handle_absent_int68(&mut self) {
        let ah = ((self.cpu.registers.eax() as u16) >> 8) as u8;
        if matches!(ah, 0x01..=0x07 | 0xFB) {
            let block = self.cpu.registers.segment(SegmentIndex::Ds).base
                + (self.cpu.registers.edx() as u16) as u32;
            self.write_guest_block(block + 0x14, &[0xF0, 0x01, 0x00, 0x00]);
        }
    }

    fn handle_absent_int6f(&mut self) {
        let ah = ((self.cpu.registers.eax() as u16) >> 8) as u8;
        match ah {
            0x03 => {
                self.set_bx(0);
                self.cpu
                    .registers
                    .set_segment(SegmentIndex::Es, SegmentRegister::real(0));
            }
            0x0B | 0x0C => {
                self.set_eax_al(0x07);
            }
            0x0D => {
                self.set_cl(0);
            }
            _ => {
                self.set_ax(0x08FF);
                self.set_int_frame_carry(true);
            }
        }
    }

    fn handle_absent_int7a(&mut self) {
        let ax = self.cpu.registers.eax() as u16;
        let bx = self.cpu.registers.ebx() as u16;
        match (ax, bx) {
            (0x0001 | 0x07D0, _) => {
                self.set_ax(0);
                self.set_bx(0);
                self.set_cx(0);
                self.set_dx(0);
            }
            (0x0200, 0) => {
                self.set_bx(0);
                self.set_cx(0);
                self.set_dx(0);
            }
            (_, 0x0010) => self.set_eax_al(0xF0),
            (_, 0x000A) => {}
            _ => self.set_eax_al(0xFF),
        }
    }

    /// Service the DOS-owned and IZCDEX functions of `INT 2Fh` (the multiplex
    /// interrupt) as HLE bridges. Unrecognized AX values fall through unchanged
    /// so other INT 2Fh consumers are unaffected. Returns true if the bridge
    /// handled the call.
    fn handle_int2f(&mut self) -> bool {
        let ax = self.cpu.registers.eax() as u16;
        match ax {
            // DOS-owned install checks for resident utilities we do not load.
            // Report "not installed, OK to install" rather than falling through
            // with stale register contents.
            0x0100 | 0x0500 | 0x1000 | 0x1400 | 0x6400 | 0x7A00 | 0xAA00 | 0xAD00 | 0xB000
            | 0xF700 => {
                self.set_eax_al(0x00);
                true
            }
            0x2300 | 0x2E00 => {
                self.set_eax_ah(0x00);
                true
            }
            0xB700 => {
                self.set_ax(0x0000);
                true
            }
            0xB800 => {
                self.set_eax_ah(0x00);
                self.set_bx(0x0000);
                true
            }
            0xB803 => {
                let (seg, off) = self.network_post_address;
                self.cpu
                    .registers
                    .set_segment(SegmentIndex::Es, SegmentRegister::real(seg));
                self.set_bx(off);
                true
            }
            0xB804 => {
                self.network_post_address = (
                    self.cpu.registers.segment(SegmentIndex::Es).selector,
                    self.cpu.registers.ebx() as u16,
                );
                true
            }
            // ASSIGN is absent. DOSINTS documents AH=0 for not installed, while
            // RBIL documents AL=0; clear AX to satisfy both probes. AX=0601h would
            // return ASSIGN's work area if installed, so return a null segment.
            0x0600 => {
                self.set_ax(0x0000);
                true
            }
            0x0601 => {
                self.cpu
                    .registers
                    .set_segment(SegmentIndex::Es, SegmentRegister::real(0));
                true
            }
            // PRINT is not resident. Its service calls report a normal DOS invalid
            // function error instead of falling through with whatever CF/AX the
            // caller happened to have.
            0x0101..=0x0105 => {
                self.set_ax(0x0001);
                self.set_int_frame_carry(true);
                true
            }
            0x1401..=0x1404 | 0x14FE | 0x14FF => {
                self.set_eax_al(0x01);
                true
            }
            0xB001 => {
                self.set_eax_al(0x00);
                true
            }
            0xB701 | 0xB702 | 0xB809 | 0xF701 => {
                self.set_ax(0x0001);
                self.set_int_frame_carry(true);
                true
            }
            ax if ax & 0xFF00 == 0x2300 || ax & 0xFF00 == 0x2E00 => {
                self.set_eax_ah(0x00);
                true
            }
            // Redirector/IFSFUNC calls. Toka-DOS does not load a network
            // redirector or installable filesystem helper here. Hooks that only
            // notify the redirector are no-ops; unsupported remote operations fail
            // with the documented invalid-function result instead of leaking stale
            // caller state.
            0x111D | 0x1122 => true,
            0x1120 => {
                self.set_int_frame_carry(false);
                true
            }
            0x1101..=0x111C | 0x111E | 0x111F | 0x1121 | 0x1123..=0x112F => {
                self.set_ax(0x0001);
                self.set_int_frame_carry(true);
                true
            }
            // Critical-error helper: no resident message override is installed, so
            // expansion requests fail instead of falling through with stale flags.
            ax if ax & 0xFF00 == 0x0500 => {
                self.set_ax(0x0001);
                self.set_int_frame_carry(true);
                true
            }
            // DOS 3.2+ disk-interrupt handler hook. Return the previous handler
            // pair, then remember the caller's new DS:DX and ES:BX values.
            ax if ax & 0xFF00 == 0x1300 => {
                let new_handler = (
                    self.cpu.registers.segment(SegmentIndex::Ds).selector,
                    self.cpu.registers.edx() as u16,
                );
                let new_restore = (
                    self.cpu.registers.segment(SegmentIndex::Es).selector,
                    self.cpu.registers.ebx() as u16,
                );
                let old_handler = self.dos_disk_handler;
                let old_restore = self.dos_disk_restore;
                self.dos_disk_handler = new_handler;
                self.dos_disk_restore = new_restore;
                self.cpu
                    .registers
                    .set_segment(SegmentIndex::Ds, SegmentRegister::real(old_handler.0));
                self.set_dx(old_handler.1);
                self.cpu
                    .registers
                    .set_segment(SegmentIndex::Es, SegmentRegister::real(old_restore.0));
                self.set_bx(old_restore.1);
                true
            }
            // Network-redirector / IZCDEX installation check (RBIL INTERRUP.K,
            // INT 2F/AX=1100h). The caller pushes a DADAh marker, runs INT 2Fh,
            // and a present IZCDEX returns AL=FFh and replaces the pushed word
            // with ADADh. A strict probe checks that the word changed, so we
            // rewrite it. The INT pushed IP, CS, FLAGS over the marker, so the
            // marker sits at SS:SP+6. Without that marker this is the plain
            // network-redirector install check, and no redirector is loaded.
            0x1100 => {
                let ss = self.cpu.registers.segment(SegmentIndex::Ss).base;
                let sp = self.cpu.registers.esp() as u16;
                let marker_addr = ss + u32::from(sp.wrapping_add(6));
                if self.read_guest_word(marker_addr) == 0xDADA {
                    let _ = self.memory.write_u16(marker_addr as usize, 0xADAD);
                    self.set_eax_al(0xFF);
                } else {
                    self.set_eax_al(0x00);
                }
                true
            }
            // CD-ROM installation check: BX = number of CD drives, CX = first
            // drive letter (0 = A:).
            0x1500 => {
                // One CD drive is present if any D:..Z: letter remains after
                // CONFIG.SYS block drivers. No disc is still a present drive.
                let cd_drive = self.icdex_cd_drive_number();
                let bx = u16::from(cd_drive.is_some());
                let ebx = (self.cpu.registers.ebx() & !0xFFFF) | u32::from(bx);
                self.cpu.registers.set_ebx(ebx);
                let ecx = (self.cpu.registers.ecx() & !0xFFFF) | u32::from(cd_drive.unwrap_or(0));
                self.cpu.registers.set_ecx(ecx);
                true
            }
            // Get drive device list: ES:BX -> 5 bytes per drive (subunit + driver
            // header far pointer). We write one entry: subunit 0, a null header
            // pointer (the guest only needs the drive count/letter to map the
            // drive; the header is informational for our HLE path).
            0x1501 => {
                if self.icdex_cd_drive_number().is_some() {
                    let es = self.cpu.registers.segment(SegmentIndex::Es).base;
                    let bx = self.cpu.registers.ebx() as u16;
                    let addr = es.wrapping_add(u32::from(bx));
                    self.write_guest_block(addr, &[0u8; 5]); // subunit 0, header 0:0
                }
                true
            }
            // Metadata filenames from the ISO primary volume descriptor. Until the
            // descriptor parser grows those fields, report empty names for a valid
            // CD drive rather than leaking stale guest buffer bytes.
            0x1502..=0x1504 => {
                if !self.icdex_drive_matches(self.cpu.registers.ecx() as u16) {
                    self.icdex_fail(0x000F);
                    return true;
                }
                self.write_guest_block(self.icdex_es_bx(), &[0u8; 38]);
                self.set_int_frame_carry(false);
                true
            }
            // Read ISO volume descriptor N. VTOC index 0 maps to LBA 16.
            0x1505 => {
                if !self.icdex_drive_matches(self.cpu.registers.ecx() as u16) {
                    self.icdex_fail(0x000F);
                    return true;
                }
                let lba = 16u32.wrapping_add(u32::from(self.cpu.registers.edx() as u16));
                match self
                    .ide
                    .device()
                    .image()
                    .and_then(|img| img.read_data_sector(lba))
                {
                    Some(sector) => {
                        let descriptor_type = sector[0];
                        self.write_guest_block(self.icdex_es_bx(), &sector);
                        self.set_ax(u16::from(descriptor_type));
                        self.set_int_frame_carry(false);
                    }
                    None => self.icdex_fail(0x0015),
                }
                true
            }
            // Absolute CD data read. SI:DI is the starting LBA, DX is the sector
            // count, and ES:BX receives contiguous 2048-byte sectors.
            0x1508 => {
                if !self.icdex_drive_matches(self.cpu.registers.ecx() as u16) {
                    self.icdex_fail(0x000F);
                    return true;
                }
                let lba = ((self.cpu.registers.esi() as u16 as u32) << 16)
                    | u32::from(self.cpu.registers.edi() as u16);
                let count = self.cpu.registers.edx() as u16;
                let mut addr = self.icdex_es_bx();
                let mut ok = true;
                for sector_index in 0..u32::from(count) {
                    match self
                        .ide
                        .device()
                        .image()
                        .and_then(|img| img.read_data_sector(lba + sector_index))
                    {
                        Some(sector) => {
                            self.write_guest_block(addr, &sector);
                            addr = addr.wrapping_add(cdimage::DATA_SECTOR as u32);
                        }
                        None => {
                            ok = false;
                            break;
                        }
                    }
                }
                if ok {
                    self.cd_accesses += u64::from(count != 0);
                    self.set_int_frame_carry(false);
                } else {
                    self.icdex_fail(0x0015);
                }
                true
            }
            // Absolute write is reserved for writable optical media. No such device
            // is modeled, so report invalid function for a valid CD drive.
            0x1509 => {
                if !self.icdex_drive_matches(self.cpu.registers.ecx() as u16) {
                    self.icdex_fail(0x000F);
                } else {
                    self.icdex_fail(0x0001);
                }
                true
            }
            // Get CD-ROM drive letters: ES:BX -> one byte per drive letter, the
            // drive number (0 = A:). One CD drive.
            0x150D => {
                if let Some(cd_drive) = self.icdex_cd_drive_number() {
                    let es = self.cpu.registers.segment(SegmentIndex::Es).base;
                    let bx = self.cpu.registers.ebx() as u16;
                    let addr = es.wrapping_add(u32::from(bx));
                    self.write_guest_block(addr, &[cd_drive]);
                }
                true
            }
            // Drive check: BX = ADADh signals IZCDEX present; AX nonzero if the
            // drive in CX is a supported CD-ROM.
            0x150B => {
                let cx = self.cpu.registers.ecx() as u16;
                let supported = u16::from(
                    self.icdex_cd_drive_number()
                        .is_some_and(|drive| cx == u16::from(drive)),
                );
                let eax = (self.cpu.registers.eax() & !0xFFFF) | u32::from(supported);
                self.cpu.registers.set_eax(eax);
                let ebx = (self.cpu.registers.ebx() & !0xFFFF) | 0xADAD;
                self.cpu.registers.set_ebx(ebx);
                true
            }
            // Reserved CD-ROM debug toggles. No debugger is modeled, but accepting
            // the calls matches their no-result contract.
            0x1506 | 0x1507 => true,
            // Reserved by MSCDEX. Consume it so probes do not fall through with
            // stale interrupt state.
            0x150A => {
                self.set_int_frame_carry(false);
                true
            }
            // Get IZCDEX version: BH = major, BL = minor. Report 2.23.
            0x150C => {
                let ebx = (self.cpu.registers.ebx() & !0xFFFF) | 0x0217; // 2.23
                self.cpu.registers.set_ebx(ebx);
                true
            }
            // Get/set volume descriptor preference. Default is primary volume
            // descriptor; a caller may request supplementary descriptors.
            0x150E => {
                if !self.icdex_drive_matches(self.cpu.registers.ecx() as u16) {
                    self.icdex_fail(0x000F);
                    return true;
                }
                match self.cpu.registers.ebx() as u16 {
                    0x0000 => {
                        self.set_dx(self.icdex_vd_preference);
                        self.set_int_frame_carry(false);
                    }
                    0x0001 => {
                        let dx = self.cpu.registers.edx() as u16;
                        let primary = (dx >> 8) as u8;
                        let supplementary = dx as u8;
                        if (primary == 0x01 || primary == 0x02)
                            && (supplementary == 0x00 || supplementary == 0x01)
                        {
                            self.icdex_vd_preference = dx;
                            self.set_int_frame_carry(false);
                        } else {
                            self.icdex_fail(0x0001);
                        }
                    }
                    _ => self.icdex_fail(0x0001),
                }
                true
            }
            // Get an ISO9660 directory entry for the ASCIZ path at ES:BX. CH bit 0
            // selects MSCDEX's canonical structure; clear means a direct raw
            // directory-record copy.
            0x150F => {
                let drive = self.cpu.registers.ecx() as u8;
                if !self.icdex_drive_matches(u16::from(drive)) {
                    self.icdex_fail(0x000F);
                    return true;
                }
                let es = self.cpu.registers.segment(SegmentIndex::Es).selector;
                let bx = self.cpu.registers.ebx() as u16;
                let path = self.read_guest_asciiz_lossy(es, bx, 255);
                let copy_canonical = (self.cpu.registers.ecx() as u16 >> 8) & 1 != 0;
                let dst = (u32::from(self.cpu.registers.esi() as u16) << 4)
                    + u32::from(self.cpu.registers.edi() as u16);
                match self.icdex_iso_dir_record(&path) {
                    Ok(record) => {
                        if copy_canonical {
                            self.write_icdex_canonical_dir_record(dst, &record);
                        } else {
                            let mut out = [0u8; 255];
                            let len = usize::from(record[0]).min(record.len()).min(out.len());
                            out[..len].copy_from_slice(&record[..len]);
                            self.write_guest_block(dst, &out);
                        }
                        self.set_ax(0x0001);
                        self.set_int_frame_carry(false);
                    }
                    Err(code) => self.icdex_fail(code),
                }
                true
            }
            // Windows enhanced-mode installation check. This is a plain DOS box,
            // so neither Windows/386 nor Windows 3.x enhanced mode is active.
            0x1600 => {
                self.set_eax_al(0x00);
                true
            }
            // DPMI mode/install checks. Memory services (XMS/UMB/EMS) are the guest
            // TOKAEMM driver's, and there is no DPMI host yet, so report
            // real-mode/no-host with AX nonzero.
            0x1686 | 0x1687 => {
                self.set_ax(0x0001);
                true
            }
            // Release current VM time-slice (AX=1680h). There is no host-side
            // scheduler to yield to in the HLE, but DOS 5+ reports support by
            // clearing AL; doing so keeps idle loops from treating the function
            // as absent.
            0x1680 => {
                self.set_eax_al(0x00);
                true
            }
            // Send device driver request: ES:BX -> a CD-ROM device driver request
            // header. CX = drive number. Dispatch it to the ATAPI device.
            0x1510 => {
                let cx = self.cpu.registers.ecx() as u16;
                if self
                    .icdex_cd_drive_number()
                    .is_none_or(|drive| cx != u16::from(drive))
                {
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

    fn icdex_cd_drive_number(&self) -> Option<u8> {
        // The ATAPI CD-ROM sits at a fixed DOS drive letter (D: = 3). With the
        // Rust DOS kernel retired there are no CONFIG.SYS block-device drivers
        // that could shift it, so the CD drive is always the first loaded block
        // drive. "No disc" is still a present drive, so this reports the letter
        // unconditionally (the install check keys off the ATAPI channel existing).
        Some(CD_DRIVE_NUMBER)
    }

    fn icdex_drive_matches(&self, drive: u16) -> bool {
        self.icdex_cd_drive_number()
            .is_some_and(|cd_drive| drive == u16::from(cd_drive))
    }

    fn icdex_es_bx(&self) -> u32 {
        self.cpu.registers.segment(SegmentIndex::Es).base + (self.cpu.registers.ebx() as u16) as u32
    }

    fn icdex_fail(&mut self, code: u16) {
        self.set_ax(code);
        self.set_int_frame_carry(true);
    }

    fn icdex_iso_dir_record(&self, path: &str) -> Result<Vec<u8>, u16> {
        let image = self.ide.device().image().ok_or(0x0015u16)?;
        let pvd = image.read_data_sector(16).ok_or(0x0015u16)?;
        if pvd[0] != 0x01 || &pvd[1..6] != b"CD001" {
            return Err(0x0015);
        }
        let root_len = usize::from(pvd[156]);
        if root_len == 0 || 156 + root_len > pvd.len() {
            return Err(0x0015);
        }
        let mut record = pvd[156..156 + root_len].to_vec();
        let mut normalized = path.replace('/', "\\");
        if normalized.as_bytes().get(1) == Some(&b':') {
            normalized.drain(..2);
        }
        for component in normalized.split('\\').filter(|part| !part.is_empty()) {
            if record.get(25).copied().unwrap_or(0) & 0x02 == 0 {
                return Err(0x0002);
            }
            record = icdex_iso_child_record(image, &record, component).ok_or(0x0002u16)?;
        }
        Ok(record)
    }

    fn write_icdex_canonical_dir_record(&mut self, dst: u32, record: &[u8]) {
        let mut out = [0u8; 285];
        if record.len() < 34 {
            self.write_guest_block(dst, &out);
            return;
        }
        let lba = u32::from_le_bytes(record[2..6].try_into().unwrap());
        let len = u32::from_le_bytes(record[10..14].try_into().unwrap());
        let blocks = len
            .div_ceil(cdimage::DATA_SECTOR as u32)
            .min(u32::from(u16::MAX)) as u16;
        out[0] = record[1];
        out[1..5].copy_from_slice(&lba.to_le_bytes());
        out[5..7].copy_from_slice(&blocks.to_le_bytes());
        out[7..11].copy_from_slice(&len.to_le_bytes());
        out[0x0b..0x12].copy_from_slice(&record[18..25]);
        out[0x12] = record[25];
        out[0x13] = record[26];
        out[0x14] = record[27];
        out[0x15..0x17].copy_from_slice(&record[28..30]);
        let (name, version) = icdex_iso_name_and_version(record);
        let name_len = name.len().min(37);
        out[0x17] = name_len as u8;
        out[0x18..0x18 + name_len].copy_from_slice(&name[..name_len]);
        out[0x3e..0x40].copy_from_slice(&version.to_le_bytes());
        let name_record_len = usize::from(record[32]);
        let sys_start = 33 + name_record_len + usize::from(name_record_len % 2 == 0);
        if sys_start < record.len() {
            let sys = &record[sys_start..];
            let sys_len = sys.len().min(220);
            out[0x40] = sys_len as u8;
            out[0x41..0x41 + sys_len].copy_from_slice(&sys[..sys_len]);
        }
        self.write_guest_block(dst, &out);
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

        match ah {
            // AH=16h detect disk change, AH=17h set disk type for format, and
            // AH=18h set media type for format are meaningful even as probe calls.
            0x16 => {
                self.int13_floppy_change_status(dl);
                return;
            }
            0x17 => {
                self.int13_floppy_set_disk_type_for_format(dl);
                return;
            }
            0x18 => {
                self.int13_floppy_set_media_type_for_format(dl);
                return;
            }
            _ => {}
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
    /// sector of that track is filled with the DOS format filler 0xF6. Limit:
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

        // The ROM BIOS boot path reads A: CHS 0/0/1 to 0000:7C00 with INT 13h,
        // then far-jumps there. Once that sector is in place, a self-booting disk
        // owns DOS-family interrupts through its IVT handlers. Host-side Toka-DOS
        // and IZEMM must stand down before the sector's first INT 21h/2Fh/66h.
        if ah == 0x02
            && done > 0
            && cyl == 0
            && head == 0
            && sector == 1
            && buffer == BOOT_SECTOR_ADDRESS as u32
        {
            self.booter_inert = true;
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

    /// AH=16h detect disk change. Izarra's in-memory drive has no change line, so
    /// a mounted A: reports unchanged. An absent or non-wired drive reports not
    /// ready.
    fn int13_floppy_change_status(&mut self, dl: u8) {
        if dl == 0x00 && self.floppy.is_some() {
            self.set_eax_ah(0x00);
            self.set_disk_status(0x00);
            self.set_int_frame_carry(false);
        } else {
            self.set_eax_ah(0x80);
            self.set_disk_status(0x80);
            self.set_int_frame_carry(true);
        }
    }

    /// AH=17h set disk type for format. The mounted image fixes the media
    /// geometry, so this validates that the requested format class matches it.
    fn int13_floppy_set_disk_type_for_format(&mut self, dl: u8) {
        let al = self.cpu.registers.eax() as u8;
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

        let supported = match al {
            0x01 | 0x02 => geom.cylinders == 40 && geom.sectors <= 9,
            0x03 => geom.cylinders == 80 && geom.sectors == 15,
            0x04 => geom.cylinders == 80 && geom.sectors == 9,
            _ => false,
        };
        if supported {
            self.set_eax_ah(0x00);
            self.set_disk_status(0x00);
            self.set_int_frame_carry(false);
        } else {
            self.set_eax_ah(0x0c);
            self.set_disk_status(0x0c);
            self.set_int_frame_carry(true);
        }
    }

    /// AH=18h set media type for format. Returns ES:DI pointing at the resident
    /// diskette parameter table when the requested cylinder and sector geometry
    /// matches the mounted image.
    fn int13_floppy_set_media_type_for_format(&mut self, dl: u8) {
        let Some(geom) = self.floppy.as_ref().map(|f| f.geometry()) else {
            self.set_eax_ah(0x80);
            self.set_disk_status(0x80);
            self.set_int_frame_carry(true);
            return;
        };
        if dl != 0x00 {
            self.set_eax_ah(0x0c);
            self.set_disk_status(0x0c);
            self.set_int_frame_carry(true);
            return;
        }

        let cx = self.cpu.registers.ecx() as u16;
        let cl = cx as u8;
        let ch = (cx >> 8) as u8;
        let requested_max_cyl = u16::from(ch) | (u16::from(cl & 0xc0) << 2);
        let requested_sectors = cl & 0x3f;
        if requested_max_cyl != geom.cylinders.saturating_sub(1)
            || requested_sectors != geom.sectors
        {
            self.set_eax_ah(0x0c);
            self.set_disk_status(0x0c);
            self.set_int_frame_carry(true);
            return;
        }

        let table = BIOS_DISKETTE_PARAMETER_TABLE_ADDR;
        let seg = (table >> 4) as u16;
        let off = (table & 0x0f) as u16;
        self.cpu
            .registers
            .set_segment(SegmentIndex::Es, SegmentRegister::real(seg));
        let edi = (self.cpu.registers.edi() & !0xFFFF) | u32::from(off);
        self.cpu.registers.set_edi(edi);
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
            self.int13_hdd_error(0x01);
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
            // AH=06/07 and AH=1A are controller-level format calls. The fixed
            // disk image has no low-level format or defect table state.
            0x06 | 0x07 | 0x1A => self.int13_hdd_error(0x01),
            // AH=08 get drive parameters: CHS geometry packed into CX/DH, fixed-
            // disk count in DL.
            0x08 => self.int13_hdd_parameters(),
            // AH=09 init drive pair, AH=0C seek, AH=0D alternate reset, AH=11
            // recalibrate, AH=19 park heads: all succeed with no data movement.
            0x09 | 0x0C | 0x0D | 0x11 | 0x19 => self.int13_hdd_ok(),
            // AH=0A/0B read/write long: transfer 512 data bytes plus synthetic
            // ECC bytes per sector.
            0x0A | 0x0B => self.int13_hdd_long_transfer(ah),
            // AH=0E/0F access an XT controller sector buffer, which this ATA model
            // does not expose.
            0x0E | 0x0F => self.int13_hdd_error(0x01),
            // AH=10 test drive ready, AH=14 controller diagnostic: ready/OK.
            0x10 | 0x14 => self.int13_hdd_ok(),
            // AH=12 controller RAM diagnostic, AH=13 drive diagnostic.
            0x12 | 0x13 => self.int13_hdd_diagnostic_ok(),
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
            _ => self.int13_hdd_error(0x01),
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

    /// Common failure for a fixed-disk call.
    fn int13_hdd_error(&mut self, status: u8) {
        self.set_eax_ah(status);
        self.set_fixed_disk_status(status);
        self.set_int_frame_carry(true);
    }

    /// AH=12h/13h diagnostics return AL=0 when the controller or drive test
    /// completes successfully.
    fn int13_hdd_diagnostic_ok(&mut self) {
        self.set_eax_al(0x00);
        self.int13_hdd_ok();
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
                let data = self.ata.as_ref().and_then(|d| d.read_lba(lba));
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
        // A CHS read of LBA 0 (the MBR) to 0000:7C00 is a fixed-disk boot. Mirror
        // the INT 19h ATA branch: only a sector carrying the 55AA boot signature is
        // a real OS, so stand the HLE Toka-DOS and IZEMM down (the booted OS then
        // owns the DOS interrupts via its IVT). Without the signature the boot ROM
        // falls back to the HLE C: shim, which needs the HLE live, so leave it set.
        // Unlike the floppy, INT 13h stays intercepted so Katea keeps serving I/O.
        if ah == 0x02 && done > 0 && start_lba == 0 && buffer == BOOT_SECTOR_ADDRESS as u32 {
            let signed = self.read_physical_u8(buffer + 510) == 0x55
                && self.read_physical_u8(buffer + 511) == 0xAA;
            if signed {
                self.booter_inert = true;
            }
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

    /// AH=0Ah/0Bh read/write long. A classic BIOS moves sector data followed by
    /// controller ECC bytes. The ATA image stores only 512-byte sectors, so reads
    /// append four zero ECC bytes and writes ignore the caller's ECC trailer.
    fn int13_hdd_long_transfer(&mut self, ah: u8) {
        const LONG_SECTOR_BYTES: u32 = ata::SECTOR as u32 + 4;
        const ZERO_ECC: [u8; 4] = [0; 4];

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
            self.set_eax_al(0);
            self.int13_hdd_error(0x04);
            return;
        };

        self.c_accesses += 1;
        let mut done: u8 = 0;
        for i in 0..count {
            let lba = start_lba + u32::from(i);
            let addr = buffer.wrapping_add(u32::from(i) * LONG_SECTOR_BYTES);
            if ah == 0x0A {
                let data = self.ata.as_ref().and_then(|d| d.read_lba(lba));
                match data {
                    Some(bytes) => {
                        self.write_guest_block(addr, &bytes);
                        self.write_guest_block(addr.wrapping_add(ata::SECTOR as u32), &ZERO_ECC);
                    }
                    None => break,
                }
            } else {
                let bytes = self.read_guest_block(addr, ata::SECTOR);
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
            self.int13_hdd_error(0x04);
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
    /// honored. Limit: the 64-bit-flat-buffer form (DAP bytes 16-23 when the
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
                let data = self.ata.as_ref().and_then(|d| d.read_lba(l));
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
    /// set/get forms for the attribute palette (00/01/02/03/07/08/09) and the DAC
    /// (10/12/13/15/17/18/19/1A/1B). Register conventions per RBIL (INT 10/AH=10h).
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
            // AL=00: set individual Attribute register. BL=index, BH=value.
            0x00 => {
                self.video.set_attr_register(bl, bh);
                if self.video.is_cga_personality() {
                    self.sync_bda_cga_latches();
                }
            }
            // AL=01: set overscan/border color. BH=value (overlap with AH=0Bh).
            0x01 => {
                self.video.set_overscan(bh);
                if self.video.is_cga_personality() {
                    self.sync_bda_cga_latches();
                }
            }
            // AL=02: set all 16 palette registers and overscan from ES:DX (17 bytes).
            0x02 => {
                let block = self.read_guest_block(es_dx, 17);
                for i in 0..16u8 {
                    self.video.set_attr_palette_reg(i, block[i as usize]);
                }
                self.video.set_overscan(block[16]);
                if self.video.is_cga_personality() {
                    self.sync_bda_cga_latches();
                }
            }
            // AL=03: BL=0 enables bright backgrounds, BL=1 enables blink.
            0x03 => {
                self.video.set_text_blink_enabled(bl & 0x01 != 0);
                if self.video.is_cga_personality() {
                    self.sync_bda_cga_latches();
                }
            }
            // AL=07: get individual Attribute register. BL=index -> BH.
            0x07 => {
                let value = self.video.attr_register(bl);
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
            // AL=13: select DAC colour-page mode/page. BL=0 picks four 64-colour
            // pages (BH=0) vs sixteen 16-colour pages (BH=1); BL=1 selects page.
            0x13 => match bl {
                0x00 => {
                    let mut mode_control = self.video.attr_register(0x10);
                    if bh & 0x01 != 0 {
                        mode_control |= 0x80;
                    } else {
                        mode_control &= !0x80;
                    }
                    self.video.set_attr_register(0x10, mode_control);
                }
                0x01 => {
                    let color_select = self.video.attr_register(0x14);
                    let page = if self.video.attr_register(0x10) & 0x80 != 0 {
                        bh & 0x0F
                    } else {
                        (color_select & 0x03) | ((bh & 0x03) << 2)
                    };
                    self.video.set_attr_register(0x14, page);
                }
                _ => {}
            },
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
            // AL=18: set PEL mask. BL=value.
            0x18 => {
                let _ = self.video.write_port(0x3C6, bl);
            }
            // AL=19: read PEL mask -> BL.
            0x19 => {
                let value = self.video.read_port(0x3C6).unwrap_or(0xFF);
                let ebx = (self.cpu.registers.ebx() & !0xFF) | u32::from(value);
                self.cpu.registers.set_ebx(ebx);
            }
            // AL=1A: read DAC page state -> BL=paging mode, BH=current page.
            0x1A => {
                let mode = u8::from(self.video.attr_register(0x10) & 0x80 != 0);
                let color_select = self.video.attr_register(0x14);
                let page = if mode == 0 {
                    (color_select >> 2) & 0x03
                } else {
                    color_select & 0x0F
                };
                let ebx =
                    (self.cpu.registers.ebx() & !0xFFFF) | (u32::from(page) << 8) | u32::from(mode);
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
    /// variants also reprogram the CRTC character height. AL=20 installs the
    /// 8x8 CGA graphics-character pointer at INT 1Fh; AL=21-24 select the
    /// planar graphics-mode BIOS text font and row grid; AL=30 returns the
    /// requested font pointer (BH=00..07) plus the live BDA font height/rows.
    /// Text-font register conventions verified against the LGPL VGABios
    /// `biosfn_load_text_*`; graphics-font register conventions follow RBIL.
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
                self.set_int43_pointer(self.cpu.registers.segment(SegmentIndex::Es).selector, bp);
                if al >= 0x10 {
                    self.video.set_char_height(bh);
                }
                self.publish_int43_font_table();
            }
            0x01 | 0x11 => {
                self.video.load_rom_font(table, 14);
                self.set_int43_pointer(BIOS_ROM_SEGMENT, BIOS_FONT_8X14_ROM_OFFSET);
                if al >= 0x10 {
                    self.video.set_char_height(14);
                }
                self.publish_int43_font_table();
            }
            0x02 | 0x12 => {
                self.video.load_rom_font(table, 8);
                self.set_int43_pointer(BIOS_ROM_SEGMENT, BIOS_FONT_8X8_ROM_OFFSET);
                if al >= 0x10 {
                    self.video.set_char_height(8);
                }
                self.publish_int43_font_table();
            }
            0x04 | 0x14 => {
                self.video.load_rom_font(table, 16);
                self.set_int43_pointer(BIOS_ROM_SEGMENT, BIOS_FONT_8X16_ROM_OFFSET);
                if al >= 0x10 {
                    self.video.set_char_height(16);
                }
                self.publish_int43_font_table();
            }
            0x03 => {
                self.video.set_char_map_select(bl);
                self.publish_int43_font_table();
            }
            0x20 => {
                let es = self.cpu.registers.segment(SegmentIndex::Es).selector;
                let bp = self.cpu.registers.ebp() as u16;
                let _ = self.memory.write_u16(0x1F * 4, bp);
                let _ = self.memory.write_u16(0x1F * 4 + 2, es);
            }
            0x21 => {
                let bp = self.cpu.registers.ebp() as u16;
                let es_base = self.cpu.registers.segment(SegmentIndex::Es).base;
                let bytes_per_char = cx.clamp(1, 32) as u8;
                let bytes = self
                    .read_guest_block(es_base + u32::from(bp), 256 * usize::from(bytes_per_char));
                self.video.set_char_map_select(0);
                self.video.load_font_table(0, 0, bytes_per_char, &bytes);
                self.set_graphics_font_grid(bytes_per_char, bl, dx as u8);
            }
            0x22 => self.load_rom_graphics_font(14, bl, dx as u8),
            0x23 => self.load_rom_graphics_font(8, bl, dx as u8),
            0x24 => self.load_rom_graphics_font(16, bl, dx as u8),
            0x30 => {
                if bh == 0x01 {
                    self.publish_int43_font_table();
                }
                self.int10_font_info(bh);
            }
            _ => {}
        }
    }

    fn set_int43_pointer(&mut self, segment: u16, offset: u16) {
        let _ = self.memory.write_u16(0x43 * 4, offset);
        let _ = self.memory.write_u16(0x43 * 4 + 2, segment);
    }

    fn publish_int43_font_table(&mut self) {
        let height = self.video.char_height();
        let table = self.video.active_font_table();
        let bytes = self.video.font_table_image(table, height);
        self.write_guest_block(VGA_BIOS_INT43_FONT_ADDR, &bytes);
        self.set_int43_pointer(VGA_BIOS_SEGMENT, VGA_BIOS_FONT_TABLE_OFF);
        let _ = self.memory.write_u8(0x485, height);
    }

    fn int10_font_info(&mut self, specifier: u8) {
        let Some((segment, offset)) = self.font_info_pointer(specifier) else {
            return;
        };
        self.cpu
            .registers
            .set_segment(SegmentIndex::Es, SegmentRegister::real(segment));
        self.cpu
            .registers
            .set_ebp((self.cpu.registers.ebp() & !0xFFFF) | u32::from(offset));
        let char_height = self.read_physical_u8(0x485).max(1);
        self.set_cx(u16::from(char_height));
        let rows_minus_1 = self.read_physical_u8(0x484);
        let edx = (self.cpu.registers.edx() & !0xFF) | u32::from(rows_minus_1);
        self.cpu.registers.set_edx(edx);
    }

    fn font_info_pointer(&mut self, specifier: u8) -> Option<(u16, u16)> {
        Some(match specifier {
            0x00 => (
                self.read_guest_word(0x1F * 4 + 2),
                self.read_guest_word(0x1F * 4),
            ),
            0x01 => (
                self.read_guest_word(0x43 * 4 + 2),
                self.read_guest_word(0x43 * 4),
            ),
            0x02 | 0x05 => (BIOS_ROM_SEGMENT, BIOS_FONT_8X14_ROM_OFFSET),
            0x03 => (BIOS_ROM_SEGMENT, BIOS_FONT_8X8_ROM_OFFSET),
            0x04 => (BIOS_ROM_SEGMENT, BIOS_FONT_8X8_HIGH_ROM_OFFSET),
            0x06 | 0x07 => (BIOS_ROM_SEGMENT, BIOS_FONT_8X16_ROM_OFFSET),
            _ => return None,
        })
    }

    fn load_rom_graphics_font(&mut self, height: u8, row_specifier: u8, user_rows: u8) {
        self.video.set_char_map_select(0);
        self.video.load_rom_font(0, height);
        self.set_graphics_font_grid(height, row_specifier, user_rows);
    }

    fn set_graphics_font_grid(&mut self, bytes_per_char: u8, row_specifier: u8, user_rows: u8) {
        if self.video.active_mode() != VideoMode::Planar {
            return;
        }
        let rows = match row_specifier {
            0 => user_rows,
            1 => 14,
            2 => 25,
            3 => 43,
            _ => self.text_rows() as u8,
        }
        .clamp(1, 60);
        let _ = self.memory.write_u8(0x484, rows - 1);
        let _ = self.memory.write_u16(0x485, u16::from(bytes_per_char));
    }

    /// The minimal interrupt surface for a `new_raw_program` machine: INT 20h
    /// and AH=4Ch terminate; AH=01h/02h/06h/09h console I/O; anything else
    /// returns DOS's "invalid function" convention (CF=1, AX=0007h) instead
    /// of doing nothing silently. No file I/O, no critical error, no EXEC —
    /// see `dev_docs/2026-06-30-katea-sp3-program-runtime-design.md` section 3.
    fn handle_raw_program_int(&mut self, vector: u8) -> Result<Option<u8>, BusError> {
        let ss = self.cpu.registers.segment(SegmentIndex::Ss).base;
        let sp = self.cpu.registers.esp() as u16;
        let flags_addr = (ss + u32::from(sp.wrapping_add(4))) as usize;

        if vector == 0x20 {
            return Ok(Some(0));
        }
        let ax = self.cpu.registers.eax() as u16;
        let ah = (ax >> 8) as u8;
        match ah {
            0x4c => return Ok(Some(ax as u8)),
            0x01 => {
                const KBD_BDA_BASE: usize = 0x400;
                const KBD_HEAD: usize = 0x1a;
                const KBD_TAIL: usize = 0x1c;
                const KBD_RING_START: u16 = 0x1e;
                const KBD_RING_END: u16 = 0x3e;
                let head = self.memory.read_u16(KBD_BDA_BASE + KBD_HEAD)?;
                let tail = self.memory.read_u16(KBD_BDA_BASE + KBD_TAIL)?;
                if head == tail {
                    // Blocking read with an empty ring: rewind the stacked
                    // return IP by 2 so the IRET stub re-enters the same
                    // `CD 21`, and set IF so IRQ1 can run the keyboard ISR
                    // before the retry (a wait-for-key spin).
                    let ip_addr = (ss + u32::from(sp)) as usize;
                    let ret_ip = self.memory.read_u16(ip_addr)?;
                    self.memory.write_u16(ip_addr, ret_ip.wrapping_sub(2))?;
                    let mut flags = self.memory.read_u16(flags_addr)?;
                    flags |= 0x0200; // IF
                    self.memory.write_u16(flags_addr, flags)?;
                    return Ok(None);
                }
                let word = self.memory.read_u16(KBD_BDA_BASE + usize::from(head))?;
                let mut next = head + 2;
                if next >= KBD_RING_END {
                    next = KBD_RING_START;
                }
                self.memory.write_u16(KBD_BDA_BASE + KBD_HEAD, next)?;
                let ch = word as u8;
                self.program_output.push(ch);
                self.cpu.registers.set_eax(u32::from(ch));
            }
            0x02 => {
                let dl = (self.cpu.registers.edx() & 0xff) as u8;
                self.program_output.push(dl);
            }
            0x06 => {
                let dl = (self.cpu.registers.edx() & 0xff) as u8;
                if dl == 0xff {
                    // Char-in-no-wait: report "nothing available" via ZF, the
                    // same shape int14_fossil_keyboard_read uses for polling.
                    const KBD_BDA_BASE: usize = 0x400;
                    const KBD_HEAD: usize = 0x1a;
                    const KBD_TAIL: usize = 0x1c;
                    let head = self.memory.read_u16(KBD_BDA_BASE + KBD_HEAD)?;
                    let tail = self.memory.read_u16(KBD_BDA_BASE + KBD_TAIL)?;
                    let mut flags = self.memory.read_u16(flags_addr)?;
                    if head == tail {
                        flags |= 0x0040; // ZF set: nothing available
                        self.cpu.registers.set_eax(0);
                    } else {
                        let word = self.memory.read_u16(KBD_BDA_BASE + usize::from(head))?;
                        let mut next = head + 2;
                        const KBD_RING_START: u16 = 0x1e;
                        const KBD_RING_END: u16 = 0x3e;
                        if next >= KBD_RING_END {
                            next = KBD_RING_START;
                        }
                        self.memory.write_u16(KBD_BDA_BASE + KBD_HEAD, next)?;
                        flags &= !0x0040; // ZF clear: a char is in AL
                        self.cpu.registers.set_eax(u32::from(word as u8));
                    }
                    self.memory.write_u16(flags_addr, flags)?;
                } else {
                    self.program_output.push(dl);
                }
            }
            0x09 => {
                let ds = self.cpu.registers.segment(SegmentIndex::Ds).base;
                let dx = self.cpu.registers.edx() as u16;
                let mut addr = ds + u32::from(dx);
                loop {
                    let byte = self.memory.read_u8(addr as usize)?;
                    if byte == b'$' {
                        break;
                    }
                    self.program_output.push(byte);
                    addr = addr.wrapping_add(1);
                }
            }
            _ => {
                let mut flags = self.memory.read_u16(flags_addr)?;
                flags |= 0x0001; // CF
                self.memory.write_u16(flags_addr, flags)?;
                self.cpu.registers.set_eax(0x0007);
            }
        }
        Ok(None)
    }

    /// Perform a Toka-DOS service requested through Lotura port 0xE3, recording the
    /// status the BIOS reads back. Cmd 0x01 (Repair Toka-DOS) resets the Katea host
    /// folder's CONFIG.SYS/AUTOEXEC.BAT. (The legacy 0x10 HLE C: boot shim was
    /// removed with the Rust DOS kernel in SP-3.)
    fn perform_toka_service(&mut self, command: u8) {
        self.toka_service_status = match command {
            0x01 => self.katea_repair(),
            _ => 0xff,
        };
    }

    /// Repair Toka-DOS: back the user's CONFIG.SYS/AUTOEXEC.BAT up to .OLD, write
    /// fresh defaults from the committed payload, and re-mount so the boot uses
    /// them. Returns the BIOS status (0 ok, 1 no Katea folder, 0xfe write/mount error).
    fn katea_repair(&mut self) -> u8 {
        let Some(root) = self.katea_root.clone() else {
            return 1; // no Katea host folder mounted
        };
        let (config, autoexec) = default_config_pair();
        // Per-file, not atomic across the two: if AUTOEXEC's write fails after
        // CONFIG was already rewritten, the folder is left half-repaired (default
        // CONFIG, no live AUTOEXEC). No data is lost — both originals survive in
        // their .OLD — and 0xfe tells the user it failed; Repair is a rare manual
        // recovery, so the simple sequence is acceptable.
        for (live_name, old_name, bytes) in [
            ("CONFIG.SYS", "CONFIG.OLD", &config),
            ("AUTOEXEC.BAT", "AUTOEXEC.OLD", &autoexec),
        ] {
            let live = root.join(live_name);
            if live.exists() {
                // Best-effort backup (std::fs::rename replaces an existing .OLD on Windows).
                let _ = std::fs::rename(&live, root.join(old_name));
            }
            if std::fs::write(&live, bytes).is_err() {
                return 0xfe;
            }
        }
        // Rebuild the volume from the repaired folder so the subsequent boot reads it.
        if self.mount_hdd_folder(&root).is_err() {
            return 0xfe;
        }
        0
    }

    fn read_guest_asciiz_lossy(&mut self, segment: u16, offset: u16, max: usize) -> String {
        let base = u32::from(segment) * 16 + u32::from(offset);
        let mut bytes = Vec::new();
        for i in 0..max {
            let byte = self.read_physical_u8(base + i as u32);
            if byte == 0 {
                break;
            }
            bytes.push(byte);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// Mirror any console output produced since the last call onto the VGA
    /// text screen. Programs write CON through INT 21h, which is buffered in
    /// `self.program_output` for the native `new_raw_program` runtime; real
    /// DOS renders that to the screen via the BIOS teletype. We do the same
    /// here so a session is visible on the framebuffer, sharing the BDA cursor
    /// at 0040:0050 with the BIOS.
    fn flush_dos_console_to_screen(&mut self) {
        let total = self.console_output().len();
        if self.dos_screen_shown >= total {
            return;
        }
        let pending: Vec<u8> = self.console_output()[self.dos_screen_shown..].to_vec();
        self.dos_screen_shown = total;
        for byte in pending {
            self.teletype_char(byte);
        }
    }

    /// The live console output buffer. With the Rust DOS kernel retired every
    /// machine uses the native `program_output` buffer.
    fn console_output(&self) -> &[u8] {
        &self.program_output
    }

    /// Write one character to the VGA text screen at the BDA cursor, advancing it
    /// with CR, LF, backspace, tab, and bottom-of-screen scroll, the way the BIOS
    /// teletype (INT 10h AH=0Eh) does. Attribute 0x07 is light grey on black.
    fn teletype_char(&mut self, byte: u8) {
        let page = self.active_bios_page();
        self.teletype_char_attr(byte, 0x07, page);
    }

    fn teletype_char_attr(&mut self, byte: u8, attr: u8, page: u8) {
        let page = self.normalize_bios_page(page);
        let cursor = self.cursor_pos(page);
        let columns = self.text_columns();
        let mut col = usize::from(cursor & 0x00ff);
        let mut row = usize::from(cursor >> 8);
        match byte {
            b'\r' => col = 0,
            b'\n' => row += 1,
            0x08 => col = col.saturating_sub(1), // backspace
            0x07 => {}                           // bell: no visible effect
            b'\t' => {
                col = (col + 8) & !7;
                if col >= columns {
                    col = 0;
                    row += 1;
                }
            }
            _ => {
                self.write_bios_char_cell(page, row, col, byte, attr);
                col += 1;
                if col >= columns {
                    col = 0;
                    row += 1;
                }
            }
        }
        while row >= self.text_rows() {
            self.scroll_text_up(page);
            row -= 1;
        }
        self.set_cursor_pos(page, ((row as u16) << 8) | col as u16);
    }

    /// Scroll the active text screen up one line, clearing the bottom row to
    /// spaces with the normal attribute.
    fn scroll_text_up(&mut self, page: u8) {
        if self.is_bios_graphics_text_mode() {
            self.scroll_graphics_text_up(page);
            return;
        }
        let base = self.text_page_base(page);
        let columns = self.text_columns();
        let rows = self.text_rows();
        let row_bytes = columns * 2;
        for offset in 0..((rows - 1) * row_bytes) {
            let byte = self
                .video
                .read_u8(base + offset + row_bytes)
                .unwrap_or(b' ');
            let _ = self.video.write_u8(base + offset, byte);
        }
        let last = base + (rows - 1) * row_bytes;
        for col in 0..columns {
            let _ = self.video.write_u8(last + col * 2, b' ');
            let _ = self.video.write_u8(last + col * 2 + 1, 0x07);
        }
    }

    fn scroll_graphics_text_up(&mut self, page: u8) {
        let columns = self.text_columns();
        let rows = self.text_rows();
        self.scroll_graphics_window(page, true, 1, 0, 0, 0, rows - 1, columns - 1);
    }

    #[allow(clippy::too_many_arguments)]
    fn scroll_graphics_window(
        &mut self,
        page: u8,
        up: bool,
        lines: usize,
        color: u8,
        top: usize,
        left: usize,
        bottom: usize,
        right: usize,
    ) {
        let cell_height = self.graphics_text_cell_height();
        let x0 = (left * 8) as u16;
        let x1 = ((right + 1) * 8).min(self.video.raster_width() as usize) as u16;
        let y0 = (top * cell_height) as u16;
        let y1 = ((bottom + 1) * cell_height) as u16;
        let height = bottom - top + 1;
        let fill = match self.video.active_mode() {
            VideoMode::Cga => color & 0x7F,
            VideoMode::Planar => color & 0x0F,
            _ => color,
        };

        if lines >= height {
            for y in y0..y1 {
                for x in x0..x1 {
                    let _ = self.graphics_write_pixel(page, x, y, fill, false);
                }
            }
            return;
        }

        let shift = (lines * cell_height) as u16;
        if up {
            for y in y0..(y1 - shift) {
                for x in x0..x1 {
                    let color = self.graphics_read_pixel(page, x, y + shift);
                    let _ = self.graphics_write_pixel(page, x, y, color, false);
                }
            }
            for y in (y1 - shift)..y1 {
                for x in x0..x1 {
                    let _ = self.graphics_write_pixel(page, x, y, fill, false);
                }
            }
        } else {
            for y in (y0 + shift..y1).rev() {
                for x in x0..x1 {
                    let color = self.graphics_read_pixel(page, x, y - shift);
                    let _ = self.graphics_write_pixel(page, x, y, color, false);
                }
            }
            for y in y0..(y0 + shift) {
                for x in x0..x1 {
                    let _ = self.graphics_write_pixel(page, x, y, fill, false);
                }
            }
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
        if !bytes.is_empty() {
            self.device_wrote_memory = true;
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
        self.distira.disable_display();
    }

    pub fn active_display(&self) -> ActiveDisplay {
        // Every VGA mode (text, planar, mode X, mode 13h) now presents a raster
        // through the core. VEGA also exposes Margo's linear framebuffer and
        // Distira's Voodoo-style front buffer as alternate scanout paths.
        if self.distira.display_enabled() {
            ActiveDisplay::Distira
        } else if self.margo_active {
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
                dots => self.video.dot_clock_hz() as f64 / dots as f64,
            },
            ActiveDisplay::MargoLfb | ActiveDisplay::Distira => 60.0,
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
            ActiveDisplay::Distira => {
                let display = self.distira.display();
                let (width, height) = (display.width as usize, display.height as usize);
                (self.distira.scanout_argb(), width, height)
            }
        }
    }

    /// An O(1) content-generation key for the host-side dirty-framebuffer cache.
    ///
    /// Returns `Some(key)` only when the output is a pure function of guest writes —
    /// the active display is the VGA raster AND the mode is a graphics mode (mode 13h,
    /// planar, mode X, CGA graphics). The key changes iff the graphics-mode output
    /// could change, so a consumer that re-renders only when the key moves can never
    /// show a stale frame, while idling on a static screen. It folds every input that
    /// can change the output: the Vga `content_gen` (bumped inside every Vga display
    /// mutator — VRAM writers, register/DAC writes, and the start-address latch — so
    /// it catches writes from BOTH the CPU bus AND the HLE BIOS INT 10h services that
    /// mutate `self.video` directly, regardless of caller), plus the raster dimensions
    /// (so a mode or resolution change always moves the key).
    ///
    /// Returns `None` for text mode (time-based cursor/attribute blink toggles with no
    /// guest write, so writes alone cannot capture it — text keeps re-rendering), and —
    /// in v1 — for Margo LFB / Distira (their own scanout; a generation for them is
    /// deferred to v2). Pure `&self`: no rendering, no timing side effects.
    pub fn frame_generation(&self) -> Option<u64> {
        if self.active_display() != ActiveDisplay::VgaRaster || self.video.is_text_mode() {
            return None;
        }
        // A cheap reversible mix: each input is multiplied by a distinct large odd
        // constant, so the key changes whenever any input changes.
        const K: u64 = 0x9E37_79B9_7F4A_7C15; // golden-ratio odd multiplier
        let width = u64::from(self.video.raster_width());
        let height = u64::from(self.video.raster_height());
        let key = self
            .video
            .content_gen()
            .wrapping_mul(K)
            .wrapping_add(width.wrapping_mul(0x0001_0000_0001))
            .wrapping_add(height.wrapping_mul(0x1_0000_0001_0000));
        Some(key)
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
    // Limit: single path, overwrite. Add an index suffix if a test ever needs
    // to capture multiple frames in one run.
    pub fn set_test_snapshot_path(&mut self, path: Option<std::path::PathBuf>) {
        self.test_snapshot_path = path;
    }

    /// Preload the Neurketa benchmark selector the guest reads at start to pick
    /// its payload. Call before `run_until_halt_or_cycles`.
    pub fn set_bench_selector(&mut self, selector: u8) {
        self.unittester
            .set_reg_u8(unittester::REG_SELECTOR, selector);
    }

    /// The iteration count the Neurketa payload reported before `CMD_EXIT`.
    pub fn bench_iterations(&self) -> u32 {
        self.unittester.reg_u32(unittester::REG_RESULT_ITER)
    }

    /// The payload-specific auxiliary value (the Sieve reports its prime count).
    pub fn bench_aux(&self) -> u32 {
        self.unittester.reg_u32(unittester::REG_RESULT_AUX)
    }

    /// The payload status byte (1 once the payload ran to completion).
    pub fn bench_status(&self) -> u8 {
        self.unittester.reg_u8(unittester::REG_RESULT_STATUS)
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
            unittester::CMD_EXIT => {
                // Diagnostic trace only (IZARRAVM_FAULT_TRACE=1): the Doom repro
                // needs to know whether the exit was a deliberate port write from
                // the running guest or a stray fetch. The run loop's OUT to 0xE6
                // always ends the batch before this deferred command executes
                // (write_io sets io_touched unconditionally), so CS:IP here is the
                // guest instruction right after the OUT, the closest reachable
                // point to the origin without threading CS:IP through CpuBus.
                if fault_trace_enabled() {
                    let cs = self.cpu.registers.cs().selector;
                    let eip = self.cpu.registers.eip;
                    eprintln!(
                        "fault trace: OUT 0xE6 CMD_EXIT val={cmd:#04x} \
                         next-guest-CS:IP={cs:#06x}:{eip:#010x} v86={} ring0={}",
                        self.cpu.is_v86_mode(),
                        self.cpu.is_ring0_protected(),
                    );
                }
                Some(self.unittester.exit_code())
            }
            _ => None, // unknown command: ignore, like an unused port write
        }
    }

    /// Log a fatal `CpuError` that stopped the run loop (env-gated, see
    /// `fault_trace_enabled`). Reports whatever CS:IP the CPU shows at the
    /// error site: for the V86-sensitive-op / selector-load faults this is the
    /// faulting guest instruction directly (the error is raised before any
    /// exception delivery runs), and for a fault raised while the TOKAEMM
    /// monitor is running ring-0 PM code it is the monitor's own CS:IP (the
    /// V86 guest CS:IP the monitor was servicing is on its stack, not
    /// reachable here without walking the ring-0 stack frame -- noted as the
    /// gap rather than adding a paging-aware stack walk to this trace).
    fn log_fault_trace(&mut self, error: &CpuError) {
        let cs = self.cpu.registers.cs().selector;
        let eip = self.cpu.registers.eip;
        let cs_base = self.cpu.registers.cs().base;
        eprintln!(
            "fault trace: {error} at CS:IP={cs:#06x}:{eip:#010x} v86={} ring0={}",
            self.cpu.is_v86_mode(),
            self.cpu.is_ring0_protected(),
        );
        eprintln!(
            "fault trace: CS base={cs_base:#010x} limit={:#010x} linear EIP={:#010x}",
            self.cpu.registers.cs().limit,
            cs_base.wrapping_add(eip),
        );
        let linear_eip = cs_base.wrapping_add(eip);
        let mut bytes_before = String::new();
        let start = linear_eip.saturating_sub(32);
        for addr in start..linear_eip {
            bytes_before.push_str(&format!("{:02x} ", self.read_physical_u8(addr)));
        }
        eprintln!(
            "fault trace: bytes before EIP [{start:#010x}..{linear_eip:#010x}): {bytes_before}"
        );
        let mut bytes_after = String::new();
        for addr in linear_eip..linear_eip.saturating_add(32) {
            bytes_after.push_str(&format!("{:02x} ", self.read_physical_u8(addr)));
        }
        eprintln!("fault trace: bytes at/after EIP [{linear_eip:#010x}..): {bytes_after}");
        // Dump the guest stack (128 bytes each direction) using SS base + ESP.
        let ss_base = self
            .cpu
            .registers
            .segment(izarravm_cpu::SegmentIndex::Ss)
            .base;
        let esp = self.cpu.registers.esp();
        let stack_linear = ss_base.wrapping_add(esp);
        let mut stack_before = String::new();
        let sb_start = stack_linear.saturating_sub(128);
        for addr in (sb_start..stack_linear).step_by(4) {
            stack_before.push_str(&format!("{:08x} ", self.read_physical_u32(addr)));
        }
        eprintln!(
            "fault trace: SS:ESP={:#06x}:{esp:#010x} linear={stack_linear:#010x}",
            self.cpu
                .registers
                .segment(izarravm_cpu::SegmentIndex::Ss)
                .selector
        );
        eprintln!("fault trace: stack before ESP: {stack_before}");
        let mut stack_after = String::new();
        for addr in (stack_linear..stack_linear.saturating_add(128)).step_by(4) {
            stack_after.push_str(&format!("{:08x} ", self.read_physical_u32(addr)));
        }
        eprintln!("fault trace: stack at/after ESP: {stack_after}");
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

    pub fn set_bus_trace_detailed(&mut self, detailed: bool) {
        self.trace.set_tracing_mode(if detailed {
            TracingMode::Full
        } else {
            TracingMode::Off
        });
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

    /// Scale a step's raw bus clocks by the active level's `bus_timing` factor,
    /// carrying the fractional remainder so a cheap access in a fast mode is not
    /// rounded to zero. This is the THIRD timing lever (B-T10): it scales the whole
    /// bus portion (instruction fetch + every tiered data access already summed into
    /// `raw`) per mode, supplying the absolute per-mode magnitude that lets a fast
    /// part pull away from the flat per-access floor. The relative L1<L2<RAM tier
    /// structure stays in the `tier_cost` wait-states; this only sets the overall
    /// scale. Cheap by construction: one multiply + a modulo per call, not per
    /// access. Mirrors the CPU's `scale_clocks` for instruction clocks.
    fn scale_bus(&mut self, raw: u64) -> u64 {
        let (num, den) = bus_timing(self.cpu.level());
        let scaled = raw * u64::from(num) + self.bus_rem;
        self.bus_rem = scaled % u64::from(den);
        scaled / u64::from(den)
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
        // The modeled cache contents are per-mode (geometry changes with the CPU
        // level); a mode switch starts cold.
        self.cache_model.set_level(cpu_level_for_mode(mode));
        // The bus scaler's fractional carry is per-mode (the ratio changes); start
        // a new mode with no carried remainder, exactly like the CPU does for its
        // instruction-clock scaler.
        self.bus_rem = 0;
    }

    /// The reported (L1 KB, L2 KB) cache for the live mode (the L2 models a
    /// motherboard cache module). Feeds the BIOS setup and GUI readout, and the same
    /// per-mode geometry (`cache_geometry`) also drives the `CacheModel` tiering, so
    /// this readout tracks the live data-access timing. Driven from the live CPU
    /// level so it tracks a Lotura mode switch.
    pub fn cache_config(&self) -> (u16, u16) {
        self.cpu.cache_kb()
    }

    /// The live compatibility mode (set at boot, changed by a Lotura mode write).
    pub fn active_mode(&self) -> GswMode {
        self.active_mode
    }

    pub fn cache_tier_lookups(&self) -> u64 {
        self.cache_model.lookups()
    }

    /// Measure pure memory read timing for a block by driving the bus directly.
    ///
    /// Resets the modeled cache, then does enough sequential dword-read passes
    /// over `[base, base + block_bytes)` to move roughly `total_bytes` total, and
    /// returns the bus clocks elapsed. The caller derives MB/s from
    /// `bytes / (clocks / clock_hz)`. Pick `base` in extended memory (>= 1 MB) so
    /// the sweep never crosses the 640 KB-1 MB device/ROM hole; `base +
    /// block_bytes` must fit the machine's memory.
    ///
    /// The reads go through the bus exactly as an instruction's data access would,
    /// so each access warms the per-mode modeled cache and records its
    /// wait-states into the shared `BusTrace`. A block that FITS the live mode's
    /// cache stays resident across passes (fast after pass 1); a block that
    /// EXCEEDS it re-misses every pass (slow). With many passes the steady state
    /// dominates, which is what produces the L1/L2/RAM steps: the tier costs are
    /// calibrated and the cheaper tiers charge fewer wait-states, so the curve
    /// DESCENDS from L1 down through L2 to RAM as the block grows past each tier.
    pub fn measure_read_bandwidth(
        &mut self,
        base: u32,
        block_bytes: u32,
        total_bytes: u64,
    ) -> BandwidthSample {
        self.cache_model.reset();
        // A bandwidth measurement is a self-contained sweep; start its bus-scaler
        // carry clean so the result does not depend on prior bus traffic.
        self.bus_rem = 0;
        let block = block_bytes.max(4) & !3; // whole dwords, at least one
        let passes = (total_bytes / u64::from(block)).max(2);
        // Build the bus in an inner scope so it drops (releasing the &mut borrow
        // of self.trace) before we read self.trace.elapsed_clocks() afterwards.
        let raw = {
            let mut bus = self.make_bus();
            // The bandwidth sweep verifies the ACCURATE tier calibration (a host
            // diagnostic, not guest-perceived time), so it always tiers even in the
            // Approximate class.
            bus.flat_data_cost = false;
            let start = bus.trace.elapsed_clocks();
            for _ in 0..passes {
                for off in (0..block).step_by(4) {
                    // Conventional RAM in extended memory never errors; ignore so a
                    // misconfigured base is recorded as zero clocks, not a panic.
                    let _ = bus.read_memory(base + off, BusWidth::Dword, BusAccessKind::DataRead);
                }
            }
            bus.trace.elapsed_clocks() - start
        };
        // The guest perceives the SCALED bus clocks (B-T10): a per-mode bus scaler
        // multiplies the whole bus portion, so the bandwidth tool must report the
        // scaled delta or it would show the pre-scaler floor and miss the per-mode
        // bus magnitude. Same `scale_bus` the run loop applies, on the swept delta.
        let clocks = self.scale_bus(raw);
        BandwidthSample {
            bytes: u64::from(block) * passes,
            clocks,
        }
    }

    /// Advance time-based devices by `clocks` of CPU time, carrying fractional
    /// remainders forward for the OPL timers (microseconds), the PIT counters,
    /// and the Margo blit engine (nanoseconds).
    fn advance_devices(&mut self, clocks: u64) {
        // The one shared fractional-advance formula (`advance_fractional`),
        // same discipline as the PIT block below.
        let (whole, remainder) =
            advance_fractional(self.opl_micros, clocks, self.timing.micros_per_clock);
        self.opl.advance_micros(whole);
        self.opl_micros = remainder;

        // The DSP reset-settle countdown advances with emulated time so a
        // detection routine's delay loop sees 0xAA become available. No lazy
        // twin yet; routed through the shared formula anyway so the last
        // hand-synchronized copy of its arithmetic is gone. `Dsp::
        // advance_micros` takes f64, and `whole as f64` reproduces the old
        // directly-passed `.floor()` value exactly: the u64 round-trip is
        // lossless for any integral value below 2^53 (~104 days of guest
        // microseconds in a single advance, unreachable under the batch caps).
        let (whole, remainder) =
            advance_fractional(self.dsp_micros, clocks, self.timing.micros_per_clock);
        self.dsp.advance_micros(whole as f64);
        self.dsp_micros = remainder;

        // DMA playback is clock-driven: accrue DSP sample phases per CPU clock
        // and, for each whole sample, advance the block and buffer the rendered
        // stereo frame onto the DSP ring. The half/end-buffer IRQ that
        // render_frame edges is forwarded to the PIC here, so playback timing and
        // IRQ5 no longer depend on the host frontend pulling audio. The host path
        // (render_dsp_audio) only drains what the clock already produced.
        //
        // MULTI-EDGE CONTRACT (holds for this DSP loop and the WSS/ADPCM loops
        // below, which mirror it): take_irq is drained INSIDE the producer loop,
        // at the sample tick that edged it, so every block edge reaches the PIC
        // within the advance in which it occurred and none is ever parked in the
        // device-side latch across a step (where a later gate, e.g. is_playing
        // going false at a single-cycle block end, could strand it). When one
        // advance spans N edges the PIC receives N requests, but the CPU does not
        // execute during advance_devices, so the guest cannot acknowledge between
        // them: the 8259 latches each request into IRR and a request on a
        // still-set IRR bit is absorbed, exactly as real hardware absorbs a new
        // pulse on a line whose interrupt is still pending. N intra-step edges
        // therefore deliver ONE guest interrupt by construction; that is the
        // architecturally correct coalescing, not a loss. What the run loop must
        // (and does, see the Approximate batch cap) arrange is that batches end
        // at block-edge instants when the guest needs one interrupt PER edge.
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
            let n = self.dsp_sample_phase as usize;
            self.dsp_sample_phase -= n as f64;
            let Machine {
                dsp, dma, memory, ..
            } = self;
            let is16 = dsp.is_16bit();
            let ch = if is16 { dma16 } else { dma8 };
            // HLE: on new block (command level), pre-fetch the entire block data
            // in bulk (advances DMA in one operation), store in buffer, then
            // render from buffer (no per-sample fetch micro steps).
            if dsp.block_remaining() == dsp.block_size() && dsp.block_buffer().is_none() {
                let bpp = if is16 { 2 } else { 1 };
                let nbytes = dsp.block_size() as usize * bpp;
                let mut buf = Vec::with_capacity(nbytes);
                for _ in 0..nbytes {
                    if is16 {
                        let w = dma.read_word(ch, memory).unwrap_or(0);
                        buf.extend_from_slice(&w.to_le_bytes());
                    } else {
                        buf.push(dma.read_byte(ch, memory).unwrap_or(0));
                    }
                }
                dsp.set_block_buffer(buf);
            }
            // HLE buffer feeding with refill support for auto-init blocks that may
            // be crossed inside a single large-n advance (e.g. tests and long steps).
            // We chunk the requested n at buffer/block boundaries so we can
            // re-prefetch the next DMA block when auto-init reloads remaining.
            let mut remaining = n;
            while remaining > 0 {
                if dsp.block_remaining() == dsp.block_size() && dsp.block_buffer().is_none() {
                    let bpp = if is16 { 2 } else { 1 };
                    let nbytes = dsp.block_size() as usize * bpp;
                    let mut buf = Vec::with_capacity(nbytes);
                    for _ in 0..nbytes {
                        if is16 {
                            let w = dma.read_word(ch, memory).unwrap_or(0);
                            buf.extend_from_slice(&w.to_le_bytes());
                        } else {
                            buf.push(dma.read_byte(ch, memory).unwrap_or(0));
                        }
                    }
                    dsp.set_block_buffer(buf);
                }
                let start_pos = dsp.block_buffer_pos();
                let mut consumed_from_buf: usize = 0;
                if let Some(buf) = dsp.block_buffer().cloned() {
                    let bytes_per_frame = if is16 { 2 } else { 1 };
                    let bytes_avail = buf.len().saturating_sub(start_pos);
                    if bytes_avail >= bytes_per_frame {
                        let frames_this = (bytes_avail / bytes_per_frame).min(remaining);
                        if is16 {
                            dsp.tick_n_samples(
                                frames_this,
                                || None,
                                || {
                                    let p = start_pos + consumed_from_buf;
                                    if p + 1 < buf.len() {
                                        let w = u16::from_le_bytes([buf[p], buf[p + 1]]);
                                        consumed_from_buf += 2;
                                        Some(w)
                                    } else {
                                        None
                                    }
                                },
                            );
                        } else {
                            dsp.tick_n_samples(
                                frames_this,
                                || {
                                    let p = start_pos + consumed_from_buf;
                                    if p < buf.len() {
                                        let b = buf[p];
                                        consumed_from_buf += 1;
                                        Some(b)
                                    } else {
                                        None
                                    }
                                },
                                || None,
                            );
                        }
                    }
                } else {
                    // Fallback direct per-frame (old path) for this chunk.
                    let frames_this = remaining; // will be limited by dry inside
                    if is16 {
                        dsp.tick_n_samples(frames_this, || None, || dma.read_word(ch, memory));
                    } else {
                        dsp.tick_n_samples(frames_this, || dma.read_byte(ch, memory), || None);
                    }
                }
                if consumed_from_buf > 0 {
                    dsp.advance_block_buffer(consumed_from_buf);
                }
                // If we hit end of this buf during the chunk, clear so next while
                // iteration (or future) can pre-fetch if auto-init reset the block.
                if dsp.block_buffer_pos() >= dsp.block_buffer_len() {
                    dsp.take_block_buffer();
                }
                // Reduce by how many we asked this chunk (actual produced may be
                // slightly less if dry, but advance will have stopped feeding).
                // Conservative: always reduce by the chunk size we targeted; if
                // underfed the phase carry will handle tail next advance.
                let did = if consumed_from_buf > 0 {
                    consumed_from_buf / (if is16 { 2 } else { 1 })
                } else {
                    remaining
                };
                if did == 0 {
                    break;
                }
                remaining = remaining.saturating_sub(did);
            }
            if dsp.take_irq() {
                let is_16bit = dsp.is_16bit();
                self.mixer.set_irq_status(is_16bit);
                self.pic.request(irq_line);
            }
        }
        // Forward a pending DSP interrupt with playback idle too: the 0xF2
        // IRQ-request command raises it without a transfer running (drivers
        // probe their IRQ wiring that way) — the real chip asserts the line
        // regardless. take_irq is a test-and-clear latch, so this never
        // double-delivers an edge the per-tick forward above already took.
        if self.dsp.take_irq() {
            let is_16bit = self.dsp.is_16bit();
            self.mixer.set_irq_status(is_16bit);
            self.pic.request(irq_line);
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
                let n = self.wss_sample_phase as usize;
                self.wss_sample_phase -= n as f64;
                if n > 0 {
                    // HLE block buffer pre-fetch + feeding for WSS (Phase 4), with
                    // support for spanning multiple blocks within large n (refill
                    // when auto-reload happens inside tick).
                    let mut remaining = n;
                    while remaining > 0 {
                        if playing_at_valid_rate && self.wss.block_buffer().is_none() {
                            let frames = self.wss.current_dma_count() as usize;
                            let count = frames * self.wss.bytes_per_frame();
                            if count > 0 {
                                let mut buf = Vec::with_capacity(count);
                                {
                                    let Machine { dma, memory, .. } = self;
                                    for _ in 0..count {
                                        buf.push(dma.read_byte(wss_dma, memory).unwrap_or(0));
                                    }
                                }
                                self.wss.set_block_buffer(buf);
                            }
                        }
                        let mut consumed_from_buf: usize = 0;
                        if playing_at_valid_rate {
                            if let Some(buf) = self.wss.block_buffer().cloned() {
                                let start_pos = self.wss.block_buffer_pos();
                                let bytes_avail = buf.len().saturating_sub(start_pos);
                                if bytes_avail > 0 {
                                    let frames_this = bytes_avail.min(remaining);
                                    self.wss.tick_n_samples(frames_this, || {
                                        let p = start_pos + consumed_from_buf;
                                        if p < buf.len() {
                                            let b = buf[p];
                                            consumed_from_buf += 1;
                                            Some(b)
                                        } else {
                                            None
                                        }
                                    });
                                }
                            } else {
                                let frames_this = remaining;
                                let Machine {
                                    wss, dma, memory, ..
                                } = self;
                                wss.tick_n_samples(frames_this, || dma.read_byte(wss_dma, memory));
                            }
                        }
                        if consumed_from_buf > 0 {
                            self.wss.advance_block_buffer(consumed_from_buf);
                        }
                        if self.wss.block_buffer_pos() >= self.wss.block_buffer_len() {
                            self.wss.take_block_buffer();
                        }
                        let did = if consumed_from_buf > 0 {
                            consumed_from_buf
                        } else {
                            remaining
                        };
                        if did == 0 {
                            break;
                        }
                        remaining = remaining.saturating_sub(did);
                    }
                    for _ in 0..n {
                        self.wss.advance_autocal();
                    }
                    // Forward any terminal-count edge produced in the batch (one
                    // request after N frames follows the multi-edge coalescing
                    // contract; see DSP path).
                    if self.wss.take_irq() {
                        self.pic.request(wss_irq);
                    }
                }
            }
        }

        // CD audio (Red Book 44.1 kHz) HLE time-driven advance (Phase 4).
        // Drive the playback LBA from guest elapsed time so position is accurate
        // independent of when the mixer drains samples. Pull in render_audio
        // consumes from the advanced position (frac for sub-frame continuity).
        // Fixed rate, no "programmed" variation.
        if self.ide.device().playback().playing {
            let cd_rate: u64 = DAC_HZ as u64; // 44100
            self.cd_sample_phase += clocks as f64 * cd_rate as f64 * self.timing.inv_clock;
            let n = self.cd_sample_phase as usize;
            self.cd_sample_phase -= n as f64;
            if n > 0 {
                let frames = n / 588;
                if frames > 0 {
                    self.ide.device_mut().advance_play(frames as u32);
                }
            }
        }

        let ch2_before = self.pit.channel_out(2);
        let pit_fraction_before = self.pit_clocks;
        // The one shared fractional-advance formula (`advance_fractional`): the
        // lazy port 0x61 peek (`MachineBus::elapsed_pit_clocks`) calls the same
        // function with the same batch-entry carry and pit_per_clock, so its
        // mid-batch answer floors exactly where this real advance will.
        let (whole, remainder) =
            advance_fractional(self.pit_clocks, clocks, self.timing.pit_per_clock);
        self.pit_clocks = remainder;
        self.speaker_transitions.clear();
        let edges =
            self.pit
                .tick_recording_out_transitions(whole, 2, &mut self.speaker_transitions);
        // Per-edge forwarding, same multi-edge contract as the DSP loop above:
        // N channel-0 edges in one step issue N requests and the PIC's IRR
        // coalesces them into the one interrupt the guest can actually take.
        for _ in 0..edges {
            self.pic.request(0); // channel 0 OUT rising edge is IRQ0
        }

        // PC speaker: integrate channel-2 OUT transitions at PIT-clock precision,
        // then let the speaker model produce DAC-rate samples.
        let seconds = clocks as f64 * self.timing.inv_clock;
        let transitions = self.speaker_transitions.iter().map(|event| {
            (
                (event.tick as f64 - pit_fraction_before) / PIT_INPUT_HZ as f64,
                event.level,
            )
        });
        self.speaker.accumulate(seconds, ch2_before, transitions);

        // Decay the keyboard-to-aux settle window (see KEYBOARD_TO_AUX_SETTLE_US
        // in keyboard.rs) so a mouse byte held back by a just-read keyboard
        // scancode releases once real PS/2 wire time has actually elapsed.
        self.keyboard
            .advance_mouse_pacing(clocks as f64 * self.timing.micros_per_clock);

        // The take_irq latches below are single-edge bools, which is safe for
        // these devices even across a multi-sample step: each is a completion
        // edge that can fire at most once per guest-initiated operation (a
        // scancode/aux byte entering the 8042 output buffer, a UART event, an
        // LPT -ACK strobe, an FDC/IDE/ATA command completion), and the next
        // operation requires guest port I/O, which ends the CPU batch. No
        // periodic producer feeds them, so one advance can never span two edges.
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
            // A single-edge bool is safe here: IRQF sources are seconds-scale
            // (far coarser than any batch) and the line stays asserted until the
            // Register C ack, so one advance cannot span two IRQ8 edges.
            if self.rtc.tick_interrupts(secs) {
                self.pic.request(8); // IRQ8: RTC periodic/alarm/update interrupt
            }
            self.rtc_seconds -= whole_secs;
        }

        self.margo_ns += clocks as f64 * self.timing.margo_ns_per_clock;
        let whole_ns = self.margo_ns.floor();
        self.margo.advance_busy(whole_ns as u64);
        self.margo_ns -= whole_ns;

        // Distira has no dot-clock beam model of its own (see
        // Distira::advance_frame_phase); feed it CPU clocks directly so
        // SST_V_RETRACE/SST_HV_RETRACE/SST_STATUS's vsync bit make forward
        // progress and a real vsync poll loop cannot hang.
        self.distira.advance_frame_phase(clocks);

        let (whole, remainder) = self.predict_dots(clocks, self.vga_dots);
        self.video.advance(whole);
        self.vga_dots = remainder;

        self.pump_pusher();
    }

    /// Whole VGA dot-clocks elapsed for `clocks` CPU clocks, given the live
    /// fractional-dot accumulator `dots_owed`. Pure: does not mutate `self`. The
    /// SAME arithmetic (via `predict_dots_core`) both `advance_devices` (the real,
    /// mutating step above) and the Slice 1 lazy port-read peek
    /// (`MachineBus::predicted_beam`) apply, so the two paths cannot structurally
    /// diverge: "no time travel" (a lazy read predicting state the later real
    /// advance would contradict) becomes a property of sharing one implementation,
    /// not an invariant maintained by hand across two call sites.
    /// See dev_docs/2026-07-02-p4a-lazy-port-device-time-plan.md Task 0.3/1.2.
    fn predict_dots(&self, clocks: u64, dots_owed: f64) -> (u64, f64) {
        predict_dots_core(
            clocks,
            dots_owed,
            self.video.dot_clock_hz(),
            self.timing.inv_clock,
        )
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
        // Fast path (N2/N4 perf plan): when the 4 bytes neither wrap the ring boundary nor run
        // off system RAM, read them as one slice instead of four bounds-checked byte fetches.
        let start = off as usize % size as usize;
        if start + 4 <= size as usize {
            if let Some(slice) = self
                .memory
                .as_slice()
                .get(base as usize + start..base as usize + start + 4)
            {
                return u32::from_le_bytes(slice.try_into().unwrap());
            }
        }
        // Slow path: the word straddles the ring wrap or the RAM edge — per-byte with wrap, an
        // out-of-range byte reading as 0 (unchanged semantics).
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
    /// host goldens can advance the clock that now paces playback. Does NOT
    /// advance the master clock; see `advance_wall_clocks` for the variant that
    /// moves both.
    pub fn advance_devices_clocks(&mut self, clocks: u64) {
        self.advance_devices(clocks);
    }

    /// Advance device time AND the master clock by `clocks` without running the
    /// CPU. Used by the GUI in the Approximate class when the host could not
    /// execute the full wall-clock budget: guest time keeps tracking wall time so
    /// audio/PIT hold realtime; the CPU simply retires fewer instructions per
    /// guest second (the DOSBox-style degradation). Never called in the Accurate
    /// class. Unconditional: prefer `advance_wall_shortfall`, which stops at VGA
    /// vertical-retrace start edges so a polling guest can observe them; this
    /// variant is the escape hatch behind the caller's defensive edge-stop cap.
    pub fn advance_wall_clocks(&mut self, clocks: u64) {
        self.advance_devices(clocks);
        self.elapsed_clocks += clocks;
    }

    /// Advance device time AND the master clock by AT MOST `clocks` without
    /// running the CPU, and return the clocks actually consumed.
    ///
    /// Contract: if the next VGA vertical-retrace START edge falls strictly
    /// inside the span, the advance stops AT that edge (the beam lands on the
    /// first dot of the retrace window, so a port 0x3DA read already returns
    /// bit 3 set) and the consumed count is returned; the caller tops up the
    /// remainder in further calls, typically granting the CPU a small execution
    /// quantum in between so a guest polling 0x3DA observes the window. With no
    /// intervening edge the full `clocks` is consumed.
    ///
    /// Why: a 16 ms wall-pacing top-up sweeps the beam across more than a whole
    /// mode-13h frame (14.3 ms) with zero instructions executing, so a guest
    /// double-polling 0x3DA for the 2-scanline vretrace window deterministically
    /// missed every window that opened and closed inside a top-up (measured
    /// catch rate 12.8 percent at a 1/8 execution share). Stopping at each start
    /// edge makes every window observable.
    ///
    /// Termination guarantee: the returned count is >= 1 whenever `clocks` >= 1.
    /// When the beam already sits on the edge or inside the retrace window, the
    /// next start edge is a full frame ahead (see
    /// `Vga::dots_until_vretrace_start`), so back-to-back calls always make
    /// progress and a caller looping `remaining -= consumed` terminates. The
    /// stop honors the fractional `vga_dots` accumulator, overshooting the edge
    /// by at most a few dots (well inside the ~1600-dot window). One caveat: a
    /// 1-ulp rounding mismatch in the dots-to-clocks conversion could in
    /// principle land the beam a dot short of the edge; the caller's peek
    /// executes instructions whose own device advance carries the beam into the
    /// window, so the contract holds for observers either way.
    pub fn advance_wall_shortfall(&mut self, clocks: u64) -> u64 {
        let consume = match self.clocks_to_vretrace_start() {
            Some(edge_clocks) => edge_clocks.min(clocks),
            None => clocks,
        };
        self.advance_wall_clocks(consume);
        consume
    }

    /// Clocks of device time until the VGA beam reaches the next vertical-
    /// retrace start edge, converted from beam dots at the live TimingFactors
    /// and accounting for the fractional `vga_dots` accumulator: delivering the
    /// returned count to `advance_devices` moves the beam onto (or a dot or two
    /// past) the edge. `None` when the CRTC has no usable frame geometry.
    fn clocks_to_vretrace_start(&self) -> Option<u64> {
        let edge_dots = self.video.dots_until_vretrace_start()?;
        let dots_per_clock = self.video.dot_clock_hz() as f64 * self.timing.inv_clock;
        if dots_per_clock <= 0.0 {
            return None;
        }
        let needed = ((edge_dots as f64 - self.vga_dots) / dots_per_clock).ceil();
        Some((needed as u64).max(1))
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

    /// Set the ReSonique 2 analog output-stage gain (the host "amp gain"). Applied
    /// in [`render_audio`](Self::render_audio) to the card's sources only. Clamped
    /// non-negative; 1.0 is unity.
    pub fn set_card_amp(&mut self, amp: f32) {
        self.card_amp = amp.max(0.0);
    }

    /// Set the PC speaker output volume (host-side). Applied in
    /// [`render_audio`](Self::render_audio) to the speaker only. Clamped to
    /// 0.0..=1.0; 0.0 mutes the beeps, 1.0 is full.
    pub fn set_speaker_volume(&mut self, volume: f32) {
        self.speaker_volume = volume.clamp(0.0, 1.0);
    }

    /// Render `native_samples` of mixed OPL3 + SB16 DSP audio at the 44100 Hz DAC
    /// rate (stereo, saturated to 16-bit). `native_samples` is counted in OPL
    /// native (49716 Hz) time; the DSP is advanced by the matching wall-clock
    /// duration at its own rate. Each stream is resampled to 44100 and summed.
    ///
    /// The ReSonique 2 analog output-stage gain (`self.card_amp`, the host-tunable
    /// "amp gain") is applied to the card's own sources (OPL, SB DSP, the WSS
    /// codec, and CD-audio through the card's CD-in) but NOT to the PC speaker,
    /// which is motherboard hardware that does not pass through the card's amp.
    pub fn render_audio(&mut self, native_samples: usize) -> Vec<(i16, i16)> {
        let card_amp = self.card_amp;
        let speaker_volume = self.speaker_volume;
        let opl_native: Vec<(i32, i32)> = (0..native_samples)
            .map(|_| self.opl.render_sample())
            .collect();
        let opl_out = self.resampler.process(&opl_native);

        // HLE: drive DSP/WSS/CD sample counts from guest elapsed clocks * their
        // programmed (or fixed) rate when possible (Phase 4). The production in
        // advance_devices uses per-device phase accum (dsp/wss/cd_sample_phase)
        // so rings are filled by guest time. render drains by delta on elapsed_clocks
        // for rate matching. Falls back to OPL-scaled when no guest delta. This
        // decouples audio production from host render cadence and reduces drift.
        let clock_hz = self.profile().clock_hz;
        let delta = self.elapsed_clocks.saturating_sub(self.last_audio_clocks);
        self.last_audio_clocks = self.elapsed_clocks;
        self.sync_dsp_resampler();
        let dsp_native_count = if delta > 0 {
            let r = self.dsp.output_frame_rate() as u64;
            ((delta as f64 * r as f64 / clock_hz as f64).round() as usize).max(1)
        } else {
            (native_samples as f64 * self.dsp.output_frame_rate() as f64 / OPL_NATIVE_HZ as f64)
                .round() as usize
        };
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
            let wss_native_count = if delta > 0 {
                let r = self.wss.output_frame_rate() as u64;
                ((delta as f64 * r as f64 / clock_hz as f64).round() as usize).max(1)
            } else {
                (native_samples as f64 * self.wss.output_frame_rate() as f64 / OPL_NATIVE_HZ as f64)
                    .round() as usize
            };
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
                // Host PC speaker volume: a straight attenuation on the beeper,
                // independent of the card amp (0.0 mutes it). Unity leaves the mix
                // bit-identical to before.
                let s = (f32::from(spk[i]) * speaker_volume) as i32;
                let (cl, cr) = cd.get(i).copied().unwrap_or((0, 0));
                let cl = (cl as f32 * cd_l_gain) as i32;
                let cr = (cr as f32 * cd_r_gain) as i32;
                // OPL + SB16 DSP take the CT1745 master/outgain; the WSS codec and
                // CD are summed in raw (their own attenuation already applied). All
                // of these are ReSonique 2 card sources, so the analog output-stage
                // gain (`card_amp`) scales their sum. The PC speaker (`s`) is
                // motherboard hardware, not on the card, so it is added AFTER the
                // amp at its own level.
                // Sum the card sources exactly as before (SB16 part truncated, then
                // the raw WSS + CD adds), scale that by the analog amp, then add the
                // speaker. At card_amp == 1.0 this is bit-identical to the pre-amp
                // mix (`... as i32 + wl + s + cl`), since a whole f32 casts back
                // unchanged and integer addition commutes.
                let card_l = (((ol + dl) as f32 * (master_l * outgain_l)) as i32 + wl + cl) as f32
                    * card_amp;
                let card_r = (((or + dr) as f32 * (master_r * outgain_r)) as i32 + wr + cr) as f32
                    * card_amp;
                let l = clamp_i16(card_l as i32 + s);
                let r = clamp_i16(card_r as i32 + s);
                (l, r)
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
    /// is woken by any of four sources: IRQ0 (PIT channel 0 OUT edge), IRQ5 (the
    /// SB16 DSP half/end-buffer edge, clock-driven), the AD1848/WSS codec's
    /// terminal-count edge, or the Yamaha ADPCM-B block edge, the latter two on
    /// their own (config) IRQ lines. Each is considered only when
    /// unmasked/deliverable; the result is the soonest of the applicable wakes,
    /// clamped to the deadline and to at least one clock so the run loop always
    /// makes progress.
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

    /// The Approximate-class (486/586) batch cap: CPU clocks until the next due
    /// device event, instead of the Accurate class's one-DAC-sample lockstep.
    ///
    /// Contract. Interrupts are serviced at batch entry and devices advance at
    /// batch end, so this cap is what bounds Approximate-class IRQ latency:
    /// - the next PIT channel 0 (IRQ0) or channel 2 (speaker/game timing) OUT
    ///   rising edge ends the batch at (within a PIT tick of) its instant, so
    ///   timer-driven cadences hold;
    /// - the next DSP/WSS/ADPCM block-IRQ edge does the same for audio blocks,
    ///   which also keeps one guest interrupt per block edge (see the
    ///   multi-edge contract in advance_devices);
    /// - a ~1 ms ceiling (clock_hz / 1000) bounds the latency of everything
    ///   else (a line masked now and unmasked later, a declined estimator);
    /// - the DAC-sample floor means no batch is ever SHORTER than the Accurate
    ///   cap: a sub-sample edge estimate degrades to exactly the Accurate
    ///   class's up-to-one-sample delivery latency instead of shrinking
    ///   batches, and a degenerate estimator can never stall progress.
    ///
    /// PIT channel 1 is EXCLUDED deliberately: the power-on DRAM-refresh
    /// heartbeat (mode 2, reload 18, ~15 us) runs forever, so its term would
    /// bind every batch below even the Accurate cap and cancel this fast path
    /// outright. Its OUT is only guest-visible through a port 0x61 read, which
    /// ends the batch anyway. PIC masking is likewise ignored on purpose: an
    /// edge on a masked line latches IRR at the same advance either way, so the
    /// per-batch mask query buys no alignment.
    ///
    /// The PIT tick -> CPU clock conversion ignores the pit_clocks fractional
    /// accumulator (up to one PIT tick of skew; the same div_ceil idiom
    /// next_timer_wake already uses for the HLT wake) and the audio estimators
    /// are conservative for 16-bit stereo; both sit inside this class's license
    /// (results bit-exact, time approximate). Device-time exactness within a
    /// batch is unchanged: devices see the exact batch clock total, the speaker
    /// integrates PIT transitions sub-step, and the sample-phase producers emit
    /// exactly the frames the elapsed clocks call for, whatever the split.
    fn approx_batch_cap(&self, remaining: u64) -> u64 {
        let clock_hz = self.active_mode.clock_hz();
        // The ~1 ms latency ceiling.
        let mut cap = (clock_hz / 1000).max(1);
        // Next PIT OUT rising edge: channel 0 feeds IRQ0, channel 2 the
        // speaker/GATE timing games poll. (Channel 1: see above.)
        for channel in [0usize, 2] {
            if let Some(ticks) = self.pit.clocks_until_out_rise(channel) {
                let clocks = ((u128::from(ticks) * u128::from(clock_hz))
                    .div_ceil(u128::from(PIT_INPUT_HZ))) as u64;
                cap = cap.min(clocks);
            }
        }
        // Next audio block-IRQ edge. The DSP/WSS/ADPCM rates mirror the wake
        // estimators in next_timer_wake (the DSP block counter drains at the
        // raw byte/word rate, rate_hz; the WSS counter at its frame rate).
        if let Some(clocks) = self.dsp.clocks_until_next_irq(self.dsp.rate_hz(), clock_hz) {
            cap = cap.min(clocks);
        }
        if self.wss_enabled
            && let Some(clocks) = self.wss.clocks_until_next_irq(self.wss.rate_hz(), clock_hz)
        {
            cap = cap.min(clocks);
        }
        cap.max(self.timing.clocks_per_audio_sample).min(remaining)
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
            if self.direct_map_changed {
                self.cpu.note_direct_map_changed();
                self.direct_map_changed = false;
            }
            // pending_soft_int is posted at a stub LANDING (V86 or real mode), so
            // for a monitor-reflected V86 INT it is set only after the monitor has
            // IRETed back into V86 with the real-mode frame in place, and serviced
            // at that same batch's end. The ring-0 guard is kept defensively: if a
            // pending vector ever survives into a ring-0 monitor batch (a landing
            // interrupted before its break), preserve it until V86 resumes.
            if !self.cpu.is_ring0_protected() {
                self.pending_soft_int = None;
            }
            self.io_touched = false;
            self.device_wrote_memory = false;
            let trace_before = self.trace.elapsed_clocks();
            // Batch-entry snapshots for the Slice 1 lazy port-read prediction (P4a
            // Task 1.1). Captured here, before the fields below are moved into the
            // destructure, so they reflect live machine state at the moment this
            // batch's MachineBus is built (the one that matters for Slice 1).
            let elapsed_clocks_at_batch_start = self.elapsed_clocks;
            let vga_dots_at_batch_start = self.vga_dots;
            let beam_at_batch_start = self.video.beam_dots();
            let trace_elapsed_at_batch_start = trace_before;
            let bus_rem_at_batch_start = self.bus_rem;
            let inv_clock_at_batch_start = self.timing.inv_clock;
            let pit_clocks_at_batch_start = self.pit_clocks;
            let pit_per_clock_at_batch_start = self.timing.pit_per_clock;
            // bus_timing's (num, den), read from the SAME source scale_bus reads
            // from (self.cpu.level()) -- not cpu_level_for_mode(self.active_mode).
            // The two can diverge: the CPU's live level only tracks active_mode
            // from a set_mode (Lotura 0xE1) call onward; at construction the CPU
            // starts at its own default level until the first mode switch. Reading
            // active_mode here would silently mispredict during that window.
            let (bus_num_at_batch_start, bus_den_at_batch_start) = bus_timing(self.cpu.level());
            // Test seam: open this batch's per-run prior_runs_core_clocks push log.
            #[cfg(test)]
            self.test_prior_core_pushes.push(Vec::new());
            // A20 is a machine-layer event the CPU never sees directly, yet toggling it changes
            // which physical bytes back a linear address near the 1 MB wrap. Any A20 write (port
            // 0x92, the 8042, INT 15h, XMS) sets io_touched or is an HLE INT, so it ends this step;
            // a before/after compare here is the one seam that catches every source and lets the CPU
            // invalidate its prefetch + decode cache before the next batch runs.
            let a20_before = self.keyboard.a20_enabled();
            // Run a batch of straight-line instructions against one MachineBus,
            // then service devices once; a port access, an HLE INT, a HLT, or a
            // fault ends the batch sooner. This is the global-TSC / event-batched
            // model (research item 2.3): it drops the per-instruction bus rebuild
            // + 14-device fan-out that dominated the old loop.
            //
            // The batch cap is per timing class:
            // - Accurate (286/386): exactly one DAC sample of CPU time, so the
            //   per-clock fine-samplers stay in lockstep. BYTE-IDENTICAL
            //   contract (bench cyc/iter + aux, boot suite, device cadence):
            //   do not touch.
            // - Approximate (486/586): up to the next due device event, bounded
            //   by a ~1 ms latency ceiling and floored at the DAC-sample cap;
            //   approx_batch_cap holds the full contract. Batch splits move the
            //   f64 device accumulators through different partial sums, so
            //   device event instants may microshift against the Accurate
            //   splitting; that is licensed in this class (results stay
            //   bit-exact, time is approximate; see TimingClass). Computed once
            //   per batch entry: the run loop sits on a measured code-layout
            //   cliff, so nothing here may run per instruction.
            let remaining = deadline - self.elapsed_clocks;
            let cap = if matches!(self.active_mode.timing_class(), TimingClass::Approximate) {
                self.approx_batch_cap(remaining)
            } else {
                self.timing.clocks_per_audio_sample.min(remaining)
            };
            let cpu_batch_start = self.host_profile.start();
            let outcome = {
                let Machine {
                    profile,
                    active_mode,
                    pending_mode,
                    cpu,
                    cache_model,
                    memory,
                    ram_lookup,
                    video,
                    margo,
                    distira,
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
                    last_int_vector,
                    fast_post,
                    booter_inert,
                    program_runtime,
                    pending_toka_service,
                    toka_service_status,
                    unittester,
                    pci,
                    io_touched,
                    isa_io_batch_clocks,
                    device_wrote_memory,
                    direct_map_changed,
                    #[cfg(test)]
                    test_prior_core_pushes,
                    ..
                } = self;
                let mut bus = MachineBus {
                    memory,
                    ram_lookup,
                    video,
                    margo,
                    distira,
                    pci,
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
                    last_int_vector,
                    active_mode: *active_mode,
                    pending_mode,
                    fast_post: *fast_post,
                    booter_inert: *booter_inert,
                    program_runtime: *program_runtime,
                    pending_toka_service,
                    toka_service_status: *toka_service_status,
                    unittester,
                    wait_states: profile.wait_states,
                    cache: cache_model,
                    flat_data_cost: matches!(active_mode.timing_class(), TimingClass::Approximate),
                    lazy_port_reads: matches!(active_mode.timing_class(), TimingClass::Approximate),
                    io_touched,
                    isa_io_clocks: isa_io_batch_clocks,
                    device_wrote_memory,
                    direct_map_changed,
                    core_clocks_so_far: 0,
                    prior_runs_core_clocks: 0,
                    elapsed_clocks_at_batch_start,
                    vga_dots_at_batch_start,
                    beam_at_batch_start,
                    trace_elapsed_at_batch_start,
                    bus_rem_at_batch_start,
                    inv_clock_at_batch_start,
                    bus_num_at_batch_start,
                    bus_den_at_batch_start,
                    pit_clocks_at_batch_start,
                    pit_per_clock_at_batch_start,
                };
                // Collapse the batch into one CycleOutcome so every downstream
                // service step (device advance, CD stall, pending INT/mode/Toka/
                // unittester, console flush, HLT fast-forward) is unchanged:
                // core_clocks is the batch sum, halted is set iff the batch ended
                // on a HLT. core_clocks can't overflow u32 (the cap is at most
                // ~1 ms of guest clocks in the Approximate class, a few hundred
                // thousand at 586).
                let mut batch_core = 0u32;
                let mut halted = false;
                let mut fault = None;
                // Service a pending interrupt / halt-wake ONCE per batch.
                // interrupt_pending() cannot change mid-batch (devices advance only
                // after the batch, and any guest PIC access ends the batch via
                // io_touched), so a per-batch check is equivalent to the old
                // per-instruction one. The STI one-instruction shadow is still
                // honored per instruction inside cycle_no_interrupt_check.
                match cpu.service_pending_interrupt(&mut bus) {
                    Ok(Some(o)) => {
                        batch_core = batch_core.saturating_add(o.core_clocks);
                        if o.halted {
                            halted = true;
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        fault = Some(e);
                    }
                }
                if fault.is_none() && !halted {
                    loop {
                        // Watch the "a maskable interrupt is now serviceable" edge
                        // (IF set AND no STI shadow pending). When an instruction
                        // raises it - POPF/IRET enabling IF, or the instruction after
                        // STI consuming the shadow - end the batch so the next batch
                        // entry re-checks interrupts at exactly that boundary. The
                        // interrupt-pending check is per-batch, not per-instruction, so
                        // without this an IF-enable whose window closes inside the same
                        // batch loses its pending interrupt. Two load-bearing cases:
                        // the HLE WaitForKey retry (the IRET stub restores IF, then the
                        // re-run INT 21h clears it again in the same batch, so IRQ1
                        // would never run), and an `STI; poll; jz` idle loop whose
                        // cap boundary always lands right after the STI (the shadow
                        // would block the per-batch check forever).
                        let can_take_before = cpu.can_take_interrupt();
                        // The batch cap's contract is GUEST clocks (its PIT terms
                        // are "clocks until the next OUT edge"), but core_clocks
                        // alone under-counts a bus-heavy stretch: a framebuffer
                        // blit can be several bus clocks per core clock, so a
                        // core-only cap overshoots the next IRQ0 edge by that
                        // ratio and the PIC coalesces the missed edges - a guest
                        // timer ISR then loses ticks that a real PIT delivers
                        // (each edge interrupts long before the next at any
                        // realistic rate). Count the in-batch SCALED bus clocks
                        // toward the cap in the Approximate class, checked at
                        // loop top so an over-budget batch does not enter one
                        // more run. APPROXIMATE ONLY: the Accurate class (frozen
                        // 286/386) must keep not just the core-only comparison
                        // but the historical batch GEOMETRY - the old post-run
                        // check meant every batch executed at least one
                        // instruction even when the interrupt-service charge
                        // alone met the cap, and review showed the loop-top
                        // relocation changes that (a gate-invisible but real
                        // frozen-class delta). So Accurate skips this break and
                        // relies solely on the restored post-run check below.
                        let spent = u64::from(batch_core) + bus.in_batch_scaled_bus_clocks();
                        if spent >= cap
                            && matches!(bus.active_mode.timing_class(), TimingClass::Approximate)
                        {
                            break;
                        }
                        // Run a straight-line run of instructions inside the CPU in one call (the
                        // first via the normal single path, then cached straight-line continuations)
                        // instead of bouncing here per instruction. The run ends on a fault, halt, a
                        // non-straight-line / un-cached / page-crossing terminator, an interrupt-
                        // serviceable transition, or its cap. The batch-break checks below still run
                        // on the collapsed outcome: the executor's internal transition check ends the
                        // RUN at the edge, and the machine's check below ends the BATCH so the next
                        // batch services the interrupt. Both are needed.
                        let remaining = cap.saturating_sub(spent);
                        // Publish the batch-scoped core clocks accumulated so far
                        // (the interrupt-service charge + every prior run of this
                        // batch, exactly the core component the batch-end step
                        // will combine) so a lazy port-read prediction inside the
                        // coming run can add the RUN-scoped core_clocks_so_far on
                        // top and see a batch-total that is monotone across run
                        // boundaries. See MachineBus::prior_runs_core_clocks.
                        bus.prior_runs_core_clocks = u64::from(batch_core);
                        // Logs the bus field itself (not an independent `batch_core`
                        // read) so `batch_loop_publishes_prior_runs_core_clocks_before_every_run`
                        // actually fails if the store above is ever deleted or the
                        // publish drifts from the field a lazy prediction reads.
                        #[cfg(test)]
                        test_prior_core_pushes
                            .last_mut()
                            .expect("opened at batch entry")
                            .push(bus.prior_runs_core_clocks);
                        match cpu.run_straight_line(&mut bus, remaining) {
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
                                if !can_take_before && cpu.can_take_interrupt() {
                                    break;
                                }
                                // Historical post-run core-clock check: the sole
                                // cap break for the Accurate class (preserving
                                // its at-least-one-run batch geometry exactly);
                                // for Approximate the loop-top guest-clock check
                                // above fires first or at the same boundary.
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
                }
                match fault {
                    Some(e) => Err(e),
                    None => Ok(CycleOutcome {
                        core_clocks: batch_core,
                        halted,
                    }),
                }
            };
            self.host_profile
                .record(MachineProfilePhaseKind::CpuBatch, cpu_batch_start);

            match outcome {
                Ok(outcome) => {
                    // Test seam: the final core total the batch-end step consumes,
                    // parallel to this batch's test_prior_core_pushes entry.
                    #[cfg(test)]
                    self.test_batch_core_totals
                        .push(u64::from(outcome.core_clocks));
                    let bus_clocks = self.trace.elapsed_clocks() - trace_before;
                    // Scale the bus portion per mode (B-T10). core_clocks is already
                    // scaled by the CPU's level_timing; this applies the third lever
                    // to the fetch + data-access bus clocks so a fast part pulls away
                    // from the flat per-access floor.
                    // ISA I/O bus time for the OPL status poll (Approximate class
                    // only), accumulated per access in read_io. The ISA bus runs at a
                    // fixed ~8 MHz, so an OPL status poll costs about a microsecond of
                    // wall time no matter how fast the CPU is.
                    // The per-mode bus scaler (scale_bus) instead prices the whole bus
                    // portion DOWN in the fast modes (586 x7/30), driving a port access
                    // toward zero guest-clocks, so a tight poll loop retires thousands
                    // of iterations per microsecond. That silently breaks the AdLib
                    // timer detection Doom runs before enabling FM music: the poll
                    // outruns the 80 us OPL timer, the overflow bit never appears, and
                    // music is disabled. Charging the real ISA period per poll lets the
                    // timer overflow within the poll. This is added OUTSIDE the
                    // io_touched batch-end gate on purpose: under TOKAEMM the poll runs
                    // in the V86 monitor (ring-0 PM), where the monitor's own device
                    // pokes are deliberately exempted from io_touched, so gating on it
                    // would miss exactly the case that fails. The Accurate class
                    // (286/386) never accumulates this (see read_io), so it stays
                    // byte-identical; its slower clock already spans the 80 us window.
                    let step = u64::from(outcome.core_clocks)
                        + self.scale_bus(bus_clocks)
                        + std::mem::take(&mut self.isa_io_batch_clocks);
                    self.elapsed_clocks += step;
                    // Advance the OPL timers so AdLib detection's delay loops see
                    // the overflow flag (the synthesis clock is driven separately
                    // by `render_audio`).
                    let advance_start = self.host_profile.start();
                    self.advance_devices(step);
                    self.host_profile
                        .record(MachineProfilePhaseKind::AdvanceDevices, advance_start);
                    // Charge the CD-ROM's seek + transfer time for a read the
                    // instruction just issued, the way the floppy stalls. The
                    // guest clock jumps; the GUI's realtime pacing turns that into
                    // a visible wait.
                    let cd_secs = self.ide.take_stall_secs();
                    if cd_secs > 0.0 {
                        let cd_start = self.host_profile.start();
                        self.stall_for(cd_secs);
                        self.host_profile
                            .record(MachineProfilePhaseKind::CdStall, cd_start);
                    }
                    let service_start = self.host_profile.start();
                    let mut serviced = false;
                    let mut service_stop = None;
                    if let Some(mode) = self.pending_mode.take() {
                        serviced = true;
                        self.set_mode(mode); // live Lotura switch takes effect next instruction
                    }
                    if let Some(cmd) = self.pending_toka_service.take() {
                        serviced = true;
                        self.perform_toka_service(cmd); // Repair (cmd 0x01)
                    }
                    if let Some(cmd) = self.unittester.take_pending() {
                        serviced = true;
                        if let Some(code) = self.perform_unittester(cmd) {
                            service_stop = Some(StopReason::TestExit { code });
                        }
                    }
                    // A software INT taken by a V86 guest faults to the TOKAEMM monitor
                    // (ring-0 PM) before its frame is reflected onto the guest stack. The
                    // HLE BIOS services assume that real-mode-style frame at SS:SP+4 (see
                    // `set_int_frame_carry`), so defer them while the monitor runs; they
                    // fire once it IRETs back into V86 with the frame in place.
                    if service_stop.is_none()
                        && !self.cpu.is_ring0_protected()
                        && let Some(vector) = self.pending_soft_int
                    {
                        serviced = true;
                        match vector {
                            0x10 | 0x42 => self.handle_int10(),
                            0x11 => self.handle_int11(),
                            0x12 => self.handle_int12(),
                            0x13 | 0x40 => self.handle_int13(),
                            0x14 => self.handle_int14(),
                            0x15 => self.handle_int15(),
                            0x17 => self.handle_int17(),
                            0x18 => self.handle_int18(),
                            0x19 => self.handle_int19(),
                            0x1A => self.handle_int1a(),
                            0x5C => self.handle_absent_resident_api(0x5C),
                            0x60 => self.handle_absent_resident_api(0x60),
                            0x68 => self.handle_absent_resident_api(0x68),
                            0x6F => self.handle_absent_resident_api(0x6F),
                            0x7A => self.handle_absent_resident_api(0x7A),
                            0x86 => self.handle_absent_resident_api(0x86),
                            0xE4 => self.handle_absent_resident_api(0xE4),
                            0x2F => {
                                self.handle_int2f();
                            }
                            0x20 | 0x21 | 0x27 if self.program_runtime => {
                                match self.handle_raw_program_int(vector) {
                                    Ok(Some(code)) => {
                                        service_stop = Some(StopReason::DosExit { code });
                                    }
                                    Ok(None) => {}
                                    Err(error) => {
                                        service_stop = Some(StopReason::CpuError(format!(
                                            "raw program INT {vector:#04x}: {error}"
                                        )));
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    if serviced {
                        self.host_profile
                            .record(MachineProfilePhaseKind::SoftInt, service_start);
                    }
                    if let Some(stop) = service_stop {
                        return Ok(stop);
                    }
                    // Mirror any DOS console output onto the VGA text screen.
                    let console_start = self.host_profile.start();
                    self.flush_dos_console_to_screen();
                    self.host_profile
                        .record(MachineProfilePhaseKind::ConsoleFlush, console_start);
                    if outcome.halted {
                        let halt_start = self.host_profile.start();
                        match self.next_timer_wake(deadline) {
                            Some(wake_step) => {
                                self.elapsed_clocks += wake_step;
                                self.advance_devices(wake_step);
                                self.host_profile
                                    .record(MachineProfilePhaseKind::HaltFastForward, halt_start);
                            }
                            None => {
                                self.host_profile
                                    .record(MachineProfilePhaseKind::HaltFastForward, halt_start);
                                return Ok(StopReason::Halted);
                            }
                        }
                    }
                    // The A20 gate toggled during this step (port 0x92, the 8042, INT 15h, or XMS):
                    // tell the CPU so it drops any prefetch/decoded bytes that A20 now remaps near
                    // the 1 MB wrap, before the next batch executes against the new gate state.
                    if self.keyboard.a20_enabled() != a20_before {
                        self.cpu.note_a20_changed();
                    }
                    // A device wrote guest RAM this step (a DMA disk/floppy transfer or block copy),
                    // bypassing the CPU's SMC tracking; drop the prefetch + decode cache so staged
                    // code is re-decoded rather than replayed stale on a later near branch into it.
                    if self.device_wrote_memory {
                        self.cpu.note_device_memory_write();
                    }
                    if self.direct_map_changed {
                        self.cpu.note_direct_map_changed();
                        self.direct_map_changed = false;
                    }
                }
                Err(error) => {
                    if fault_trace_enabled() {
                        self.log_fault_trace(&error);
                    }
                    return Ok(StopReason::CpuError(error.to_string()));
                }
            }
        }

        Ok(StopReason::CycleLimit { requested })
    }
}

struct MachineBus<'a> {
    memory: &'a mut Memory,
    ram_lookup: &'a mut RamPageLookup,
    video: &'a mut Vga,
    margo: &'a mut Margo,
    distira: &'a mut Distira,
    pci: &'a mut PciConfig,
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
    last_int_vector: &'a mut Option<u8>,
    active_mode: GswMode,                  // a copy, for the 0xE1 read
    pending_mode: &'a mut Option<GswMode>, // a 0xE1 write records the request here
    fast_post: bool,                       // a copy, for the 0xE2 POST-pacing read
    booter_inert: bool,                    // a copy, stands the multiplex vectors down at INT-ack
    // A copy: when set (a `new_raw_program` machine) INT 20h/21h/27h stay
    // intercepted at INT-ack so the run loop's guarded raw-program arm services them.
    program_runtime: bool,
    pending_toka_service: &'a mut Option<u8>, // a 0xE3 write records the command
    toka_service_status: u8,                  // a copy, for the 0xE3 status read
    unittester: &'a mut unittester::UnitTester, // Lotura ports 0xE4-0xE6
    wait_states: WaitStateProfile,
    // The cache model carries the active CPU level's geometry/cost. A data access
    // warms it via data_access_wait_states, and the resolved tier's calibrated cost
    // is the charged wait-state.
    cache: &'a mut CacheModel,
    /// True in the Approximate timing class (486/586): data accesses charge a flat
    /// cost and skip the per-access cache-tier tag arrays. False in the Accurate
    /// class (286/386) and forced false by the bandwidth diagnostic so its tier
    /// curve stays on the accurate model.
    flat_data_cost: bool,
    /// True in the Approximate timing class (486/586), computed identically to
    /// `flat_data_cost` (same `active_mode.timing_class()` match, same
    /// construction sites). Gates the lazy 3DA/3BA/3C2 dispatch in `read_io`
    /// (P4a Task 1.3): when true, a status-port read does not set `io_touched`
    /// and computes its returned bits from `predicted_beam()` instead of the
    /// live device beam; when false (Accurate class, 286/386) the port keeps
    /// the byte-identical pre-Task-1.3 behavior. A single bool test at the top
    /// of the one arm that branches on it, not a per-access classification.
    lazy_port_reads: bool,
    // Set true by any port I/O this batch. The run loop batches straight-line
    // instructions and services devices once per batch; a port access (a PIT
    // latch read, 0x3DA retrace poll, RTC read, a PIT/PIC/DSP/mode write) reads
    // or changes time-dependent device state, so it ends the batch to keep that
    // state exact. Memory/MMIO (framebuffer blits, the hot path) does not set it.
    io_touched: &'a mut bool,
    // Accrues fixed ISA-bus time (CPU clocks) for the OPL status poll in the
    // Approximate class; the run loop folds it into the batch's device advance.
    // Points at `Machine::isa_io_batch_clocks`.
    isa_io_clocks: &'a mut u64,
    device_wrote_memory: &'a mut bool,
    direct_map_changed: &'a mut bool,
    // A copy of the current read_io call's core_clocks_so_far argument (CPU core
    // clocks charged by prior instructions in this straight-line run, not
    // including the in-flight IN). Written at the top of every read_io call so a
    // future lazy-port arm can read it without its own plumbing. Not read by any
    // arm yet; Slice 0 is pure seam-threading (dev_docs/2026-07-02-p4a-lazy-port-
    // device-time-plan.md Task 0.2). Initialized to 0 at bus construction; the
    // first read_io call overwrites it before any arm can observe it.
    core_clocks_so_far: u64,
    // CPU core clocks accumulated by everything BEFORE the current straight-line
    // run of this batch: the once-per-batch interrupt-service charge plus every
    // completed run_straight_line call's core_clocks. `core_clocks_so_far` above
    // is RUN-scoped (it resets to 0 at the first instruction of every run), but a
    // batch chains many runs and only the batch total feeds the batch-end `step`;
    // a prediction that used the run-scoped term alone would silently drop the
    // earlier runs' core clocks and go non-monotone across run boundaries.
    // Initialized 0 at batch entry; the batch loop rewrites it with the live
    // `batch_core` accumulator immediately before each run_straight_line call, so
    // `prior_runs_core_clocks + core_clocks_so_far` is always the batch-scoped
    // core total as of the in-flight instruction, mirroring exactly the core
    // component of the batch-end step (nothing more, nothing less).
    prior_runs_core_clocks: u64,
    // Five batch-entry snapshots for the Slice 1 lazy port-read prediction (P4a
    // Task 1.1: dev_docs/2026-07-02-p4a-lazy-port-device-time-plan.md). Each is a
    // copy of the corresponding live Machine/BusTrace value at the moment this
    // bus is constructed (once per batch), never mutated afterward.
    // `vga_dots_at_batch_start`/`beam_at_batch_start`/`trace_elapsed_at_batch_start`/
    // `bus_rem_at_batch_start` are consumed by `predicted_beam`, which the lazy
    // 3DA/3BA/3C2 arm in `read_io` calls (Task 1.3). `elapsed_clocks_at_batch_start`
    // is not needed by that formula (predicted_beam derives its clock total from
    // core_clocks + trace bus clocks directly, not from elapsed_clocks); it stays
    // for construction-site symmetry and is pinned directly by
    // `predicted_beam_at_batch_start_equals_the_unmutated_beam`.
    #[allow(dead_code)] // pinned by its own test, not read by predicted_beam's formula
    elapsed_clocks_at_batch_start: u64,
    vga_dots_at_batch_start: f64,
    beam_at_batch_start: u64,
    trace_elapsed_at_batch_start: u64,
    bus_rem_at_batch_start: u64,
    // The active mode's `1 / clock_hz` factor (Machine::timing.inv_clock), copied
    // at bus construction like the five batch-entry snapshots above. Needed by
    // `predicted_beam` to call the shared `predict_dots_core` formula; MachineBus
    // has no `&Machine` to read `self.timing` from directly. A copy field rather
    // than a per-call recompute: `TimingFactors::for_clock` only changes on a
    // Lotura mode write (`set_mode`), so it is batch-entry-stable exactly like
    // `active_mode` above it.
    inv_clock_at_batch_start: f64,
    // bus_timing(cpu.level())'s (num, den) ratio, copied at bus construction from
    // the SAME source `scale_bus` reads (`self.cpu.level()`), NOT derived from
    // `active_mode` here: the CPU's live level only tracks `active_mode` from a
    // `set_mode` call onward, so at construction (before any Lotura 0xE1 write)
    // the two can disagree. `predicted_beam` must scale in-batch bus clocks with
    // exactly this ratio to match what the real end-of-batch `scale_bus` call
    // will use.
    bus_num_at_batch_start: u32,
    bus_den_at_batch_start: u32,
    // The fractional PIT-input-clock accumulator (Machine::pit_clocks), copied at
    // bus construction like the snapshots above (P4a Task 2.3: the lazy port 0x61
    // bits 4/5 read). `advance_devices` is the only place that mutates
    // `pit_clocks` or steps `self.pit`, and it only runs at batch end / wake step
    // (never mid-batch), so this snapshot plus the in-batch clock total
    // (identical construction to `predicted_beam`'s) is enough to reproduce
    // exactly the elapsed-PIT-clocks `whole` value the real `advance_devices`
    // would compute for the same total, via the shared `advance_fractional`
    // formula.
    pit_clocks_at_batch_start: f64,
    // The active mode's PIT_INPUT_HZ / clock_hz factor (Machine::timing.
    // pit_per_clock), copied at bus construction like `inv_clock_at_batch_start`
    // above and for the same reason (batch-entry-stable, only a Lotura mode
    // write recomputes TimingFactors). Snapshotted RATHER than recomputed from
    // PIT_INPUT_HZ and inv_clock: the real `advance_devices` multiplies by this
    // exact pre-divided f64, and re-deriving it as `PIT_INPUT_HZ as f64 *
    // inv_clock` is a DIFFERENT factoring whose product floor-diverges from the
    // real one at the IEEE-f64 level (see `advance_fractional`'s doc comment).
    pit_per_clock_at_batch_start: f64,
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

/// The port each byte of a wider-than-byte I/O cycle targets. The IDE/ATA 16-bit
/// data registers (primary `0x1F0`, secondary `0x170`) stream every byte through
/// the same port via their data FIFO, so a word/dword access repeats the port.
/// Every other (8-bit-decoded) port takes consecutive bytes at `port`, `port+1`,
/// ... - exactly the VGA index/data-pair behaviour a single 16-bit `OUT` to
/// `0x3C4`/`0x3CE`/`0x3D4` relies on to set an index and its datum at once.
const fn io_word_sub_port(port: u16, index: u32) -> u16 {
    if port == ata::PRIMARY_CMD_BASE || port == ide::SECONDARY_CMD_BASE {
        port
    } else {
        port.wrapping_add(index as u16)
    }
}

impl CpuBus for MachineBus<'_> {
    fn read_memory_direct(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<DirectMemoryRead, BusError> {
        if let Some((address, start, end)) =
            self.direct_page_ram_bytes(address, width.bytes() as usize, width)
        {
            let ws = self.data_access_wait_states(address, width);
            self.trace.record(kind, address, width, ws);
            let data = &self.memory.as_slice()[start..end];
            let value = match width {
                BusWidth::Byte => u32::from(data[0]),
                BusWidth::Word => u32::from(u16::from_le_bytes([data[0], data[1]])),
                BusWidth::Dword => u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            };
            return Ok(DirectMemoryRead {
                value,
                direct: true,
            });
        }
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
        if let Some((address, start, _)) =
            self.direct_page_ram_bytes(address, width.bytes() as usize, width)
        {
            let ws = self.data_access_wait_states(address, width);
            self.trace.record(kind, address, width, ws);
            match width {
                BusWidth::Byte => self.memory.write_u8(start, value as u8)?,
                BusWidth::Word => self.memory.write_u16(start, value as u16)?,
                BusWidth::Dword => self.memory.write_u32(start, value)?,
            }
            return Ok(DirectMemoryWrite { direct: true });
        }
        self.write_memory(address, width, value, kind)
            .map(|()| DirectMemoryWrite { direct: false })
    }

    fn read_memory_bytes_direct(
        &mut self,
        address: u32,
        out: &mut [u8],
        access_width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<usize, BusError> {
        if out.is_empty() {
            return Ok(0);
        }
        let access = access_width.bytes() as usize;
        if out.len() % access != 0 {
            return Ok(0);
        }
        let Some((address, start, end)) =
            self.direct_page_ram_bytes(address, out.len(), access_width)
        else {
            return Ok(0);
        };
        for offset in (0..out.len()).step_by(access) {
            let at = address + offset as u32;
            let ws = self.data_access_wait_states(at, access_width);
            self.trace.record(kind, at, access_width, ws);
        }
        out.copy_from_slice(&self.memory.as_slice()[start..end]);
        Ok(out.len())
    }

    fn write_memory_bytes_direct(
        &mut self,
        address: u32,
        data: &[u8],
        access_width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<usize, BusError> {
        if data.is_empty() {
            return Ok(0);
        }
        let access = access_width.bytes() as usize;
        if data.len() % access != 0 {
            return Ok(0);
        }
        let Some((address, start, end)) =
            self.direct_page_ram_bytes(address, data.len(), access_width)
        else {
            return Ok(0);
        };
        for offset in (0..data.len()).step_by(access) {
            let at = address + offset as u32;
            let ws = self.data_access_wait_states(at, access_width);
            self.trace.record(kind, at, access_width, ws);
        }
        self.memory.as_mut_slice()[start..end].copy_from_slice(data);
        Ok(data.len())
    }

    fn direct_memory_bytes(&self, address: u32, bytes: usize, access_width: BusWidth) -> usize {
        self.direct_page_ram_bytes(address, bytes, access_width)
            .map_or(0, |(_, start, end)| end - start)
    }

    #[inline]
    fn direct_page(
        &mut self,
        address: u32,
        kind: BusAccessKind,
    ) -> Result<Option<DirectPage>, BusError> {
        let gated = self.apply_a20(address);
        if gated != address {
            return Ok(None);
        }
        let physical_page = gated & !(RAM_LOOKUP_PAGE_MASK as u32);
        let Some((start, end)) = self.direct_ram_bytes(physical_page, RAM_LOOKUP_PAGE_SIZE) else {
            return Ok(None);
        };
        if end - start != RAM_LOOKUP_PAGE_SIZE {
            return Ok(None);
        }
        Ok(Some(DirectPage {
            physical_page,
            ptr: unsafe { self.memory.as_mut_ptr().add(start) },
            len: RAM_LOOKUP_PAGE_SIZE,
            writable: matches!(kind, BusAccessKind::DataWrite),
        }))
    }

    #[inline]
    fn charge_direct_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<(), BusError> {
        // Only the CPU's DirectPageCache fast paths call this, and a live entry
        // guarantees cacheable RAM under the current A20 state: `direct_page`
        // installs a page only when `apply_a20` is the identity for it (an A20
        // toggle then invalidates the cache via note_a20_changed), and the
        // direct map never covers a device window. Conventional pages sit below
        // the 0xA0000 aperture; extended pages start at 1 MiB, exclude the
        // Distira BAR (whose decode changes rebuild the map AND invalidate the
        // cache), and system RAM ends below the Margo LFB/MMIO and high-ROM
        // bases. So in the Approximate class (`flat_data_cost`) the charge is
        // always the flat L1 cost: skip apply_a20 and the wait-state routing.
        // The Accurate class keeps the full path so its tag arrays stay warm.
        //
        // Accepted residue: a same-instruction REP OUTS that moves the Distira
        // BAR over its own source buffer keeps charging the stale entry's flat
        // cost until the post-instruction io_touched step break invalidates it.
        // That divergence is timing-only; functional behavior is identical.
        if self.flat_data_cost {
            self.trace.record(kind, address, width, self.cache.cost.l1);
            return Ok(());
        }
        let address = self.apply_a20(address);
        let ws = self.data_access_wait_states(address, width);
        self.trace.record(kind, address, width, ws);
        Ok(())
    }

    /// One instruction-fetch access of cacheable RAM: `clocks_for(_, code_fetch_wait_states)` = 2 +
    /// the per-mode I-cache constant. Matches what `charge_instruction_fetch_run`'s cacheable-RAM
    /// fast path records for one access (machine.rs ~9806). The JIT cost-fold folds this per slot.
    fn jit_fetch_cost_clocks(&self) -> u64 {
        2 + u64::from(self.cache.code_fetch_wait_states())
    }

    /// One byte-wide direct data access: `clocks_for(Byte, cost.l1)` = 2 + the flat L1 wait-state,
    /// exactly what `charge_direct_memory` records for a direct-page hit in the Approximate class.
    fn jit_data_byte_cost_clocks(&self) -> u64 {
        2 + u64::from(self.cache.cost.l1)
    }

    /// Flush the JIT cost-fold's accumulated bus clocks into the trace's running total in one op.
    fn charge_bus_clocks_bulk(&mut self, clocks: u64) {
        self.trace.add_elapsed_clocks(clocks);
    }

    /// See the trait doc: the straight-line run loop adds this figure's growth
    /// to its core total against the (guest-clock) run cap. Approximate class
    /// only; the Accurate class returns 0 so its lockstep batches keep the
    /// historical core-only check bit-for-bit (frozen 286/386 byte-identity).
    /// Same arithmetic as `in_batch_clocks` minus the core terms the CPU
    /// already tracks itself.
    fn in_batch_scaled_bus_clocks(&self) -> u64 {
        if matches!(self.active_mode.timing_class(), TimingClass::Accurate) {
            return 0;
        }
        let raw = self.trace.elapsed_clocks() - self.trace_elapsed_at_batch_start;
        (raw * u64::from(self.bus_num_at_batch_start) + self.bus_rem_at_batch_start)
            / u64::from(self.bus_den_at_batch_start)
    }

    fn read_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<u32, BusError> {
        let address = self.apply_a20(address);
        let bytes = width.bytes() as usize;

        if let Some(offset) = self.distira_lfb_offset(address, bytes) {
            let ws = self.data_access_wait_states(address, width);
            self.trace.record(kind, address, width, ws);
            let offset = if width == BusWidth::Byte {
                offset
            } else {
                offset & !1
            };
            return Ok(match width {
                BusWidth::Byte => 0xff,
                BusWidth::Word => u32::from(u16::from_le_bytes([
                    self.distira.read_lfb_u8(offset),
                    self.distira.read_lfb_u8(offset + 1),
                ])),
                BusWidth::Dword => u32::from_le_bytes([
                    self.distira.read_lfb_u8(offset),
                    self.distira.read_lfb_u8(offset + 1),
                    self.distira.read_lfb_u8(offset + 2),
                    self.distira.read_lfb_u8(offset + 3),
                ]),
            });
        }

        if should_split(address, width) {
            let mut value = 0u32;
            for offset in 0..width.bytes() {
                let byte = self.read_memory(address + offset, BusWidth::Byte, kind)?;
                value |= byte << (offset * 8);
            }
            return Ok(value);
        }

        if let Some((start, end)) = self.direct_ram_range(address, width) {
            let ws = self.data_access_wait_states(address, width);
            self.trace.record(kind, address, width, ws);
            let data = &self.memory.as_slice()[start..end];
            return Ok(match width {
                BusWidth::Byte => u32::from(data[0]),
                BusWidth::Word => u32::from(u16::from_le_bytes([data[0], data[1]])),
                BusWidth::Dword => u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            });
        }

        let ws = self.data_access_wait_states(address, width);
        self.trace.record(kind, address, width, ws);

        let mut data = [0u8; 4];
        self.read_phys(address, &mut data[..bytes])?;
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
        if let Some(offset) = self.distira_lfb_offset(address, width.bytes() as usize) {
            let ws = self.data_access_wait_states(address, width);
            self.trace.record(kind, address, width, ws);
            let offset = if width == BusWidth::Byte {
                offset
            } else {
                offset & !1
            };
            match width {
                BusWidth::Byte => {}
                BusWidth::Word => self.distira.write_lfb_u16(offset, value as u16),
                BusWidth::Dword => self.distira.write_lfb_u32(offset, value),
            }
            return Ok(());
        }

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

        if let Some((start, _)) = self.direct_ram_range(address, width) {
            let ws = self.data_access_wait_states(address, width);
            self.trace.record(kind, address, width, ws);
            return match width {
                BusWidth::Byte => self.memory.write_u8(start, value as u8),
                BusWidth::Word => self.memory.write_u16(start, value as u16),
                BusWidth::Dword => self.memory.write_u32(start, value),
            };
        }

        let ws = self.data_access_wait_states(address, width);
        self.trace.record(kind, address, width, ws);

        if let Some(offset) = self.distira_cmdfifo_offset(address, width.bytes() as usize) {
            if width == BusWidth::Dword {
                self.distira.write_command_fifo_u32(offset, value);
            }
            return Ok(());
        }

        if let Some(offset) = self.distira_texture_offset(address, width.bytes() as usize) {
            if width == BusWidth::Dword {
                self.distira.write_texture_u32(offset, value);
            }
            return Ok(());
        }

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

    fn prefetch_memory(&mut self, address: u32, out: &mut [u8]) -> Result<usize, BusError> {
        let address = self.apply_a20(address);
        // Fast path: a prefetch window entirely within conventional RAM (below
        // the 0xA0000 video aperture) is one bounded slice copy instead of a
        // gauntlet-walking read_phys_u8 per byte.
        let ram_end = address as usize + out.len();
        if ram_end <= 0x000A_0000 && ram_end <= self.memory.len() {
            out.copy_from_slice(&self.memory.as_slice()[address as usize..ram_end]);
            return Ok(out.len());
        }
        let mut copied = 0;
        for (offset, byte) in out.iter_mut().enumerate() {
            match self.read_phys_u8(address + offset as u32) {
                Ok(value) => {
                    *byte = value;
                    copied += 1;
                }
                Err(BusError::UnmappedMemory { .. }) if copied > 0 => break,
                Err(err) => return Err(err),
            }
        }
        Ok(copied)
    }

    #[inline]
    fn note_code_fetch_linear(&mut self, linear: u32) {
        // One range compare (0xFF000..0xFF400: the legacy FF00:0000 target
        // through the end of the per-vector stub table) keeps this out of the
        // way of every ordinary fetch.
        if linear.wrapping_sub(BIOS_LEGACY_IRET_LINEAR) < BIOS_STUB_WINDOW_LEN {
            self.note_stub_fetch(linear);
        }
    }

    fn charge_instruction_fetch(&mut self, address: u32) -> Result<(), BusError> {
        let address = self.apply_a20(address);
        let ws = self.code_fetch_wait_states(address);
        self.trace.record(
            BusAccessKind::InstructionPrefetch,
            address,
            BusWidth::Byte,
            ws,
        );
        Ok(())
    }

    fn charge_instruction_fetch_run(&mut self, start: u32, count: u32) -> Result<(), BusError> {
        if count == 0 {
            return Ok(());
        }
        // Stub recognition, run-charge seam: the trigger check runs on the
        // run's START address only (a stub entry is always a fresh run:
        // execution arrives by IVT far transfer or IRET return, never by
        // falling through). `start` here is the run's LINEAR address - the
        // same domain `note_code_fetch_linear` observes on the cold path.
        if start.wrapping_sub(BIOS_LEGACY_IRET_LINEAR) < BIOS_STUB_WINDOW_LEN {
            self.note_stub_fetch(start);
        }
        // Fast path: a run that lies entirely in conventional RAM (below the
        // 0xA0000 video aperture). Every address below 0x100000 has bit 20 clear,
        // so `apply_a20` is the identity there regardless of the gate state;
        // `code_fetch_wait_states` is the per-mode I-cache constant for any
        // address below 0xA0000 (the device-window gate only engages at or above
        // it); and a contiguous run below 0xA0000 is uniform by construction.
        // The classification below would therefore always land in the uniform
        // cacheable-RAM arm and charge ONE I-cache access at the constant
        // wait-state, so charge exactly that in one step. ROM/device/A20-edge
        // runs keep the full classification, byte-for-byte.
        if let Some(end) = start.checked_add(count - 1) {
            if end < 0x000A_0000 {
                self.trace.record_instruction_fetch_run(
                    start,
                    1,
                    self.cache.code_fetch_wait_states(),
                );
                return Ok(());
            }
        }
        let first = self.apply_a20(start);
        let last = self.apply_a20(start.wrapping_add(count - 1));
        let first_ws = self.code_fetch_wait_states(first);
        // Uniform iff every byte lands in the same wait-state region with no A20 wrap
        // between the ends. apply_a20 already folded both ends, so equal wait-states on
        // contiguous post-A20 addresses means the whole run is one region. The endpoint-only
        // test relies on `count` being one instruction's length (at most 15 bytes), far smaller
        // than any wait-state region: a narrower region wholly contained between two matching
        // endpoints cannot exist at that scale. A caller passing a large `count` must not assume
        // this holds; the non-uniform branch's exact per-byte loop is the safe fallback regardless.
        let uniform =
            last == first.wrapping_add(count - 1) && first_ws == self.code_fetch_wait_states(last);
        if uniform {
            // I-cache model: an instruction whose bytes lie in cacheable RAM is
            // delivered by the I-cache in ONE bus access, not one per byte. The
            // per-byte bus cost (>= 2 clocks/byte) is a slow-bus artifact; on a part
            // with an instruction cache a hit returns the whole (pre-decoded) line in
            // a single fetch. Charging per byte here floors every mode's Dhrystone/
            // Sieve far below its era band (the floor is the same clocks in every
            // mode, so the fast modes can never separate). One access per instruction
            // makes the bands reachable for the slower modes and lifts the fast modes
            // toward (though not all the way to, see bench_reference.rs) their targets.
            //
            // ROM / device code (uncached) keeps the exact per-byte charge: those
            // windows are not I-cached, so `is_device_window` routes them to the
            // per-byte loop below to preserve firmware/POST and device-execution
            // timing unchanged.
            if first >= 0x000A_0000 && self.is_device_window(first, BusWidth::Byte) {
                self.trace
                    .record_instruction_fetch_run(first, count, first_ws);
            } else {
                // Single I-cache access for the whole instruction run.
                self.trace.record_instruction_fetch_run(first, 1, first_ws);
            }
        } else {
            for i in 0..count {
                self.charge_instruction_fetch(start.wrapping_add(i))?;
            }
        }
        Ok(())
    }

    fn read_io(
        &mut self,
        port: u16,
        width: BusWidth,
        core_clocks_so_far: u64,
        cpu_is_ring0_pm: bool,
    ) -> Result<u32, BusError> {
        // A copy, mirroring `active_mode`: available to any lazy-read arm without
        // re-threading the parameter, per dev_docs/2026-07-02-p4a-lazy-port-device-
        // time-plan.md Task 0.2. Read by the lazy 3DA/3BA/3C2 arm below (Task 1.3).
        self.core_clocks_so_far = core_clocks_so_far;
        // Ring-0-monitor port-time exemption (V86 trap tax, Part 1): the TOKAEMM
        // monitor's own device pokes (the vec13 discriminator's PIC OCW3 probe,
        // chiefly) are chipset-side bookkeeping done on the guest's behalf, not
        // guest-visible device activity in their own right. Ending the CPU batch
        // around them (the normal io_touched contract) triples the guest-visible
        // cost of every V86 trap for no fidelity gain: device time is still exact
        // because the batch still ends at the next approx_batch_cap edge or the
        // next GUEST port access, and OCW3's read-select is pure register state
        // (see pic.rs -- `read_isr` is a mode bit, not time-derived), so deferring
        // exactly when it is consumed relative to batch-end timing is safe. Gated
        // on `lazy_port_reads` (Approximate class only, i.e. 486/586): the
        // Accurate class (286/386) keeps byte-identical batch semantics, matching
        // every other P4a lazy gate in this function.
        let skip_io_touched = cpu_is_ring0_pm && self.lazy_port_reads;
        // Bus-clock trace recording stays unconditional for every port, both timing
        // classes: `predicted_beam`'s bus term scales exactly the clocks recorded
        // here, so a lazy read that skipped this would under-predict its own beam.
        self.trace.record(
            BusAccessKind::IoRead,
            u32::from(port),
            width,
            self.wait_states.io,
        );

        if let Some(value) = self.pci.read_io(port, width) {
            if !skip_io_touched {
                *self.io_touched = true;
            }
            return Ok(value);
        }

        if width != BusWidth::Byte {
            // A wider-than-byte port access decomposes into byte cycles, the way the
            // ISA bus does for a port that is not 16-bit: the low byte comes from the
            // port and each higher byte from the next port (`io_word_sub_port` keeps
            // the IDE/ATA data registers on the same port). This is the canonical VGA
            // mode-set path - a single 16-bit `OUT 0x3C4`/`0x3CE`/`0x3D4` sets an
            // index and its datum - which used to halt the VM with WidthMismatch.
            // Per-byte io_touched/lazy dispatch happens in the recursive calls below
            // (each byte re-enters read_io), so nothing to set here directly.
            let mut value = 0u32;
            for i in 0..width.bytes() {
                let byte = self.read_io(
                    io_word_sub_port(port, i),
                    BusWidth::Byte,
                    core_clocks_so_far,
                    cpu_is_ring0_pm,
                )?;
                value |= (byte & 0xff) << (8 * i);
            }
            return Ok(value);
        }

        if self.video_io_disabled_for_port(port) {
            if !skip_io_touched {
                *self.io_touched = true;
            }
            return Ok(0xff);
        }

        if let Some(value) = self.serial.read_port(port) {
            if !skip_io_touched {
                *self.io_touched = true;
            }
            return Ok(u32::from(value));
        }
        if let Some(value) = self.serial2.read_port(port) {
            if !skip_io_touched {
                *self.io_touched = true;
            }
            return Ok(u32::from(value));
        }
        if let Some(value) = self.lpt.read_port(port) {
            if !skip_io_touched {
                *self.io_touched = true;
            }
            return Ok(u32::from(value));
        }
        if let Some(value) = self.lpt2.read_port(port) {
            if !skip_io_touched {
                *self.io_touched = true;
            }
            return Ok(u32::from(value));
        }
        // The VGA status ports (3DA/3BA/3C2) are the ONLY arm in this function that
        // does not unconditionally set io_touched -- the P4a lazy-read case: in the
        // Approximate timing class they must NOT end the batch (io_touched stays
        // false) so a poll loop chains as `run_straight_line` continuations. Static
        // per-port dispatch: these three port numbers always land here, whether or
        // not lazy_port_reads is set, so the branch is a single bool test, never a
        // per-access classification.
        //
        // DECISION (batch-retroactive-rate subtlety): a batch shaped [lazy 3DA polls
        // ... OUT 0x3C2 lowering the dot clock] applies the new dot-clock rate to the
        // WHOLE batch at batch end (the pre-existing retroactive-rate behavior of
        // advance_devices/scale_bus), so the batch-end beam can land behind the last
        // lazy-predicted value this loop observed. Accepted as-is, no compensation:
        // a dot-clock switch is not beam-continuous on real hardware either, and the
        // write itself sets io_touched and ends the batch, so no further lazy read
        // can observe the stale prediction within the same batch.
        if matches!(port, 0x3DA | 0x3BA | 0x3C2) && self.video_io_enabled_for_port(port) {
            if self.lazy_port_reads {
                let beam = self.predicted_beam();
                if let Some(value) = self.video.read_status_port_lazy(port, beam) {
                    return Ok(u32::from(value));
                }
                // Inactive alias (e.g. 3BA polled in a color setup): no side
                // effects, matching `Vga::read_port`'s existing
                // `status1_port_selected` gate, and -- since this arm's static
                // port set is disjoint from every other device's decoded ports
                // (grep-confirmed: nothing else claims 0x3B0..=0x3DF) -- the same
                // 0xFF the non-lazy path's fallthrough to `device_ports`'s passive
                // table would eventually produce. Returned directly, without
                // setting io_touched, so an inactive-alias poll stays lazy too
                // instead of silently falling back to the old behavior.
                return Ok(0xff);
            } else {
                if !skip_io_touched {
                    *self.io_touched = true;
                }
                if let Some(value) = self.video.read_port(port) {
                    return Ok(u32::from(value));
                }
            }
        } else if self.video_io_enabled_for_port(port) {
            if let Some(value) = self.video.read_port(port) {
                if !skip_io_touched {
                    *self.io_touched = true;
                }
                return Ok(u32::from(value));
            }
        }
        // Port 0x61 bits 4/5 (P4a Task 2.3): the second lazy read arm, same static
        // per-port dispatch discipline as 3DA/3BA/3C2 above -- 0x61 always lands
        // here whether or not lazy_port_reads is set. Bits 0/1 (speaker gate/data)
        // are plain register state that cannot change mid-batch: the only writer
        // is `write_io`, which unconditionally sets io_touched and so ends the
        // batch before a later lazy read in the same batch could observe a stale
        // value. Bits 4/5 come from PIT channels 1/2 OUT, which `out_after`'s
        // GATE-stays-level assumption also depends on: GATE2 is wired from this
        // same port's bit 0, and its only writer is that same batch-ending
        // write_io, so GATE cannot move between this read and the batch end
        // either.
        if port == 0x61 {
            if self.lazy_port_reads {
                // Both channels share the SAME elapsed-PIT-clocks conversion
                // (same rate, same batch-entry carry): computed once here rather
                // than twice inside two separate predicted_pit_out calls, since
                // that redundant second predict_dots_core call was pure waste on
                // this hot path (measured: it erased most of the batch-chaining
                // win in the P4a Task 2.3 A/B, see the microbench report).
                let elapsed_pit_clocks = self.elapsed_pit_clocks();
                let ch1 = self.pit.out_after(1, elapsed_pit_clocks);
                let ch2 = self.pit.out_after(2, elapsed_pit_clocks);
                if let (Some(ch1_out), Some(ch2_out)) = (ch1, ch2) {
                    let value = (self.speaker.control_bits() & 0x03)
                        | (u8::from(ch1_out) << 4)
                        | (u8::from(ch2_out) << 5);
                    return Ok(u32::from(value));
                }
                // BCD fallback: at least one of channel 1/2 is BCD-programmed, so
                // `out_after` conservatively declined. Fall through to the exact
                // non-lazy path below (io_touched set, today's live read) rather
                // than a second implementation of the same bit composition.
            }
            if !skip_io_touched {
                *self.io_touched = true;
            }
            // Bit 4 is the DRAM-refresh heartbeat: PIT channel 1 OUT (the AT
            // refresh timer, mode 2), not the speaker's standalone toggle. The PIT
            // seeds channel 1 at power-on so this pulses without guest programming.
            let value = (self.speaker.control_bits() & 0x03)
                | (u8::from(self.pit.channel_out(1)) << 4)
                | (u8::from(self.pit.channel_out(2)) << 5);
            return Ok(u32::from(value));
        }
        // OPL status reads are intentionally exact. AdLib detection is a timer
        // probe, and letting the poll continue inside an approximate CPU batch
        // can starve the emulated timer progression enough to fail on fast CPU
        // modes. Keep every AdLib/SB OPL read batch-ending.
        if let Some(resolved) = opl_port(port) {
            // Always end the batch on an OPL status read, even under the ring-0 PM
            // monitor. The skip_io_touched exemption exists for the monitor's OWN
            // chipset pokes (the vec13 PIC OCW3 probe), but an OPL poll reflected
            // from a V86 guest is real guest device I/O: it must end the batch so the
            // OPL timer advances BETWEEN polls. Without this the whole AdLib
            // detection loop runs inside one batch, the timer only advances at batch
            // end (after every poll already read a stale 0x00), and detection fails.
            *self.io_touched = true;
            // Charge the poll its real ISA bus time in the Approximate class so it
            // cannot outrun the 80 us OPL timer on a fast CPU (folded into the batch
            // device advance in run_until_clock). The Accurate class never accrues
            // this, keeping its byte-identical batch cadence; it also does not need
            // it (its slower clock already spans the window).
            if self.lazy_port_reads {
                *self.isa_io_clocks += isa_io_clocks(self.active_mode);
            }
            // The chip drives only the status byte on reads; data ports read open-bus.
            return Ok(u32::from(self.opl.read_port(resolved).unwrap_or(0xff)));
        }
        // DSP status reads are intentionally exact. SB reset/probe code polls
        // 0x22E for the reset ACK byte, so keeping that loop inside one
        // approximate CPU batch can starve the DSP settle timer.
        // Every arm from here down is unchanged from before Task 1.3: a single
        // unconditional set covers all of them, exactly like the old top-of-function
        // set did, since none of them is a lazy arm (3DA/3BA/3C2, 0x61, OPL status)
        // handled above. This is also where the ring-0-monitor PIC OCW3 probe (port
        // 0x20/0xA0) lands (V86 trap tax, Part 1), so it takes the same
        // skip_io_touched gate as everything else in this function.
        if !skip_io_touched {
            *self.io_touched = true;
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
        if (0x0200..=0x0207).contains(&port) {
            // Game port with no joystick attached: the four one-shot axis timers
            // (bits 0-3) have no pots to charge through so they read expired (0),
            // and the button inputs (bits 4-7) float high (open switches,
            // active-low) -- the same absent-joystick answer INT 15h AH=84h gives.
            // A routine joystick probe must see "no joystick", not an
            // UnsupportedPort fault that halts the machine. The ISA gameport
            // decodes the whole 0x200-0x207 range as aliases of one register
            // (TSUMERA probes 0x200, not 0x201).
            return Ok(0xf0);
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

    fn write_io(
        &mut self,
        port: u16,
        width: BusWidth,
        value: u32,
        cpu_is_ring0_pm: bool,
    ) -> Result<(), BusError> {
        // See read_io's matching comment (V86 trap tax, Part 1): the ring-0
        // monitor's own device pokes (e.g. the vec13 discriminator's PIC OCW3
        // select write) are chipset bookkeeping, not guest-visible activity, so
        // they are exempted from ending the batch in the Approximate class only.
        //
        // A20 carve-out: the batch loop's A20 seam ("any A20 write ... ends this
        // step" -- the before/after compare at batch entry) depends on EVERY
        // write that can move the A20 gate ending the batch, ring-0 or not.
        // Ports 0x92 (system control A), 0x60/0x64 (the 8042 path) can; keep
        // them batch-ending unconditionally. TOKAEMM's a20_apply is PTE-based
        // today (the real gate never drops), so this is belt-and-braces for a
        // future monitor that pokes the real gate, at zero hot-path cost (the
        // monitor's hot pokes are the PIC/EOI ports, not these three).
        let skip_io_touched =
            cpu_is_ring0_pm && self.lazy_port_reads && !matches!(port, 0x60 | 0x64 | 0x92);
        if !skip_io_touched {
            *self.io_touched = true;
        }
        self.trace.record(
            BusAccessKind::IoWrite,
            u32::from(port),
            width,
            self.wait_states.io,
        );

        let pci_decode = self.pci.distira_memory_decode_key();
        if self.pci.write_io(port, width, value) {
            if self.pci.distira_memory_decode_key() != pci_decode {
                self.ram_lookup.rebuild(self.memory.len(), self.pci);
                *self.direct_map_changed = true;
            }
            // initEnable lives in PCI config space (offset 0x40) on real SST-1
            // hardware, not the MMIO window, but Distira's own fbiInit2/dacData
            // DAC-detect handshake needs to see its remap bit. Mirror it into
            // the device on every config-space write so it never drifts from
            // the PciConfig copy of record.
            self.distira.set_init_enable(self.pci.distira_init_enable());
            return Ok(());
        }

        if width != BusWidth::Byte {
            // A wider-than-byte port write decomposes into byte cycles, mirroring
            // `read_io`: the low byte goes to the port and each higher byte to the
            // next port (`io_word_sub_port` keeps the IDE/ATA data registers on the
            // same port). The VGA index/data idiom (a single 16-bit `OUT 0x3C4`/
            // `0x3CE`/`0x3D4`) depends on this; it used to halt the VM with
            // WidthMismatch.
            for i in 0..width.bytes() {
                self.write_io(
                    io_word_sub_port(port, i),
                    BusWidth::Byte,
                    value >> (8 * i),
                    cpu_is_ring0_pm,
                )?;
            }
            return Ok(());
        }

        if self.video_io_disabled_for_port(port) {
            return Ok(());
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
                // A DMA block copy wrote guest RAM directly; if it staged code, drop the caches
                // (see the FDC transfer path). The run loop honors the flag at end of step.
                *self.device_wrote_memory = true;
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
        if (0x0200..=0x0207).contains(&port) {
            // Game port (0x200-0x207 aliases): an OUT fires the four axis
            // one-shots. With no joystick they expire immediately, so there is
            // no state to keep.
            return Ok(());
        }
        if port == 0x00e1 {
            if let Some(mode) = gsw_mode_from_code(value as u8) {
                *self.pending_mode = Some(mode);
            }
            return Ok(());
        }
        if port == 0x00e3 {
            // Toka-DOS service command: 1 = Repair (the only one left after the HLE
            // was retired in SP-3; Format and LoadBootRecord are gone).
            // The run loop performs it after this cycle (it needs &mut self).
            *self.pending_toka_service = Some(value as u8);
            return Ok(());
        }
        if self.unittester.write_port(port, value as u8) {
            return Ok(());
        }
        if port == 0x00e7 {
            // Lotura port 0xE7: bank a code-page font page into the window at
            // CODEPAGE_FONT_WINDOW. sel = cp*3 + size_index where size_index
            // 0=8x16 (4096 bytes), 1=8x14 (3584 bytes), 2=8x8 (2048 bytes).
            // Valid selectors are 0..14 (five code pages, three sizes each).
            // An out-of-range selector is silently ignored.
            let sel = value as usize;
            let cp = sel / 3;
            let size_index = sel % 3;
            if cp < 5 {
                let (size_off, len) = [(0usize, 4096usize), (4096, 3584), (7680, 2048)][size_index];
                let off = cp * 9728 + size_off;
                let page = &izarravm_firmware::CODEPAGE_FONTS[off..off + len];
                for (i, &byte) in page.iter().enumerate() {
                    let _ = self
                        .memory
                        .write_u8(CODEPAGE_FONT_WINDOW as usize + i, byte);
                }
            }
            return Ok(());
        }
        if self.rtc.write_port(port, value as u8) {
            return Ok(());
        }
        if self.serial.write_port(port, value as u8)
            || self.serial2.write_port(port, value as u8)
            || self.lpt.write_port(port, value as u8)
            || self.lpt2.write_port(port, value as u8)
            || (self.video_io_enabled_for_port(port) && self.video.write_port(port, value as u8))
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

    #[inline]
    fn requires_step_break(&self) -> bool {
        // The exact condition the batch loop checks after each instruction: a port access touched
        // time-dependent device state, or an HLE software interrupt is pending. The straight-line
        // run executor ends its run on this so the machine services it at the old per-instruction
        // boundary.
        *self.io_touched || self.pending_soft_int.is_some()
    }

    fn interrupt_acknowledge(&mut self, vector: u8, _ax: u16) -> Result<(), BusError> {
        self.trace.record(
            BusAccessKind::InterruptAcknowledge,
            u32::from(vector),
            BusWidth::Byte,
            self.wait_states.io,
        );
        // THE LANDING ADDRESS IS THE ONLY POSTER. A host-serviced BIOS INT is
        // recognized where the dispatch LANDS (`note_stub_fetch` on a
        // per-vector ROM stub), never at the `INT n` opcode: posting here as
        // well double-serviced two standard dispatch shapes (a guest hook
        // chaining to the saved default, and a copied vector landing on
        // another vector's stub). Real-hardware semantics follow from
        // landing-only posting: a hook that fully handles without chaining
        // gets NO HLE service (the hook replaced the ROM), a hook that chains
        // gets exactly one service at the landing, and a copied vector
        // services as the LANDED vector, once.
        //
        // This arm still posts the two shapes whose landing the fetch seam
        // cannot see:
        // (a) raw-program INT 20h/21h: their IVT entries target the low-RAM
        //     IRET at 0x600, not the per-vector table (0x27 IS table-seeded
        //     and rides the fetch seam like everything else).
        if self.program_runtime && matches!(vector, 0x20 | 0x21) {
            *self.pending_soft_int = Some(vector);
            return Ok(());
        }
        // (b) the legacy shared chain target FF00:0000, which period booters
        //     hardcode (IVT[0x13] -> FF00:0000, or a hook chaining there).
        //     That single address serves every vector, so the fetch seam
        //     cannot attribute a landing by address alone: stash the vector
        //     here and let the FF00:0000 fetch post it (consumed there; a
        //     per-vector stub landing also disarms it). Known corner, accepted
        //     for this legacy-only path: a nested intercepted INT inside a
        //     hook body overwrites the stash before the hook chains to
        //     FF00:0000, dropping the outer service; and a stash left armed by
        //     a non-chaining hook posts once if the guest later jumps to
        //     FF00:0000 outside any INT context.
        if self.soft_int_intercepted(vector)? {
            *self.last_int_vector = Some(vector);
        }
        Ok(())
    }
}

impl MachineBus<'_> {
    /// The one interception predicate for host-serviced software interrupts,
    /// shared by the two dispatch seams: `note_stub_fetch` (execution reaching
    /// the vector's ROM stub by any route - an `INT n` opcode's IVT dispatch,
    /// a DPMI host's simulate-real-mode-interrupt far dispatch, or a guest
    /// chaining to a saved default vector) and `interrupt_acknowledge` (the
    /// legacy-chain stash and the raw-program low-RAM vectors).
    ///
    /// The DOS multiplex vector (INT 2Fh) HLE -- including the AX=1686h/1687h
    /// DPMI-install check -- only stands in for a real handler when none
    /// exists: once a guest hooks IVT[0x2F] (JEMMEX, DOS/32A's stub) the hook
    /// owns it, same for the absent-resident-API vectors. In booter-inert mode
    /// 2Fh also stands down so a self-booting disk owns it through the IVT.
    /// The pure DOS vectors 0x20-0x2E are not intercepted at all outside the
    /// raw-program runtime (the Rust DOS kernel was retired in SP-3), and INT
    /// 67h is never intercepted (the TOKAEMM guest driver owns the EMS API).
    fn soft_int_intercepted(&mut self, vector: u8) -> Result<bool, BusError> {
        let dos_multiplex = vector == 0x2F && self.vector_points_at_rom_iret(vector)?;
        let absent_resident_api = matches!(vector, 0x5C | 0x60 | 0x68 | 0x6F | 0x7A | 0x86 | 0xE4)
            && self.vector_points_at_rom_iret(vector)?;
        // A `new_raw_program` machine keeps INT 20h/21h/27h intercepted so the run
        // loop's guarded raw-program arm (`handle_raw_program_int`) services them.
        // Outside that runtime nothing intercepts the pure-DOS vectors any more.
        let raw_program_vector = self.program_runtime && matches!(vector, 0x20 | 0x21 | 0x27);
        let intercepted = matches!(
            vector,
            0x10 | 0x11 | 0x12 | 0x13 | 0x14 | 0x15 | 0x17 | 0x18 | 0x19 | 0x1A | 0x40 | 0x42
        ) || raw_program_vector
            || absent_resident_api
            || dos_multiplex;
        Ok(intercepted && !(self.booter_inert && vector == 0x2F))
    }

    /// The fetch-seam half of software-interrupt interception: execution has
    /// reached a per-vector ROM stub entry (see BIOS_INT_STUB_TABLE_ROM_OFFSET)
    /// or the legacy shared chain target FF00:0000. Posts the vector for the
    /// run loop's deferred HLE dispatch. Both landing shapes lead with a NOP,
    /// which guarantees a post-instruction break fires before the IRET, so the
    /// service still sees the INT frame on the stack. This is the ONLY poster
    /// for every dispatch route - INT opcode, a DPMI host's simulated
    /// real-mode interrupt, a guest chaining to a saved default - so each
    /// dispatch services exactly once. Repeated fetch charges of the same
    /// visit are absorbed by the pending_soft_int check (the pending vector is
    /// only cleared at the next batch entry, after the service ran and
    /// execution moved to the IRET byte, whose odd offset never posts).
    ///
    /// `address` is a LINEAR address on both seams (`note_code_fetch_linear`
    /// per cold-fetched byte, `charge_instruction_fetch_run` per cached run):
    /// the stub table's identity is architectural, and a paging guest that
    /// shadows the BIOS F-page (JemmEx) still dispatches through linear
    /// FF00:02xx while backing it with another physical page. Residual
    /// divergence, recorded per review: a pmode guest running unrelated code
    /// AT linear 0xFF0xx/0xFF2xx (mapped wherever) posts a bogus service;
    /// deliberate-hostile only, no real DOS stack does this.
    fn note_stub_fetch(&mut self, address: u32) {
        if address == BIOS_LEGACY_IRET_LINEAR {
            // The legacy shared nop;iret at FF00:0000: one address for every
            // vector, so attribution comes from the stash the `INT n` opcode
            // arm left behind. Consumed here; a landing with no armed stash
            // (a simulated jump with no preceding INT) stays a no-op.
            let stashed = self.last_int_vector.take();
            if self.pending_soft_int.is_none()
                && let Some(vector) = stashed
                && self.soft_int_intercepted(vector).unwrap_or(false)
            {
                *self.pending_soft_int = Some(vector);
            }
            return;
        }
        let offset = address.wrapping_sub(BIOS_INT_STUB_TABLE_LINEAR);
        if offset >= BIOS_INT_STUB_TABLE_LEN || offset & 1 != 0 {
            return; // outside the table, or the IRET byte (mid-stub resume)
        }
        let vector = (offset / 2) as u8;
        let intercepted = self.soft_int_intercepted(vector).unwrap_or(false);
        // An INTERCEPTED landing supersedes any armed legacy stash: the
        // dispatch the stash described has been attributed here by address
        // instead. A NON-intercepted landing must leave the stash alone - its
        // ack never armed it, and the machine's own timer ISR chains INT 1Ch
        // (stub 0x1C) every tick, so an unconditional disarm would race a
        // hardware IRQ against a live hook-chain attribution and drop the
        // chained service (round-2 review finding 1).
        if intercepted {
            *self.last_int_vector = None;
        }
        if let Some(pending) = *self.pending_soft_int {
            // The dedup above is vector-blind; the only legitimate repeat is a
            // re-charge of the SAME pending visit (round-1 review finding 3).
            debug_assert!(
                pending == vector,
                "stub fetch posted vector {vector:#04x} while {pending:#04x} is still pending"
            );
            return;
        }
        if intercepted {
            *self.pending_soft_int = Some(vector);
        }
    }

    fn vector_points_at_rom_iret(&mut self, vector: u8) -> Result<bool, BusError> {
        let address = usize::from(vector) * 4;
        let off = self.memory.read_u16(address)?;
        let seg = self.memory.read_u16(address + 2)?;
        // A vector is "still the BIOS default" when it points at its per-vector
        // ROM stub. The legacy shared IRET at FF00:0000 is also accepted:
        // period booters hardcode that address (IVT[0x13] -> FF00:0000 to chain
        // disk calls), and pre-table guests may restore a saved default.
        Ok(seg == BIOS_ROM_IRET_SEG && (off == bios_int_stub_off(vector) || off == 0))
    }

    /// Peek the VGA beam's dot position "now" -- mid-batch, as of whatever this
    /// batch's accumulated core clocks plus the bus clocks recorded into `trace`
    /// since batch entry add up to -- WITHOUT mutating any device state (`video`,
    /// `vga_dots`, `bus_rem` on the owning `Machine` are all untouched; `&self`
    /// makes that compiler-enforced). This is the P4a Slice 1 lazy port-read peek
    /// (dev_docs/2026-07-02-p4a-lazy-port-device-time-plan.md Task 1.2); wiring
    /// it into `read_io` is Task 1.3.
    ///
    /// Units combined, matching exactly what the real batch-end step in
    /// `run_until_clock`/`advance_devices` will later consume:
    /// - the core portion, BATCH-scoped: `prior_runs_core_clocks` (the
    ///   interrupt-service charge plus every completed straight-line run of this
    ///   batch, republished by the batch loop before each run) plus
    ///   `core_clocks_so_far` (the current run's prior instructions, excluding
    ///   the in-flight instruction's own charge). One batch chains many runs and
    ///   only the batch total feeds the batch-end step, so the run-scoped term
    ///   alone would drop earlier runs' core clocks and jump backward at every
    ///   run boundary; the monotonicity claim below rests on this batch-scoping,
    ///   not on a port read ending the run.
    /// - the bus portion: `trace.elapsed_clocks() - trace_elapsed_at_batch_start`
    ///   raw bus clocks recorded so far this batch, scaled by the SAME (num, den)
    ///   `bus_timing` ratio and the SAME fractional carry (`bus_rem_at_batch_start`)
    ///   the real end-of-batch `scale_bus` call will start from -- no `scale_bus`
    ///   call happens between batch entry and batch end, so the batch-entry carry
    ///   IS the carry the real call uses. This mirrors `scale_bus`'s arithmetic
    ///   shape exactly but reads `bus_rem_at_batch_start` instead of the live
    ///   `bus_rem` and does not write the carry back anywhere (no mutation).
    ///
    /// The in-flight instruction's own fetch/data bus clocks may already be
    /// partially recorded into `trace` by the time this runs; that is fine and
    /// intentional -- the real batch-end total (computed once the whole
    /// instruction has retired) is always a superset of what is recorded here, so
    /// the clock total this predicts from is monotone within the batch and never
    /// exceeds the batch's eventual final total. It never overshoots what the
    /// real advance would show for the same clock total, because it uses the same
    /// formula.
    ///
    /// Predicts POSITION ONLY: the dots-per-frame modulo wrap, never the
    /// frame-boundary side effects (`finalize_frame`, the frame counter) that
    /// `Vga::advance` performs -- those stay exclusively in the real
    /// `advance_devices` at batch end. Shares the exact `predict_dots_core`
    /// arithmetic `Machine::predict_dots` uses (same operation order, same
    /// floor/subtract sequence), so a mid-batch peek can never structurally
    /// diverge from what the later real advance will show for the same clocks.
    fn predicted_beam(&self) -> u64 {
        let in_batch_clocks = self.in_batch_clocks();
        let (whole_dots, _remainder) = predict_dots_core(
            in_batch_clocks,
            self.vga_dots_at_batch_start,
            self.video.dot_clock_hz(),
            self.inv_clock_at_batch_start,
        );
        let frame = self.video.frame_dots();
        if frame == 0 {
            return self.beam_at_batch_start; // guard: un-programmed CRTC, mirrors Vga::advance
        }
        (self.beam_at_batch_start + whole_dots) % frame
    }

    /// The batch-scoped CPU clock total elapsed as of "now" (mid-batch), the
    /// shared T both `predicted_beam` and `predicted_pit_clocks` build on: batch-
    /// scoped core clocks (`prior_runs_core_clocks + core_clocks_so_far`) plus
    /// in-batch bus clocks recorded into `trace` since batch entry, scaled by the
    /// SAME (num, den) `bus_timing` ratio and fractional carry
    /// (`bus_rem_at_batch_start`) the real end-of-batch `scale_bus` call will
    /// start from. Extracted from `predicted_beam` (P4a Task 2.3) so the PIT lazy
    /// read consumes byte-for-byte the same clock total the beam peek does,
    /// rather than a second hand-rolled copy of this arithmetic.
    fn in_batch_clocks(&self) -> u64 {
        let in_batch_bus_clocks = self.trace.elapsed_clocks() - self.trace_elapsed_at_batch_start;
        let scaled = in_batch_bus_clocks * u64::from(self.bus_num_at_batch_start)
            + self.bus_rem_at_batch_start;
        let scaled_bus_clocks = scaled / u64::from(self.bus_den_at_batch_start);
        self.prior_runs_core_clocks + self.core_clocks_so_far + scaled_bus_clocks
    }

    /// Elapsed PIT input CLKs "now" -- mid-batch, WITHOUT mutating `pit_clocks`
    /// (P4a Task 2.3: the lazy port 0x61 bits 4/5 read). Shared by every channel
    /// a caller peeks in the same read (0x61 needs both channel 1 and channel
    /// 2), so a caller that needs more than one channel should compute this ONCE
    /// and pass it to `Pit::out_after` per channel, not call `predicted_pit_out`
    /// (below) once per channel -- the two calls would otherwise redo this exact
    /// conversion redundantly (measured: that redundancy erased most of the
    /// batch-chaining win in the P4a Task 2.3 A/B, see the microbench report).
    ///
    /// Converts the shared in-batch clock total `T` (`in_batch_clocks`, the same
    /// T `predicted_beam` peeks with) into elapsed PIT input clocks by calling
    /// the SAME `advance_fractional` function the real `advance_devices` PIT
    /// step calls, with `pit_clocks_at_batch_start` standing in for the live
    /// accumulator and `pit_per_clock_at_batch_start` for the live rate. NOT
    /// `predict_dots_core` with PIT_INPUT_HZ standing in for the dot clock:
    /// that formula's `clocks * rate_hz * inv_clock` factoring floor-diverges
    /// from the real advance's pre-divided `clocks * pit_per_clock` product at
    /// the IEEE-f64 level (see `advance_fractional`'s doc comment), which would
    /// let a lazy read report an OUT level one PIT clock ahead of or behind
    /// what batch end establishes. `advance_devices` only runs at batch end /
    /// wake step, never mid-batch, so `pit_clocks_at_batch_start` IS the live
    /// `pit_clocks` value the real call will start folding T's clocks into: no
    /// time travel, this predicts exactly what a real `advance_devices` at T
    /// followed by a read would produce.
    fn elapsed_pit_clocks(&self) -> u64 {
        let (elapsed_pit_clocks, _remainder) = advance_fractional(
            self.pit_clocks_at_batch_start,
            self.in_batch_clocks(),
            self.pit_per_clock_at_batch_start,
        );
        elapsed_pit_clocks
    }

    /// Peek `channel`'s live PIT OUT level "now" -- mid-batch, WITHOUT stepping
    /// `pit` or mutating `pit_clocks`. `None` when the channel's counter is BCD
    /// (see `Counter::out_after` via `Pit::out_after`); the caller falls back to
    /// a real read in that case. Convenience wrapper over `elapsed_pit_clocks`
    /// for a single-channel peek (tests, and any future single-channel lazy
    /// port); the production 0x61 read arm needs both channels and calls
    /// `elapsed_pit_clocks` directly instead, per the note above.
    #[cfg(test)]
    fn predicted_pit_out(&self, channel: usize) -> Option<bool> {
        self.pit.out_after(channel, self.elapsed_pit_clocks())
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
        0x0280..=0x028f, // C/MS Game Blaster alternate-base probe range (Prince of
        // Persia's sound detect reads 0x283 and must see open bus,
        // not a fault -- the port-0x201 joystick-stub precedent)
        0x0388..=0x038b, // OPL2/OPL3 (intercepted by the chip, kept as a fallback)
        0x03b0..=0x03df, // MDA/CGA/EGA/VGA registers
        0x5658..=0x565b, // VMware backdoor probe (DX=0x5658, EAX='VMXh'): real,
                         // non-VMware hardware has nothing at this port, so a guest's `IN
                         // EAX, DX` detection probe must read open bus (all-ones), never the
                         // VMware magic response and never an UnsupportedPort fault. A dword
                         // IN decomposes into four byte reads at 0x5658-0x565b (the same
                         // io_word_sub_port widening as every other wide port access), so all
                         // four bytes are covered here. JEMMEX runs this probe during its own
                         // hypervisor-presence check and used to halt the machine with
                         // CpuError("unsupported I/O port 0x5658") before this stub existed.
    ];
    ranges.into_iter().flatten()
}

fn icdex_iso_child_record(image: &CdImage, dir_record: &[u8], component: &str) -> Option<Vec<u8>> {
    let lba = u32::from_le_bytes(dir_record.get(2..6)?.try_into().ok()?);
    let len = u32::from_le_bytes(dir_record.get(10..14)?.try_into().ok()?);
    let sectors = len.div_ceil(cdimage::DATA_SECTOR as u32);
    let wanted = component.to_ascii_uppercase();
    for sector_index in 0..sectors {
        let sector = image.read_data_sector(lba + sector_index)?;
        let mut offset = 0usize;
        while offset < sector.len() {
            let record_len = usize::from(sector[offset]);
            if record_len == 0 {
                break;
            }
            let end = offset.checked_add(record_len)?;
            if end > sector.len() {
                break;
            }
            let record = &sector[offset..end];
            if icdex_iso_name_matches(record, &wanted) {
                return Some(record.to_vec());
            }
            offset = end;
        }
    }
    None
}

fn icdex_iso_name_matches(record: &[u8], wanted: &str) -> bool {
    let name_len = usize::from(*record.get(32).unwrap_or(&0));
    let Some(name) = record.get(33..33 + name_len) else {
        return false;
    };
    if name == [0] || name == [1] {
        return false;
    }
    let raw = String::from_utf8_lossy(name).to_ascii_uppercase();
    raw == wanted || raw.split_once(';').is_some_and(|(base, _)| base == wanted)
}

fn icdex_iso_name_and_version(record: &[u8]) -> (Vec<u8>, u16) {
    let name_len = usize::from(*record.get(32).unwrap_or(&0));
    let name = record.get(33..33 + name_len).unwrap_or(&[]);
    if name == [0] {
        return (b".".to_vec(), 1);
    }
    if name == [1] {
        return (b"..".to_vec(), 1);
    }
    let raw = String::from_utf8_lossy(name).to_ascii_uppercase();
    if let Some((base, version)) = raw.split_once(';') {
        let version = version.parse::<u16>().unwrap_or(1);
        (base.as_bytes().to_vec(), version)
    } else {
        (raw.into_bytes(), 1)
    }
}

fn gsw_mode_from_code(code: u8) -> Option<GswMode> {
    match code {
        0 => Some(GswMode::Gsw386),
        1 => Some(GswMode::Gsw486),
        2 => Some(GswMode::Gsw586),
        3 => Some(GswMode::Gsw386Slow),
        _ => None,
    }
}

fn gsw_mode_code(mode: GswMode) -> u8 {
    match mode {
        GswMode::Gsw386 => 0,
        GswMode::Gsw386Slow => 3,
        GswMode::Gsw486 => 1,
        GswMode::Gsw586 => 2,
    }
}

/// Map a GSW compatibility mode to the CPU instruction-set level it presents to the
/// guest. The 586 native default keeps the full ISA; a lower mode lowers the level
/// so the core raises #UD for instructions that part lacked.
fn cpu_level_for_mode(mode: GswMode) -> CpuLevel {
    match mode {
        GswMode::Gsw386 | GswMode::Gsw386Slow => CpuLevel::I386,
        GswMode::Gsw486 => CpuLevel::I486,
        GswMode::Gsw586 => CpuLevel::I586,
    }
}

/// Whole VGA dot-clocks elapsed for `clocks` CPU clocks, given the live
/// fractional-dot accumulator `dots_owed`, the VGA's live dot-clock rate, and the
/// active mode's `1 / clock_hz` factor. Pure free function: the one shared
/// arithmetic core `Machine::predict_dots` (the real `advance_devices` step) and
/// `MachineBus::predicted_beam` (the Slice 1 lazy port-read peek) both call, so a
/// mid-batch prediction and the later real advance can never structurally diverge
/// in rounding. Kept textually identical to the arithmetic it was extracted from
/// (same operation order, same floor/subtract sequence) -- do not "simplify" this
/// without re-checking both callers' bit-for-bit tests.
fn predict_dots_core(clocks: u64, dots_owed: f64, dot_clock_hz: u64, inv_clock: f64) -> (u64, f64) {
    let raw = dots_owed + clocks as f64 * dot_clock_hz as f64 * inv_clock;
    let whole = raw.floor();
    (whole as u64, raw - whole)
}

/// Whole device clocks elapsed for `clocks` CPU clocks at a PRE-COMBINED
/// per-CPU-clock rate, given the live fractional carry. Pure free function: the
/// one shared arithmetic core every real `advance_devices` fractional block
/// (PIT, OPL, DSP) and the mid-batch lazy PIT peek (`MachineBus::
/// elapsed_pit_clocks`, the P4a Task 2.3 lazy port 0x61 read), so a mid-batch
/// prediction and the later real advance can never diverge in rounding.
/// NOT interchangeable with `predict_dots_core` above
/// even where the rates are mathematically equal: that formula multiplies
/// `clocks * rate_hz as f64 * inv_clock` (two roundings, left-associated),
/// while the PIT path has always multiplied by the pre-divided
/// `pit_per_clock = PIT_INPUT_HZ / clock_hz` factor (one rounding) -- the two
/// factorings floor-diverge at the IEEE-f64 level for reachable (carry, clocks)
/// pairs, which is exactly the seam this extraction closes. Kept textually
/// identical to the `advance_devices` arithmetic it was extracted from (carry
/// plus product, floor, subtract) -- do not "simplify" this without re-checking
/// all callers' bit-for-bit tests.
fn advance_fractional(carry: f64, clocks: u64, rate_per_clock: f64) -> (u64, f64) {
    let raw = carry + clocks as f64 * rate_per_clock;
    let whole = raw.floor();
    (whole as u64, raw - whole)
}

/// CPU clocks for one 8-bit ISA I/O bus cycle at the live mode's clock. The ISA
/// bus runs at a fixed ~8 MHz, so an access costs roughly a microsecond of wall
/// time no matter how fast the CPU is; charging that keeps a fast-CPU device poll
/// from outrunning the hardware it polls (chiefly the 80 us OPL timer that AdLib
/// detection waits on). At least one clock so it always makes progress.
fn isa_io_clocks(mode: GswMode) -> u64 {
    (mode.clock_hz() / 1_000_000).max(1)
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

fn boot_sector_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
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

/// RAM address of a one-byte HLT sentinel in the free gap between the IRET stub at
/// 0x600 and the RTC ISR stub at 0x610. `install_dos_low_memory_stubs` seeds it as
/// a safe HLT landing spot in the reserved low-memory stub cluster.
const SYSINIT_HALT_STUB: usize = BIOS_IRET_STUB_ADDRESS + 1;

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
/// RAM address of the default IRQ12/IRQ13/IRQ14 slave-PIC handler. It sends EOIs
/// to both PICs and returns, which is enough for an unclaimed slave interrupt.
const BIOS_SLAVE_IRQ_ISR_ADDRESS: usize = 0x0622;

/// RAM address of the INT 24h host-trampoline completion stub. A live guest INT
/// 24h handler IRETs here, executes HLT to stop the instruction batch, then the
/// machine decodes handler AL and resumes the original host-serviced INT return.
const BIOS_CRITICAL_ERROR_RETURN_STUB_ADDRESS: usize = 0x0630;
/// Default DOS Ctrl-C handler: terminate via INT 21h AH=4Ch with code 0.
const DOS_INT23_DEFAULT_STUB_ADDRESS: usize = 0x0632;
/// Default DOS critical-error handler: return Fail (AL=03h) to the caller.
const DOS_INT24_DEFAULT_STUB_ADDRESS: usize = 0x0637;

/// EBDA offset of the far pointer to the user pointing-device (mouse) handler the
/// guest installs with INT 15h AX=C207h (offset word then segment word). Offset
/// 0x22 overlapped the fixed-disk parameter table (0x20..0x2F): a mounted HDD
/// clobbered the handler pointer and a registered handler corrupted the disk
/// geometry. The mouse sub-block lives in the free 0x01..0x0F gap: handler
/// far-pointer at 0x02/0x04, packet buffer at 0x06..0x09 (4 bytes, the IntelliMouse
/// wheel byte included), byte-index at 0x0A, packet size at 0x0B. The izbios INT 74h
/// ISR mirrors these offsets (izbios-defs.inc EBDA_MOUSE_*).
const EBDA_MOUSE_HANDLER_OFF: u32 = 0x0002;
/// EBDA offset of the mouse packet-size byte (izbios-defs.inc EBDA_MOUSE_PKT_SIZE):
/// 3 for a standard mouse, 4 once the platform enables IntelliMouse wheel mode. The
/// BIOS INT 74h ISR accumulates this many aux bytes before dispatching a frame.
const EBDA_MOUSE_PKT_SIZE_OFF: u32 = 0x000B;

/// Physical address of the CP/M CALL 5 entry. DOSINTS names this as INT 30h, but
/// it is code over the IVT bytes for INT 30h and INT 31h rather than a real
/// interrupt vector.
const DOS_CALL5_ENTRY_ADDRESS: usize = 0x00c0;
const DOS_CALL5_ENTRY_SEG: u16 = 0xff00;
const DOS_CALL5_ENTRY_OFF: u16 = 0x0020;
const DOS_CALL5_ROM_OFFSET: usize = 0xf020;
const DOS_CALL5_MAX_FUNCTION: u8 = 0x24;

/// ROM adapter for the PSP:0005 CP/M entry. CALL 5 pushes a near return address,
/// the PSP far-call pushes PSP:000A, and this adapter rewrites the stack so RETF
/// lands back at the original caller. CL selects the old DOS function.
const DOS_CALL5_ENTRY_STUB: [u8; 49] = [
    0x80,
    0xf9,
    DOS_CALL5_MAX_FUNCTION, // cmp cl,24h
    0x77,
    0x17, // ja bad
    0x55, // push bp
    0x8b,
    0xec, // mov bp,sp
    0x50, // push ax
    0x8b,
    0x46,
    0x04, // mov ax,[bp+4]
    0x87,
    0x46,
    0x06, // xchg ax,[bp+6]
    0x89,
    0x46,
    0x04, // mov [bp+4],ax
    0x58, // pop ax
    0x5d, // pop bp
    0x83,
    0xc4,
    0x02, // add sp,2
    0x88,
    0xcc, // mov ah,cl
    0xcd,
    0x21, // int 21h
    0xcb, // retf
    0x55, // bad: push bp
    0x8b,
    0xec, // mov bp,sp
    0x50, // push ax
    0x8b,
    0x46,
    0x04, // mov ax,[bp+4]
    0x87,
    0x46,
    0x06, // xchg ax,[bp+6]
    0x89,
    0x46,
    0x04, // mov [bp+4],ax
    0x58, // pop ax
    0x5d, // pop bp
    0x83,
    0xc4,
    0x02, // add sp,2
    0xb0,
    0x00, // mov al,0
    0xcb, // retf
];

/// Per-vector BIOS software-interrupt stub table: 256 two-byte `nop; iret`
/// entries in ROM at FF00:0200 (physical 0xFF200), one per vector, seeded into
/// the IVT as `FF00:(0x200 + 2*vector)`. The per-vector ENTRY ADDRESS is what
/// lets the host recognize which BIOS service is being invoked from the
/// instruction-FETCH seam, independent of HOW execution arrived: an `INT n`
/// opcode (also caught by `interrupt_acknowledge`), a DPMI host's
/// simulate-real-mode-interrupt dispatch that far-jumps through the IVT
/// without any INT opcode (CWSDPMI servicing DJGPP `int86`, JEMM's
/// Simulate_Int), or a guest chaining to a saved vector. The legacy shared
/// IRET at FF00:0000 made all of those silent no-ops: Quake under CWSDPMI
/// issued its INT 10h mode set and console teletype through the simulate path
/// and the screen never left text mode.
///
/// The stub body is `nop; iret`, not a bare `iret`: the fetch-seam trigger
/// posts `pending_soft_int`, which the run loop services at the next
/// post-instruction break - after the NOP, BEFORE the IRET - so the
/// real-mode INT frame is still on the stack when the HLE service runs
/// (`set_int_frame_carry` patches CF in the saved FLAGS image). A bare IRET
/// would pop the frame before the break.
///
/// Placed at 0x200 to stay clear of the machine-patched ROM residents below
/// it (the shared legacy IRET at 0x0000, the CALL-5 adapter, the timer and
/// master-IRQ ISR stubs at 0x0060/0x0080).
const BIOS_INT_STUB_TABLE_ROM_OFFSET: usize = 0xF200;
const BIOS_INT_STUB_TABLE_LINEAR: u32 = 0xFF200;
const BIOS_INT_STUB_TABLE_LEN: u32 = 512;
/// Linear address of the legacy shared chain target FF00:0000. Period
/// booters hardcode it (IVT[0x13] -> FF00:0000, or a hook chaining there), so
/// the machine writes the same `nop; iret` shape there that the per-vector
/// stubs use: the NOP fetch posts the vector stashed by the `INT n` opcode arm
/// (`last_int_vector`) and the post-instruction break services the HLE before
/// the IRET pops the frame.
///
/// Stub recognition is keyed on the LINEAR fetch address, never the physical
/// one: an EMM386-class paging monitor (JemmEx) shadows the BIOS F-page, so
/// the guest dispatches through linear FF00:02xx while the bytes are fetched
/// from a copy in extended RAM. The linear address is the architectural
/// identity of the stub; the physical backing is the guest's business.
const BIOS_LEGACY_IRET_LINEAR: u32 = 0xFF000;
/// One compare covers FF000 (legacy) through the stub table's end (FF3FF).
const BIOS_STUB_WINDOW_LEN: u32 = 0x400;
const BIOS_LEGACY_IRET_ROM_OFFSET: usize = 0xF000;

const fn bios_int_stub_off(vector: u8) -> u16 {
    0x0200 + (vector as u16) * 2
}

// The fetch seam treats 0xFF000..0xFF400 as service-posting addresses, so no
// machine-written ROM resident may grow into the window or span its start
// (an immediate byte at an even table offset would post a bogus service).
const _: () = {
    assert!(DOS_CALL5_ROM_OFFSET + DOS_CALL5_ENTRY_STUB.len() <= BIOS_TIMER_ISR_ROM_OFFSET);
    assert!(
        BIOS_TIMER_ISR_ROM_OFFSET + BIOS_TIMER_ISR_STUB.len() <= BIOS_MASTER_IRQ_ISR_ROM_OFFSET
    );
    assert!(
        BIOS_MASTER_IRQ_ISR_ROM_OFFSET + BIOS_MASTER_IRQ_ISR_STUB.len()
            <= BIOS_INT_STUB_TABLE_ROM_OFFSET
    );
    // The legacy nop;iret pair itself must sit below every other resident's
    // start and inside the recognition window.
    assert!(BIOS_LEGACY_IRET_ROM_OFFSET + 2 <= DOS_CALL5_ROM_OFFSET);
};

fn write_bios_int_stub_table(rom: &mut [u8]) {
    for vector in 0..=255usize {
        rom[BIOS_INT_STUB_TABLE_ROM_OFFSET + vector * 2] = 0x90; // nop
        rom[BIOS_INT_STUB_TABLE_ROM_OFFSET + vector * 2 + 1] = 0xCF; // iret
    }
    // The legacy shared chain target gets the same nop; iret shape (see
    // BIOS_LEGACY_IRET_LINEAR). Machine-written for every ROM: the Izarra BIOS
    // reserves a bare IRET here followed by zero padding, and the synthetic
    // HLE ROMs relied on the constructors writing the IRET byte.
    rom[BIOS_LEGACY_IRET_ROM_OFFSET] = 0x90; // nop
    rom[BIOS_LEGACY_IRET_ROM_OFFSET + 1] = 0xCF; // iret
}

const BIOS_TIMER_ISR_ROM_OFF: u16 = 0x0060;
const BIOS_TIMER_ISR_ROM_OFFSET: usize = 0xf060;
const BIOS_MASTER_IRQ_ISR_ROM_OFF: u16 = 0x0080;
const BIOS_MASTER_IRQ_ISR_ROM_OFFSET: usize = 0xf080;

// INT 08h: bump the BIOS tick dword, chain INT 1Ch, EOI the master PIC, IRET.
const BIOS_TIMER_ISR_STUB: [u8; 25] = [
    0x50, // push ax
    0x1e, // push ds
    0x31, 0xc0, // xor ax,ax
    0x8e, 0xd8, // mov ds,ax
    0x83, 0x06, 0x6c, 0x04, 0x01, // add word [046Ch],1
    0x83, 0x16, 0x6e, 0x04, 0x00, // adc word [046Eh],0
    0xcd, 0x1c, // int 1Ch
    0xb0, 0x20, // mov al,20h
    0xe6, 0x20, // out 20h,al
    0x1f, // pop ds
    0x58, // pop ax
    0xcf, // iret
];

const BIOS_MASTER_IRQ_ISR_STUB: [u8; 7] = [
    0x50, // push ax
    0xb0, 0x20, // mov al,20h
    0xe6, 0x20, // out 20h,al
    0x58, // pop ax
    0xcf, // iret
];

/// Real-mode segment of the 1 KB extended BIOS data area (EBDA), reserved at the
/// top of conventional memory. Segment 0x9FC0 is physical 0x9FC00, so the EBDA
/// runs 0x9FC00-0x9FFFF and the conventional-memory word at 0040:0013 drops from
/// 640 to 639 KB. INT 15h AH=C1h returns this segment in ES.
const EBDA_SEGMENT: u16 = 0x9FC0;

/// Physical base of the INT 15h AH=C0h system-configuration table. It lives inside
/// the reserved EBDA (after the size byte at offset 0), so it is consistent with
/// the lowered conventional-memory size and out of the BDA's way.
const BIOS_CONFIG_TABLE_ADDR: u32 = 0x9FC10;
/// AT fixed-disk parameter table for drive 80h, published through IVT[41h].
const BIOS_FIXED_DISK_PARAMETER_TABLE_ADDR: u32 = 0x9FC20;
/// Default AT diskette parameter table, published through IVT[1Eh].
const BIOS_DISKETTE_PARAMETER_TABLE_ADDR: u32 = 0x9FC30;
/// POST error-log backing storage for INT 15h AH=21h. One count byte lives just
/// before the returned record array.
const BIOS_POST_ERROR_LOG_COUNT_ADDR: u32 = 0x9FC3F;
const BIOS_POST_ERROR_LOG_ADDR: u32 = 0x9FC40;
const BIOS_POST_ERROR_LOG_MAX: u8 = 16;

fn install_bios_font_mirror(rom: &mut [u8]) {
    let off = usize::from(BIOS_FONT_8X8_ROM_OFFSET);
    rom[off..off + font::VGAFONT_8X8.len()].copy_from_slice(&font::VGAFONT_8X8);
    let off = usize::from(BIOS_FONT_8X14_ROM_OFFSET);
    rom[off..off + font::VGAFONT_8X14.len()].copy_from_slice(&font::VGAFONT_8X14);
    let off = usize::from(BIOS_FONT_8X16_ROM_OFFSET);
    rom[off..off + font::VGAFONT_8X16.len()].copy_from_slice(&font::VGAFONT_8X16);
    let high = &font::VGAFONT_8X8[128 * 8..];
    let off = usize::from(BIOS_FONT_8X8_HIGH_ROM_OFFSET);
    rom[off..off + high.len()].copy_from_slice(high);
}

fn seed_bda_video_save_pointer(memory: &mut Memory) -> Result<(), BusError> {
    memory.write_u16(
        BDA_VIDEO_SAVE_POINTER,
        INT10_VIDEO_SAVE_POINTER_TABLE_OFFSET,
    )?;
    memory.write_u16(BDA_VIDEO_SAVE_POINTER + 2, VGA_BIOS_SEGMENT)
}

fn seed_video_bios_tables(memory: &mut Memory) -> Result<(), BusError> {
    let vga_base = VGA_BIOS_BASE as usize;
    for (index, byte) in INT10_STATIC_FUNCTIONALITY.iter().copied().enumerate() {
        memory.write_u8(vga_base + index, byte)?;
    }

    let save_ptr = vga_base + usize::from(INT10_VIDEO_SAVE_POINTER_TABLE_OFFSET);
    memory.write_u16(save_ptr, INT10_VIDEO_PARAM_TABLE_OFFSET)?;
    memory.write_u16(save_ptr + 2, VGA_BIOS_SEGMENT)?;
    for slot in 1..INT10_VIDEO_SAVE_POINTER_TABLE_PTRS {
        memory.write_u16(save_ptr + slot * 4, 0)?;
        memory.write_u16(save_ptr + slot * 4 + 2, 0)?;
    }

    let param_table = vga_base + usize::from(INT10_VIDEO_PARAM_TABLE_OFFSET);
    for offset in 0..INT10_VIDEO_PARAM_TABLE_ENTRIES * INT10_VIDEO_PARAM_ENTRY_LEN {
        memory.write_u8(param_table + offset, 0)?;
    }
    for &(entry, bytes) in INT10_VIDEO_PARAM_ENTRIES {
        let base = param_table + entry * INT10_VIDEO_PARAM_ENTRY_LEN;
        for (offset, byte) in bytes.iter().copied().enumerate() {
            memory.write_u8(base + offset, byte)?;
        }
    }
    Ok(())
}

fn install_boot_bios_stubs(memory: &mut Memory) -> Result<(), BusError> {
    // Low CPU exception vectors and INT 05h Print Screen start as safe BIOS
    // defaults. Guests and DOS can replace them through the IVT.
    for vector in 0x00usize..=0x07 {
        let address = vector * 4;
        memory.write_u16(address, bios_int_stub_off(vector as u8))?;
        memory.write_u16(address + 2, BIOS_ROM_IRET_SEG)?;
    }
    // IRQ0 is real guest ISR code because the PIT can interrupt outside a BIOS
    // service call. It maintains the BDA tick count and chains INT 1Ch.
    memory.write_u16(0x08 * 4, BIOS_TIMER_ISR_ROM_OFF)?;
    memory.write_u16(0x08 * 4 + 2, BIOS_ROM_IRET_SEG)?;
    // Unclaimed master-PIC device IRQs only need to acknowledge the controller.
    // IRQ1 is installed by the resident keyboard BIOS instead.
    for vector in [0x0Ausize, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F] {
        let address = vector * 4;
        memory.write_u16(address, BIOS_MASTER_IRQ_ISR_ROM_OFF)?;
        memory.write_u16(address + 2, BIOS_ROM_IRET_SEG)?;
    }

    // BIOS service interrupts the host intercepts by vector. Their IVT targets
    // point at the ROM IRET so they survive a guest low-memory wipe. INT 33h is
    // the mouse driver and INT 2Fh is the IZCDEX CD bridge; INT 29h is the DOS
    // fast-console hook; INT 25h/26h are the DOS absolute disk read/write; INT
    // 27h is the obsolete TSR exit; INT 28h is the DOS idle hook's default IRET;
    // INT 2Ah is the DOS network/critical-section hook; INT 2Bh-2Dh, 32h,
    // 34h-3Fh, 47h, 4Bh-4Fh, 58h, and 5Dh-5Fh are DOS reserved IRET vectors;
    // 61h-66h, 69h-6Bh, 6Eh, 78h-79h, 7Bh-85h, F0h-F7h, F9h, and FCh-FDh
    // are unused, BASIC-reserved, or user-reserved IRET vectors.
    // 45h, 48h-49h, 59h-5Bh, 6Dh, E0h, EFh, F8h, FAh-FBh, and FEh-FFh are
    // optional vendor or machine-specific entry points with no resident provider.
    // 5Ch, 60h, 68h, 6Fh, 7Ah, 86h, and E4h are optional resident API vectors;
    // the host returns absent while the IVT still points at the default IRET.
    // INT 2Eh is the DOS command-interpreter back door. INT 6Ch is the DOS
    // realtime-clock/resume hook's default IRET. INT 18h/19h are the host-serviced
    // boot and diskless vectors (the run loop services them and redirects CS:IP
    // itself, so the IRET target is only a fallback). INT 1Bh and 1Ch are the
    // Ctrl-Break and timer-tick hooks: no host handlers, just default IRETs so a
    // guest that hooks or calls them through the vectors has valid targets. INT 40h
    // is the relocated floppy handler, routed through the same disk service as
    // INT 13h. INT 42h is the relocated video handler, routed through INT 10h.
    // INT 4Ah is the AT user-alarm hook's default IRET.
    for vector in [
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x25, 0x26, 0x27,
        0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x2F, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38,
        0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E, 0x3F, 0x40, 0x42, 0x47, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F,
        0x45, 0x48, 0x49, 0x4A, 0x58, 0x59, 0x5A, 0x5B, 0x5C, 0x5D, 0x5E, 0x5F, 0x60, 0x61, 0x62,
        0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x6B, 0x6C, 0x6D, 0x6E, 0x6F, 0x78, 0x79,
        0x7A, 0x7B, 0x7C, 0x7D, 0x7E, 0x7F, 0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0xE0, 0xE4,
        0xEF, 0xF0, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0xFA, 0xFB, 0xFC, 0xFD,
        0xFE, 0xFF,
    ] {
        let address = vector * 4;
        memory.write_u16(address, bios_int_stub_off(vector as u8))?;
        memory.write_u16(address + 2, BIOS_ROM_IRET_SEG)?;
    }
    // INT 70h (IRQ8) is a real hardware interrupt, not a host-serviced INT, so its
    // vector points at the RAM ISR stub that acks Register C and EOIs both PICs.
    memory.write_u16(0x70 * 4, BIOS_RTC_ISR_ADDRESS as u16)?;
    memory.write_u16(0x70 * 4 + 2, 0)?;
    // INT 71h-77h are the AT slave-PIC IRQ9-IRQ15 defaults. With no BIOS
    // device handler installed, acknowledge the interrupt and return.
    for vector in [0x71usize, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77] {
        memory.write_u16(vector * 4, BIOS_SLAVE_IRQ_ISR_ADDRESS as u16)?;
        memory.write_u16(vector * 4 + 2, 0)?;
    }
    install_rtc_isr_stub(memory)?;
    install_slave_irq_isr_stub(memory)?;
    install_dos_low_memory_stubs(memory)?;
    seed_int1d_video_parameter_table(memory)?;
    seed_int1e_diskette_parameter_table(memory)?;
    seed_int1f_graphics_font_table(memory)?;
    seed_int43_font_table(memory)?;
    seed_int44_font_table(memory)?;
    seed_int46_absent_fixed_disk_table(memory)?;
    seed_video_bios_tables(memory)?;
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
    memory.write_u8(0x465, 0x29)?; // CGA mode-control shadow (80x25 color text)
    memory.write_u8(0x466, 0x00)?; // CGA color-select shadow
    memory.write_u8(0x484, 24)?; // rows on screen minus one
    memory.write_u16(0x485, 16)?; // character cell height in scan lines
    memory.write_u8(0x487, 0x60)?; // EGA/VGA video-control byte
    memory.write_u8(0x488, 0xf9)?; // EGA/VGA switches / feature bits
    memory.write_u8(0x489, 0x51)?; // cursor emulation enabled, 400-line alphanumeric mode
    memory.write_u8(0x48A, 0x08)?; // display-combination code: VGA colour
    seed_bda_video_save_pointer(memory)?;
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
    Ok(())
}

fn install_dos_low_memory_stubs(memory: &mut Memory) -> Result<(), BusError> {
    memory.write_u8(BIOS_IRET_STUB_ADDRESS, 0xcf)?;
    memory.write_u8(DOS_CALL5_ENTRY_ADDRESS, 0xea)?; // jmp far FF00:0020
    memory.write_u16(DOS_CALL5_ENTRY_ADDRESS + 1, DOS_CALL5_ENTRY_OFF)?;
    memory.write_u16(DOS_CALL5_ENTRY_ADDRESS + 3, DOS_CALL5_ENTRY_SEG)?;
    // A safe HLT sentinel byte in the reserved low-memory stub cluster.
    memory.write_u8(SYSINIT_HALT_STUB, 0xf4)?;
    // INT 18h's halt target: CLI;HLT in low RAM. INT 22h uses the same safe
    // default terminate-address target until a shell or guest replaces it.
    memory.write_u8(BIOS_HALT_STUB_ADDRESS, 0xfa)?;
    memory.write_u8(BIOS_HALT_STUB_ADDRESS + 1, 0xf4)?;
    memory.write_u8(BIOS_CRITICAL_ERROR_RETURN_STUB_ADDRESS, 0xf4)?;
    memory.write_u8(DOS_INT23_DEFAULT_STUB_ADDRESS, 0xb8)?; // mov ax,4C00h
    memory.write_u8(DOS_INT23_DEFAULT_STUB_ADDRESS + 1, 0x00)?;
    memory.write_u8(DOS_INT23_DEFAULT_STUB_ADDRESS + 2, 0x4c)?;
    memory.write_u8(DOS_INT23_DEFAULT_STUB_ADDRESS + 3, 0xcd)?; // int 21h
    memory.write_u8(DOS_INT23_DEFAULT_STUB_ADDRESS + 4, 0x21)?;
    memory.write_u8(DOS_INT24_DEFAULT_STUB_ADDRESS, 0xb0)?; // mov al,03h
    memory.write_u8(DOS_INT24_DEFAULT_STUB_ADDRESS + 1, 0x03)?;
    memory.write_u8(DOS_INT24_DEFAULT_STUB_ADDRESS + 2, 0xcf)?; // iret

    for (vector, target) in [
        (0x20usize, BIOS_IRET_STUB_ADDRESS),
        (0x21, BIOS_IRET_STUB_ADDRESS),
        (0x22, BIOS_HALT_STUB_ADDRESS),
        (0x23, DOS_INT23_DEFAULT_STUB_ADDRESS),
        (0x24, DOS_INT24_DEFAULT_STUB_ADDRESS),
    ] {
        let address = vector * 4;
        memory.write_u16(address, target as u16)?;
        memory.write_u16(address + 2, 0)?;
    }
    Ok(())
}

/// Write the default INT 70h (IRQ8) handler into low RAM: acknowledge the RTC by
/// reading Register C (which clears its flags and de-asserts the line) and send
/// end-of-interrupt to both 8259 PICs, then IRET. This is the minimum a real BIOS
/// INT 70h does before chaining to any user routine. A guest that masks IRQ8 or
/// installs its own handler simply overwrites this vector.
///
/// Limit: the real BIOS INT 70h also tests the RTC wait flag (0040:00A0) and
/// signals the INT 15h AH=83h/86h event-wait completion at 0040:0098. The host
/// INT 15h AH=83h path completes waits synchronously, so this stub only acks
/// and EOIs.
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

/// Write the shared IRQ12/IRQ13/IRQ14 default handler into low RAM. These arrive
/// through the slave PIC, so a default handler must EOI both controllers before
/// returning.
fn install_slave_irq_isr_stub(memory: &mut Memory) -> Result<(), BusError> {
    const STUB: [u8; 9] = [
        0x50, // push ax
        0xb0, 0x20, // mov al,20h
        0xe6, 0xa0, // out A0h,al
        0xe6, 0x20, // out 20h,al
        0x58, // pop ax
        0xcf, // iret
    ];
    for (offset, &byte) in STUB.iter().enumerate() {
        memory.write_u8(BIOS_SLAVE_IRQ_ISR_ADDRESS + offset, byte)?;
    }
    Ok(())
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

fn write_ivt_pointer(memory: &mut Memory, vector: u8, linear: u32) -> Result<(), BusError> {
    let address = usize::from(vector) * 4;
    memory.write_u16(address, (linear & 0x0f) as u16)?;
    memory.write_u16(address + 2, (linear >> 4) as u16)
}

fn seed_int1d_video_parameter_table(memory: &mut Memory) -> Result<(), BusError> {
    const TEXT_40X25: [u8; 16] = [
        0x38, 0x28, 0x2d, 0x0a, 0x1f, 0x06, 0x19, 0x1c, 0x02, 0x07, 0x06, 0x07, 0x00, 0x00, 0x00,
        0x00,
    ];
    const TEXT_80X25: [u8; 16] = [
        0x71, 0x50, 0x5a, 0x0a, 0x1f, 0x06, 0x19, 0x1c, 0x02, 0x07, 0x06, 0x07, 0x00, 0x00, 0x00,
        0x00,
    ];
    const CGA_320X200: [u8; 16] = [
        0x38, 0x28, 0x2d, 0x0a, 0x7f, 0x06, 0x64, 0x70, 0x02, 0x01, 0x06, 0x07, 0x00, 0x00, 0x00,
        0x00,
    ];
    const CGA_640X200: [u8; 16] = [
        0x71, 0x50, 0x5a, 0x0a, 0x7f, 0x06, 0x64, 0x70, 0x02, 0x01, 0x06, 0x07, 0x00, 0x00, 0x00,
        0x00,
    ];
    const MDA_TEXT_80X25: [u8; 16] = [
        0x61, 0x50, 0x52, 0x0f, 0x19, 0x06, 0x19, 0x19, 0x02, 0x0d, 0x0b, 0x0c, 0x00, 0x00, 0x00,
        0x00,
    ];
    const TABLE: [[u8; 16]; 8] = [
        TEXT_40X25,
        TEXT_40X25,
        TEXT_80X25,
        TEXT_80X25,
        CGA_320X200,
        CGA_320X200,
        CGA_640X200,
        MDA_TEXT_80X25,
    ];

    let base = VGA_BIOS_INT1D_VIDEO_TABLE_ADDR as usize;
    for (mode, regs) in TABLE.iter().enumerate() {
        for (offset, &byte) in regs.iter().enumerate() {
            memory.write_u8(base + mode * regs.len() + offset, byte)?;
        }
    }
    write_ivt_pointer(memory, 0x1d, VGA_BIOS_INT1D_VIDEO_TABLE_ADDR)
}

fn seed_int1e_diskette_parameter_table(memory: &mut Memory) -> Result<(), BusError> {
    const DPT_1440K: [u8; 11] = [
        0xdf, 0x02, 0x25, 0x02, 0x12, 0x1b, 0xff, 0x6c, 0xf6, 0x0f, 0x08,
    ];

    let base = BIOS_DISKETTE_PARAMETER_TABLE_ADDR as usize;
    for (offset, &byte) in DPT_1440K.iter().enumerate() {
        memory.write_u8(base + offset, byte)?;
    }
    write_ivt_pointer(memory, 0x1e, BIOS_DISKETTE_PARAMETER_TABLE_ADDR)
}

fn seed_int1f_graphics_font_table(memory: &mut Memory) -> Result<(), BusError> {
    let upper_half = &izarravm_video::font::VGAFONT_8X8[0x80 * 8..];
    for (offset, &byte) in upper_half.iter().enumerate() {
        memory.write_u8(VGA_BIOS_INT1F_FONT_ADDR as usize + offset, byte)?;
    }
    write_ivt_pointer(memory, 0x1f, VGA_BIOS_INT1F_FONT_ADDR)
}

fn seed_int43_font_table(memory: &mut Memory) -> Result<(), BusError> {
    for (offset, &byte) in izarravm_video::font::VGAFONT_8X16.iter().enumerate() {
        memory.write_u8(VGA_BIOS_INT43_FONT_ADDR as usize + offset, byte)?;
    }
    memory.write_u16(0x43 * 4, VGA_BIOS_FONT_TABLE_OFF)?;
    memory.write_u16(0x43 * 4 + 2, (VGA_BIOS_BASE >> 4) as u16)
}

fn seed_int44_font_table(memory: &mut Memory) -> Result<(), BusError> {
    for (offset, &byte) in izarravm_video::font::VGAFONT_8X8.iter().enumerate() {
        memory.write_u8(VGA_BIOS_INT44_FONT_ADDR as usize + offset, byte)?;
    }
    memory.write_u16(0x44 * 4, VGA_BIOS_INT44_FONT_OFF)?;
    memory.write_u16(0x44 * 4 + 2, (VGA_BIOS_BASE >> 4) as u16)
}

fn seed_int46_absent_fixed_disk_table(memory: &mut Memory) -> Result<(), BusError> {
    memory.write_u16(0x46 * 4, 0)?;
    memory.write_u16(0x46 * 4 + 2, 0)
}

impl MachineBus<'_> {
    fn video_io_disabled_for_port(&self, port: u16) -> bool {
        !self.video.video_subsystem_enabled() && port != 0x3C3 && (0x3B0..=0x3DF).contains(&port)
    }

    fn video_io_enabled_for_port(&self, port: u16) -> bool {
        self.video.video_subsystem_enabled() || port == 0x3C3
    }

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

    fn distira_mmio_offset(&self, address: u32, width: usize) -> Option<usize> {
        self.pci
            .distira_mmio_offset(address, width)
            .or_else(|| distira_mmio_offset(address, width))
    }

    fn distira_lfb_offset(&self, address: u32, width: usize) -> Option<usize> {
        self.pci
            .distira_lfb_offset(address, width)
            .or_else(|| distira_lfb_offset(address, width))
    }

    fn distira_texture_offset(&self, address: u32, width: usize) -> Option<usize> {
        self.pci.distira_texture_offset(address, width)
    }

    fn distira_cmdfifo_offset(&self, address: u32, width: usize) -> Option<usize> {
        self.pci.distira_cmdfifo_offset(address, width)
    }

    fn video_text_offset(&self, address: u32, width: usize) -> Option<usize> {
        // The Hercules personality has its own dedicated B0000-BFFFF decode
        // (`hgc_offset`, checked first by both callers below): it must not
        // also fall through to this single-sliding-window text/CGA decode,
        // which would let an unpaged-in B8000 access reach `text_memory` as
        // an MDA/CGA text write instead of correctly missing.
        if self.video.is_hercules_personality() {
            return None;
        }
        self.video
            .video_memory_enabled()
            .then(|| video_text_offset(self.video.text_memory_base(), address, width))
            .flatten()
    }

    /// The Hercules graphics window, B0000-BFFFF: unlike the single sliding
    /// 32K window `video_text_offset` decodes for text/CGA, both Hercules pages
    /// are simultaneously addressable at their real hardware addresses (page 0
    /// always at B0000, page 1 at B8000 once 3BFh pages it in), independent of
    /// which page the CRTC is currently scanning out. Only live while the
    /// Hercules personality is active; text mode 07h (also mono, also
    /// B0000-based) keeps using `video_text_offset` as before.
    fn hgc_offset(&self, address: u32, width: usize) -> Option<usize> {
        if !self.video.video_memory_enabled() || !self.video.is_hercules_personality() {
            return None;
        }
        let end = VGA_MONO_TEXT_BASE + (HGC_FB_SIZE as u32 * 2);
        if !(VGA_MONO_TEXT_BASE..end).contains(&address) || address + width as u32 > end {
            return None;
        }
        let offset = (address - VGA_MONO_TEXT_BASE) as usize;
        // Page 1 (B8000-BFFFF, offset 0x8000..0x10000) only decodes once 3BFh
        // has paged it in; otherwise that half is open bus (unmapped), matching
        // real hardware where the second bank simply is not there.
        if offset >= HGC_FB_SIZE && !self.video.hgc_page1_addressable() {
            return None;
        }
        Some(offset)
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

    #[inline]
    fn direct_ram_range(&self, address: u32, width: BusWidth) -> Option<(usize, usize)> {
        self.direct_ram_bytes(address, width.bytes() as usize)
    }

    #[inline]
    fn direct_ram_bytes(&self, address: u32, bytes: usize) -> Option<(usize, usize)> {
        let start = address as usize;
        let end = start.checked_add(bytes)?;
        if end <= 0x000A_0000 && end <= self.memory.len() {
            return Some((start, end));
        }
        self.ram_lookup.direct_bytes(address, bytes)
    }

    #[inline]
    fn direct_page_ram_bytes(
        &self,
        address: u32,
        bytes: usize,
        access_width: BusWidth,
    ) -> Option<(u32, usize, usize)> {
        let gated = self.apply_a20(address);
        if gated != address || bytes == 0 {
            return None;
        }
        if should_split(gated, access_width)
            || ((gated as usize & RAM_LOOKUP_PAGE_MASK) + bytes > RAM_LOOKUP_PAGE_SIZE)
        {
            return None;
        }
        self.direct_ram_bytes(gated, bytes)
            .map(|(start, end)| (gated, start, end))
    }

    /// The plane-window offset for an access that the guest-selected GC06 graphics
    /// aperture redirects. Only graphics modes consult the aperture; text and CGA
    /// keep the fixed B8000 decode.
    fn vga_gfx_offset(&self, address: u32, width: usize) -> Option<usize> {
        if !self.video.video_memory_enabled() {
            return None;
        }
        match self.video.active_mode() {
            VideoMode::Planar | VideoMode::ModeX | VideoMode::Mode13h => {
                let ap = self.video.gfx_aperture();
                vga_gfx_aperture_offset(ap.base, ap.length, address, width)
            }
            VideoMode::Text | VideoMode::Cga | VideoMode::Hercules => None,
        }
    }

    fn read_phys_u8(&mut self, address: u32) -> Result<u8, BusError> {
        let mut byte = [0];
        self.read_phys(address, &mut byte)?;
        Ok(byte[0])
    }

    fn read_phys(&mut self, address: u32, out: &mut [u8]) -> Result<(), BusError> {
        let width = out.len();
        if width == 0 {
            return Ok(());
        }

        if let Some((start, end)) = self.direct_ram_bytes(address, width) {
            out.copy_from_slice(&self.memory.as_slice()[start..end]);
            return Ok(());
        }

        if let Some(offset) = rom_offset(address, width) {
            out.copy_from_slice(&self.rom[offset..offset + width]);
            return Ok(());
        }

        // A guest that moves the graphics aperture through GC06 (memory map select)
        // redirects the framebuffer window. When the active mode is a graphics mode
        // and GC06 points at a moved window, route the access through the planar /
        // chain-4 datapath before the fixed text/CGA window decode below.
        if let Some(offset) = self.vga_gfx_offset(address, width) {
            for (i, byte) in out.iter_mut().enumerate() {
                *byte = match self.video.active_mode() {
                    VideoMode::Mode13h => self.video.cpu_read_chain4(offset + i),
                    _ => self.video.cpu_read(offset + i),
                };
            }
            return Ok(());
        }

        // Hercules graphics: both B0000 (page 0) and B8000 (page 1, once paged
        // in) are live simultaneously, unlike the single sliding text/CGA
        // window below, so this is checked first and independently.
        if let Some(offset) = self.hgc_offset(address, width) {
            for (index, byte) in out.iter_mut().enumerate() {
                *byte = self.video.hgc_read(offset + index);
            }
            return Ok(());
        }

        if let Some(offset) = self.video_text_offset(address, width) {
            // In a CGA graphics mode the B800 aperture is the 16 KiB CGA
            // framebuffer; in text mode it is the character/attribute buffer.
            let cga_window = self.video.is_cga_personality();
            let cga_graphics = self.video.active_mode() == VideoMode::Cga;
            for (index, byte) in out.iter_mut().enumerate() {
                let byte_offset = if cga_window {
                    (offset + index) & (CGA_FB_SIZE - 1)
                } else {
                    offset + index
                };
                *byte = if cga_graphics {
                    self.video.cga_read(byte_offset)
                } else {
                    self.video
                        .read_u8(byte_offset)
                        .map_err(|_| BusError::UnmappedMemory { address })?
                };
            }
            return Ok(());
        }

        // The 64 KB A0000 window serves all three graphics modes. Unchained (mode
        // X) and 16-color planar route through the planar datapath (cpu_read loads
        // the VGA latches as a side effect, so it needs &mut self); chained mode
        // 13h routes through the chain-4 decode.
        if self.video.video_memory_enabled() {
            if let Some(offset) = vga_planar_offset(address, width) {
                match self.video.active_mode() {
                    VideoMode::Planar | VideoMode::ModeX => {
                        for (i, byte) in out.iter_mut().enumerate() {
                            *byte = self.video.cpu_read(offset + i);
                        }
                        return Ok(());
                    }
                    VideoMode::Mode13h => {
                        for (i, byte) in out.iter_mut().enumerate() {
                            *byte = self.video.cpu_read_chain4(offset + i);
                        }
                        return Ok(());
                    }
                    // Text, CGA, and Hercules do not decode the A0000 window; fall through.
                    VideoMode::Text | VideoMode::Cga | VideoMode::Hercules => {}
                }
            }
        }

        if let Some(offset) = margo_lfb_offset(address, width) {
            for (index, byte) in out.iter_mut().enumerate() {
                *byte = self.margo.read_vram_u8(offset + index);
            }
            return Ok(());
        }

        if let Some(offset) = margo_mmio_offset(address, width) {
            for (index, byte) in out.iter_mut().enumerate() {
                *byte = self.margo.read_mmio_u8(offset + index);
            }
            return Ok(());
        }

        if let Some(offset) = self.distira_lfb_offset(address, width) {
            if width == 1 {
                out[0] = 0xff;
                return Ok(());
            }
            for (index, byte) in out.iter_mut().enumerate() {
                *byte = self.distira.read_lfb_u8(offset + index);
            }
            return Ok(());
        }

        if let Some(offset) = self.distira_mmio_offset(address, width) {
            for (index, byte) in out.iter_mut().enumerate() {
                *byte = self.distira.read_mmio_u8(offset + index);
            }
            return Ok(());
        }

        if is_open_bus_uma(address, width) {
            // Unoccupied upper memory: open bus reads as 0xFF, matching a real
            // machine's floating data bus over an adapter-free UMA hole.
            out.fill(0xff);
            return Ok(());
        }

        let end = address as usize + width;
        if end <= self.memory.len() {
            out.copy_from_slice(&self.memory.as_slice()[address as usize..end]);
            return Ok(());
        }

        Err(BusError::UnmappedMemory { address })
    }

    fn write_memory_byte(&mut self, address: u32, value: u8) -> Result<(), BusError> {
        if let Some((addr, _)) = self.direct_ram_bytes(address, 1) {
            return self.memory.write_u8(addr, value);
        }

        if rom_offset(address, 1).is_some() {
            return Ok(());
        }

        if is_open_bus_uma(address, 1) {
            // Unoccupied upper memory: open bus, a write with nothing wired to
            // receive it.
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

        // Hercules graphics: see the matching check in `read_phys`.
        if let Some(offset) = self.hgc_offset(address, 1) {
            self.video.hgc_write(offset, value);
            return Ok(());
        }

        if let Some(offset) = self.video_text_offset(address, 1) {
            // In a CGA graphics mode the B800 aperture is the 16 KiB CGA
            // framebuffer; in text mode it is the character/attribute buffer.
            let offset = if self.video.is_cga_personality() {
                offset & (CGA_FB_SIZE - 1)
            } else {
                offset
            };
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
        if self.video.video_memory_enabled() {
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
                    // Text, CGA, and Hercules do not decode the A0000 window; fall through.
                    VideoMode::Text | VideoMode::Cga | VideoMode::Hercules => {}
                }
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

        if self.distira_lfb_offset(address, 1).is_some() {
            return Ok(());
        }

        if let Some(offset) = self.distira_mmio_offset(address, 1) {
            self.distira.write_mmio_u8(offset, value);
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

        // A disk -> memory transfer wrote guest RAM directly via the DMA controller, bypassing the
        // CPU's self-modifying-code tracking. If that RAM held cached code (a loaded overlay or boot
        // stage later re-entered by a near branch, which would not otherwise invalidate), the decode
        // cache and prefetch must drop it. This runs in the bus, so flag it; the run loop calls the
        // CPU's note_device_memory_write at the end of the step (where the A20 seam also lives).
        if req.read && moved_any {
            *self.device_wrote_memory = true;
        }
    }

    /// Wait-states to charge for a DATA access at the post-A20 physical `address`,
    /// routed through the cosmetic cache so its tag state stays warm. The cache
    /// tiers ONLY cacheable RAM: a ROM or video/MMIO window keeps its existing
    /// `memory_wait_states` cost UNCHANGED (it is never cached, so it must not warm
    /// the model nor be re-timed by it). Cacheable RAM (conventional `< 0xA0000`
    /// and any extended RAM that is not a device window) is tiered, and the resolved
    /// tier's per-mode cost is charged.
    fn data_access_wait_states(&mut self, address: u32, width: BusWidth) -> u8 {
        if address >= 0x000A_0000 && self.is_device_window(address, width) {
            // Device/ROM: untiered, unchanged timing (both classes).
            return self.memory_wait_states(address);
        }
        if self.flat_data_cost {
            // Approximate class (486/586): charge the flat L1-resident cost and skip
            // the per-access tag-array tiering (the Slice-0 measured floor). The
            // benchmarks are L1-resident so cyc/iter stays near the accurate model;
            // the win is skipping ~3M tag lookups per run. Guest-invisible: only time.
            return self.cache.cost.l1;
        }
        self.cache.data_wait_states(address, width)
    }

    /// Wait-states for a single code-fetch byte at the post-A20 physical `address`.
    /// Code in cacheable RAM is charged the per-mode L1 constant (code is assumed
    /// I-cache resident); code fetched from ROM/device keeps `memory_wait_states`,
    /// so firmware/POST and any execution out of a device window are unchanged.
    fn code_fetch_wait_states(&self, address: u32) -> u8 {
        if address >= 0x000A_0000 && self.is_device_window(address, BusWidth::Byte) {
            self.memory_wait_states(address)
        } else {
            self.cache.code_fetch_wait_states()
        }
    }

    /// True iff `address` (post-A20, width `width`) lands in a ROM or video/MMIO
    /// window the cache must not tier. Mirrors the device-classification arm of
    /// `memory_wait_states_device` (the `wait_states.rom`/`wait_states.video`
    /// branches); the fall-through (cacheable RAM) returns false. Only called for
    /// `address >= 0xA0000`, so conventional RAM never reaches here.
    fn is_device_window(&self, address: u32, width: BusWidth) -> bool {
        let bytes = width.bytes() as usize;
        rom_offset(address, bytes).is_some()
            || self.vga_gfx_offset(address, bytes).is_some()
            || self.video_text_offset(address, bytes).is_some()
            || (self.video.video_memory_enabled() && vga_planar_offset(address, bytes).is_some())
            || margo_lfb_offset(address, bytes).is_some()
            || margo_mmio_offset(address, bytes).is_some()
            || self.distira_lfb_offset(address, bytes).is_some()
            || self.distira_cmdfifo_offset(address, bytes).is_some()
            || self.distira_mmio_offset(address, bytes).is_some()
    }

    #[inline]
    fn memory_wait_states(&self, address: u32) -> u8 {
        // Conventional RAM (below the 0xA0000 video aperture) is never overlapped
        // by a ROM, VGA, Margo, or Distira window, so it always runs at RAM speed.
        // The hot fetch/data path hits this on every access, so keep it a tiny
        // inlinable check and defer the device-window gauntlet to a cold helper.
        // This matches the fall-through the gauntlet would reach anyway (it already
        // classifies by the base address only).
        if address < 0x000A_0000 {
            return self.wait_states.ram;
        }
        self.memory_wait_states_device(address)
    }

    #[cold]
    fn memory_wait_states_device(&self, address: u32) -> u8 {
        if rom_offset(address, 1).is_some() {
            self.wait_states.rom
        } else if self.vga_gfx_offset(address, 1).is_some()
            || self.video_text_offset(address, 1).is_some()
            || (self.video.video_memory_enabled() && vga_planar_offset(address, 1).is_some())
            || margo_lfb_offset(address, 1).is_some()
            || margo_mmio_offset(address, 1).is_some()
            || self.distira_lfb_offset(address, 1).is_some()
            || self.distira_cmdfifo_offset(address, 1).is_some()
            || self.distira_mmio_offset(address, 1).is_some()
        {
            // The Approximate class charges the era bus latency of a real video
            // card (see `video_wait_states_approx`); the Accurate class keeps the
            // frozen profile value bit-for-bit.
            match self.active_mode.timing_class() {
                TimingClass::Accurate => self.wait_states.video,
                TimingClass::Approximate => {
                    video_wait_states_approx(cpu_level_for_mode(self.active_mode))
                }
            }
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

/// True if `address` (for an access of `width` bytes, entirely) falls in the
/// unoccupied part of the upper-memory area: the UMB-able holes between the
/// video option ROM span and the system BIOS, 0xC8000-0xEFFFF. On a real
/// machine nothing answers there unless an adapter or a memory manager's
/// page-frame claims it; this machine's own occupants (VGA BIOS data tables,
/// the code-page font bank, TOKAEMM's linear-to-extended-RAM UMB remap) all
/// live below 0xC8000 or are reached through paging at a physical address
/// above 1 MiB, so this range check never needs to special-case them.
///
/// Guests that probe the UMA for a free window (JEMMEX and other EMS/UMB
/// managers scanning for a page frame) rely on this reading as open bus
/// (conventionally 0xFF, writes ignored), exactly like the existing
/// 0x201/0x280-0x28F/0x5658 open-bus port conventions but for memory instead
/// of I/O space.
fn is_open_bus_uma(address: u32, width: usize) -> bool {
    let uma_occupied_end = UPPER_MEMORY_BASE + VGA_BIOS_SPAN_SIZE;
    let Some(end) = address.checked_add(width as u32) else {
        return false;
    };
    address >= uma_occupied_end && end <= SYSTEM_ROM_BASE
}

fn video_text_offset(base: u32, address: u32, width: usize) -> Option<usize> {
    let end = base + VGA_TEXT_MEMORY_SIZE as u32;
    if (base..end).contains(&address) && address + width as u32 <= end {
        Some((address - base) as usize)
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
/// datapath. The VGA datapath addresses a 64 KB plane window; map-select 00's
/// 128 KB host window mirrors that 64 KB plane window twice.
fn vga_gfx_aperture_offset(base: u32, length: u32, address: u32, width: usize) -> Option<usize> {
    let end = base + length;
    if (base..end).contains(&address) && address + width as u32 <= end {
        let offset = ((address - base) % VGA_PLANAR_WINDOW_SIZE) as usize;
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

fn distira_lfb_offset(address: u32, width: usize) -> Option<usize> {
    let end = DISTIRA_LFB_BASE + DISTIRA_FB_SIZE as u32;
    if (DISTIRA_LFB_BASE..end).contains(&address) && address + width as u32 <= end {
        Some((address - DISTIRA_LFB_BASE) as usize)
    } else {
        None
    }
}

fn distira_mmio_offset(address: u32, width: usize) -> Option<usize> {
    let end = DISTIRA_MMIO_BASE + DISTIRA_MMIO_SIZE as u32;
    if (DISTIRA_MMIO_BASE..end).contains(&address) && address + width as u32 <= end {
        Some((address - DISTIRA_MMIO_BASE) as usize)
    } else {
        None
    }
}

#[cfg(test)]
#[path = "machine_test.rs"]
mod tests;
