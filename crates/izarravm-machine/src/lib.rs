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
mod bios;
mod cdimage;
mod dma;
mod dos;
mod pci;

pub(crate) use pci::PciConfig;
mod cache_config;
mod ram_lookup;
mod timing;
#[cfg(test)]
use timing::advance_fractional;
mod video;
mod video_params;

pub(crate) use ram_lookup::RamPageLookup;
pub(crate) use timing::{DAC_HZ, OPL_NATIVE_HZ, PIT_INPUT_HZ};

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
mod run;
mod speaker;
mod storage;
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

    /// Whether the PC speaker was ever enabled (port 0x61 bit 1 driven high). The
    /// power-on chime sets this during POST, so a headless run can assert the
    /// speaker was exercised without draining the audio ring.
    pub fn speaker_ever_enabled(&self) -> bool {
        self.speaker.ever_enabled()
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

    /// Whether the guest is executing in virtual-8086 mode (under the TOKAEMM
    /// ring-0 monitor). Exposed so the SP-4b M4 default-boot e2e can assert the
    /// default CONFIG.SYS really put the system in V86.
    pub fn in_v86(&self) -> bool {
        self.cpu.is_v86_mode()
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
