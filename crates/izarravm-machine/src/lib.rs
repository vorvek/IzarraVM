// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

pub use fat32::{
    FAT_ATTR_DIRECTORY, FAT32_EOC, Fat32Geometry, Fat32Table, fat32_boot_sector, fat32_dir_entry,
    fat32_dot_entries, fat32_fsinfo_sector, fat32_geometry, fat32_is_eoc,
};
pub use fat32_volume::{Fat32Volume, build_fat32};
use izarravm_audio::{Ad1848, Ad1848Config, Mpu401, OplChip, Resampler, TimedMidiMessage};
use izarravm_bus::{
    BusAccessKind, BusCycle, BusError, BusTrace, BusWidth, CompiledBusDelta, CompiledBusWindow,
    CpuBus, DirectMemoryRead, DirectMemoryWrite, DirectPage, Memory, NativeVgaWrites, TracingMode,
};
use izarravm_core::{
    CpuPersona, GswMode, HardwareProfile, MIDI_MPU_BASE, SoundBlasterConfig, VideoCard,
    WAVETABLE_MPU_BASE, WssConfig,
};
pub use izarravm_cpu::PerfCounters;
use izarravm_cpu::{CpuError, CpuGsw, CycleOutcome, SegmentIndex, SegmentRegister, bus_timing};
#[cfg(test)]
use izarravm_video::HGC_FB_SIZE;
use izarravm_video::{
    CGA_FB_SIZE, DAC_ENTRIES, MARGO_VBE_MODES, TextFrame, VGA_PLANAR_WINDOW_SIZE,
    VGA_TEXT_MEMORY_SIZE, VGA_TEXT_PAGE_STRIDE, bytes_per_pixel, font, pixel_format, vbe_mode,
};
pub use izarravm_video::{MARGO_ID_VALUE, VideoMode};
#[cfg(test)]
use izarravm_video::{Margo, Vga, VgaRaster};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU8, Ordering};
use thiserror::Error;

mod ata;
mod atapi;
mod bios;
mod bmide;
mod bus;
mod canonical_state;
mod cdimage;
mod dma;
mod dos;
mod pci;

use bus::DevicePorts;
pub(crate) use pci::PciConfig;
mod cache_config;
mod ram_lookup;
mod sb16_path;
mod timeline;
mod timing;
mod vega;
mod vga_wipe_census;
mod video;
mod video_params;

use timeline::{DeviceAdvance, DeviceRates, RatePhase, Timeline};
use vega::{Vega, VideoWrite};

use sb16_path::{Ct1745Mix, Sb16Path, Sb16RenderWindow};

/// Lightweight live state for the CD-ROM controls and status display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CdAudioState {
    pub media_present: bool,
    pub audio_capable: bool,
    pub playing: bool,
    pub paused: bool,
    /// Raw CT1745 register `0x36` level (`0..=31`).
    pub left_level: u8,
    /// Raw CT1745 register `0x37` level (`0..=31`).
    pub right_level: u8,
}

pub(crate) use ram_lookup::RamPageLookup;
#[cfg(test)]
pub(crate) use timing::PIT_INPUT_HZ;
pub(crate) use timing::{DAC_HZ, DAC_PENDING_FRAME_CAP, MIX_HEADROOM, OPL_NATIVE_HZ};

pub(crate) use cache_config::{
    CACHE_L1_MAX_LINES, CACHE_L2_MAX_LINES, CACHE_TIER_DISABLED_MASK, CacheLevelConfig, TierCost,
    cache_level_config, code_fetch_ws, tier_cost,
};

#[allow(unused_imports)]
pub(crate) use video_params::{
    DISTIRA_PCI_BAR_SIZE, DISTIRA_PCI_DEVICE_ID, DISTIRA_PCI_LFB_OFFSET, DISTIRA_PCI_REVISION,
    DISTIRA_PCI_SLOT, DISTIRA_PCI_TEX_OFFSET, DISTIRA_PCI_VENDOR_ID, INT10_STATE_BDA_LEN,
    INT10_STATE_CGA_LATCH_MARKER, INT10_STATE_CGA_LATCH_OFFSET, INT10_STATE_DAC_LEN,
    INT10_STATE_HARDWARE_LEN, INT10_STATIC_FUNCTIONALITY, INT10_VIDEO_PARAM_ENTRIES,
    INT10_VIDEO_PARAM_ENTRY_LEN, PCI_CONFIG_ADDRESS_PORT, PCI_CONFIG_DATA_END,
    PCI_CONFIG_DATA_PORT, RAM_LOOKUP_PAGE_BITS, RAM_LOOKUP_PAGE_MASK, RAM_LOOKUP_PAGE_SIZE,
    RAM_LOOKUP_SLOW,
};
mod fat32;
mod fat32_volume;
mod fat_name;
mod fdc;
mod firmware_contract;
mod floppy;
mod gameport;
mod ide;
mod iso9660;
mod katea_names;
mod katea_store;
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
mod sector_cache;
mod speaker;
mod storage;
mod uart;
mod unittester;

use bios::BootDevice;
pub(crate) use firmware_contract::address::{
    BDA_DAY_COUNT, BDA_RTC_WAIT_COMPLETE, BDA_RTC_WAIT_FLAG, BDA_RTC_WAIT_PENDING,
    BDA_RTC_WAIT_TIMEOUT, BDA_VIDEO_SAVE_POINTER, BIOS_BOOT_CHOICE_ADDR, BIOS_CONFIG_TABLE_ADDR,
    BIOS_DISKETTE_PARAMETER_TABLE_ADDR, BIOS_FIXED_DISK_PARAMETER_TABLE_ADDR,
    BIOS_FONT_8X8_HIGH_ROM_OFFSET, BIOS_FONT_8X8_ROM_OFFSET, BIOS_FONT_8X14_ROM_OFFSET,
    BIOS_FONT_8X16_ROM_OFFSET, BIOS_HALT_STUB_ADDRESS, BIOS_INT_STUB_TABLE_LEN,
    BIOS_INT_STUB_TABLE_LINEAR, BIOS_LEGACY_IRET_LINEAR, BIOS_POST_ERROR_LOG_ADDR,
    BIOS_POST_ERROR_LOG_COUNT_ADDR, BIOS_POST_ERROR_LOG_MAX, BIOS_ROM_IRET_SEG, BIOS_ROM_SEGMENT,
    BIOS_STUB_WINDOW_LEN, BIOS32_DIRECTORY_LINEAR, BIOS32_PCI_LINEAR, BIOS32_PCI_ROM_OFFSET,
    CMOS_AUDIO_MAGIC, CMOS_AUDIO_MAGIC_VALUE, CMOS_GSW_MODE, CMOS_MPU_PORT,
    CMOS_PRIMARY_BOOT_DEVICE, CMOS_SB_DMA8, CMOS_SB_DMA16, CMOS_SB_IRQ, CMOS_WSS_DMA, CMOS_WSS_IRQ,
    CODEPAGE_FONT_WINDOW, CONVENTIONAL_MEMORY_TOP, EBDA_CD_BOOTABLE_OFF, EBDA_LINEAR,
    EBDA_MOUSE_HANDLER_OFF, EBDA_MOUSE_PKT_SIZE_OFF, EBDA_SEGMENT,
    INT10_FUNCTIONALITY_TABLE_OFFSET, INT10_VIDEO_SAVE_POINTER_TABLE_OFFSET, RESULT_BLOCK_ADDRESS,
    VGA_BIOS_FONT_TABLE_OFF, VGA_BIOS_INT43_FONT_ADDR, VGA_BIOS_SEGMENT, VGA_BIOS_SPAN_SIZE,
    bios_int_stub_off,
};
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use firmware_contract::address::{
    BIOS_CRITICAL_ERROR_RETURN_STUB_ADDRESS, BIOS_INT_STUB_TABLE_ROM_OFFSET,
    BIOS_IRET_STUB_ADDRESS, BIOS_LEGACY_IRET_ROM_OFFSET, BIOS_MASTER_IRQ_ISR_ROM_OFF,
    BIOS_MASTER_IRQ_ISR_ROM_OFFSET, BIOS_RTC_ISR_ADDRESS, BIOS_SLAVE_IRQ_ISR_ADDRESS,
    BIOS_TIMER_ISR_ROM_OFF, BIOS_TIMER_ISR_ROM_OFFSET, BIOS32_DIRECTORY_ROM_OFFSET,
    BIOS32_HEADER_ROM_OFFSET, CONVENTIONAL_MEMORY_KIB, DOS_INT23_DEFAULT_STUB_ADDRESS,
    DOS_INT24_DEFAULT_STUB_ADDRESS, EBDA_MOUSE_INDEX_OFF, EBDA_MOUSE_PACKET_OFF,
    INT10_VIDEO_PARAM_TABLE_ENTRIES, INT10_VIDEO_PARAM_TABLE_OFFSET,
    INT10_VIDEO_SAVE_POINTER_TABLE_PTRS, SETUP_SCRATCH_ADDRESS, SETUP_SCRATCH_USED, VGA_BIOS_BASE,
    VGA_BIOS_INT1D_VIDEO_TABLE_ADDR, VGA_BIOS_INT1D_VIDEO_TABLE_OFF, VGA_BIOS_INT1F_FONT_ADDR,
    VGA_BIOS_INT1F_FONT_OFF, VGA_BIOS_INT44_FONT_ADDR, VGA_BIOS_INT44_FONT_OFF,
};
use firmware_contract::{Bios32Call, install_boot_memory, patch_rom};
pub use gameport::JoystickState;

pub use canonical_state::{CanonicalMachineStateCapture, MachineCanonicalCaptureError};
pub use katea_tree::KateaGeometryReport;
pub use katea_tree::KateaStorageCounters;
pub use storage::Int13Profile;
pub use vga_wipe_census::{VgaWipeCensusSnapshot, VgaWipeKeyRow};

pub use cdimage::CdImage;
pub use iso9660::{MAX_IMAGE_BYTES as CD_FOLDER_MAX_BYTES, build as build_cd_folder};
pub use memmap::{
    CONVENTIONAL_TOP, HMA_BASE, HMA_TOP, MemRegion, SYSTEM_ROM_BASE, UPPER_MEMORY_BASE,
    VIDEO_RAM_BASE, classify, is_hma, is_umb_window,
};

pub const HIGH_ROM_BASE: u32 = 0xffff_0000;
pub const MARGO_LFB_BASE: u32 = 0xE000_0000;
pub const MARGO_MMIO_BASE: u32 = 0xE040_0000;
pub const DISTIRA_MMIO_BASE: u32 = 0xE100_0000;
pub const DISTIRA_LFB_BASE: u32 = 0xE140_0000;

pub const LOW_BIOS_BASE: u32 = 0x000f_0000;
pub const BIOS_ROM_SIZE: usize = 64 * 1024;

pub const BOOT_IMAGE_SIZE: usize = 1440 * 1024;
pub const BOOT_SECTOR_ADDRESS: usize = 0x7c00;
pub const BOOT_STAGE2_ADDRESS: usize = 0x8000;
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

/// Process default for CPU execution in machines constructed afterwards.
///
/// The application selects this once before it creates worker threads. Library
/// users keep automatic native admission unless they explicitly opt into the
/// interpreter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ExecutionBackend {
    #[default]
    Automatic,
    Interpreter,
}

// Encoding: 0 Automatic, 1 Interpreter.
static PROCESS_EXECUTION_BACKEND: AtomicU8 = AtomicU8::new(0);

/// Set the execution backend inherited by subsequently constructed machines.
pub fn set_process_execution_backend(backend: ExecutionBackend) {
    let encoded = match backend {
        ExecutionBackend::Automatic => 0,
        ExecutionBackend::Interpreter => 1,
    };
    PROCESS_EXECUTION_BACKEND.store(encoded, Ordering::Release);
}

/// Return the execution backend currently inherited by new machines.
pub fn process_execution_backend() -> ExecutionBackend {
    match PROCESS_EXECUTION_BACKEND.load(Ordering::Acquire) {
        1 => ExecutionBackend::Interpreter,
        _ => ExecutionBackend::Automatic,
    }
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

/// Per-mode video-window wait states for the Approximate class (486/586).
/// A real VGA card sits across an expansion bus whose access latency does not
/// scale with CPU speed. The flat `WaitStateProfile.video = 1` rode `scale_bus`
/// (486 x1/3, 586 x7/30), which priced VRAM far below VLB and PCI latency. Doom
/// issues about 131 million VGA accesses in the max-detail demo3 timedemo, so it
/// exposes this error while the synthetic CPU benchmarks do not.
///
/// Narrow SMC invalidation removed an accidental cold-decode timing tax. The 586
/// value was recalibrated at that point, but the 486 value was left stale. The
/// current values are measured after that change and after `scale_bus`:
///
/// - 486 ws=45: 2980 realtics, 25.1 fps (target about 3000 realtics)
/// - 586 ws=75 at 200 MHz with `bus_timing` 7/30: 833 realtics, 89.7 fps
///   (target 820 to 850 realtics); re-seated 2026-08-08 for the 166 MHz /
///   PC100 spec as ws=147 with `bus_timing` 16/105, jointly solved so doom-586
///   holds ~1001 realtics (74.6 fps) while quake reaches ~41.2 fps.
///
/// Interpreter, direct-page, REP, and native VGA paths all use this table. The
/// Accurate 386 class keeps the frozen `WaitStateProfile.video` path. Recalibrate
/// these values if `bus_timing` changes.
const fn video_wait_states_approx(persona: CpuPersona) -> u8 {
    match persona {
        // Unreachable in practice because the Accurate class takes the profile path.
        CpuPersona::I386 => 1,
        // (2 + 45) * 1/3 clocks at 66 MHz is about 237 ns per access.
        CpuPersona::I486 => 45,
        // (2 + 147) * 16/105 clocks at 166 MHz is about 137 ns per access. The
        // count rises with the `bus_timing` cut so the VGA product lands where
        // the doom-586 anchor (~1001 realtics / 74.6 fps) needs it; see the
        // joint solve note on `bus_timing`.
        CpuPersona::I586 => 147,
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
    fn new(mode: GswMode) -> Self {
        Self {
            l1_tags: vec![CACHE_EMPTY_TAG; CACHE_L1_MAX_LINES].into_boxed_slice(),
            l2_tags: vec![CACHE_EMPTY_TAG; CACHE_L2_MAX_LINES].into_boxed_slice(),
            config: cache_level_config(mode),
            cost: tier_cost(mode),
            code_fetch_ws: code_fetch_ws(mode),
            lookups: 0,
        }
    }

    fn set_mode(&mut self, mode: GswMode) {
        self.config = cache_level_config(mode);
        self.cost = tier_cost(mode);
        self.code_fetch_ws = code_fetch_ws(mode);
        self.reset();
    }

    /// Resolve a DATA access at `phys` to a tier for the live `level`, installing the
    /// line into the cheaper tiers on a miss (modeling an inclusive fill). A 0-size
    /// tier is skipped: the 386 has no L1.
    ///
    #[cfg(test)]
    fn data_tier(&mut self, mode: GswMode, phys: u32) -> Tier {
        self.data_tier_with_config(cache_level_config(mode), phys)
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

const MACHINE_PROFILE_PHASES: usize = 7;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MachineProfilePhase {
    pub name: &'static str,
    pub wall_ns: u64,
    pub count: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MachineHostProfileSnapshot {
    pub machine_phase_timing_enabled: bool,
    pub phases: Vec<MachineProfilePhase>,
}

/// Phase-boundary ids recorded by the boot profiler. The host places `POST_END`
/// and `IDLE_END`; the guest places the rest by writing the id to the unit
/// tester's `REG_MARK` and issuing `CMD_MARK`.
pub mod phase_mark {
    /// The run's own start, placed by the profiler before the first instruction
    /// so the POST phase has a left edge like every other phase.
    pub const RUN_START: u8 = 255;
    /// POST finished and the BIOS reached INT 19h. Host-side: no guest code of
    /// ours runs inside the BIOS, and the first INT 19h is exact and free.
    pub const POST_END: u8 = 0;
    /// AUTOEXEC.BAT reached its end: Toka-DOS is up at the prompt.
    pub const BOOT_END: u8 = 1;
    /// The idle window elapsed. Host-side, because the phase being measured is
    /// COMMAND.COM's own prompt loop with no guest code of ours running in it.
    pub const IDLE_END: u8 = 2;
    /// LOADTEST.COM reached its entry point, so COMMAND.COM has finished
    /// parsing the command line and loading the image off Katea.
    pub const EXEC_END: u8 = 3;
    /// LOADTEST.COM finished reading its target file.
    pub const LOAD_END: u8 = 4;
    /// A periodic sample, placed from inside the run loop every N master ticks.
    ///
    /// DISTINCT from every id above, and it has to be: `has_mark` and `build_rows`
    /// (bootprofile.rs) both first-match on id, so a periodic mark sharing an id with a
    /// boundary would silently become that boundary and corrupt the boot profiler's phases.
    /// Never arm the interval in the boot profiler for the same reason.
    pub const PERIODIC: u8 = 200;
    /// The benchmark run's own edges, placed by the host either side of the single
    /// `run_until_halt_or_cycles` call so the first and last periodic intervals are closed.
    /// Host-placed, so no run boundary moves.
    pub const BENCH_START: u8 = 201;
    pub const BENCH_END: u8 = 202;
}

/// One phase boundary, with every counter the boot profiler attributes per
/// phase snapshotted at the instant it fired. The profiler reports differences
/// between consecutive marks, so POST cannot hide inside boot.
#[derive(Debug, Clone)]
pub struct PhaseMark {
    pub id: u8,
    pub wall: std::time::Instant,
    pub master_ticks: u64,
    pub elapsed_clocks: u64,
    pub perf: izarravm_cpu::PerfCounters,
    pub machine_phases: MachineHostProfileSnapshot,
    /// None when C: is not a mounted host folder.
    pub katea: Option<katea_tree::KateaStorageCounters>,
    /// Guest ticks granted for I/O stalls and for HLT, at this boundary.
    ///
    /// Both are needed to read the series honestly. `stall_for_master_ticks` grants guest time
    /// for ZERO emulation work while the host burns real wall inside Katea, so a loading phase
    /// looks fast in raw rt for an accounting reason rather than an emulation-rate one. Netting
    /// these out (with `katea.host_wall_ns`) is what makes two intervals comparable.
    pub io_stall_ticks: u64,
    pub halted_ticks: u64,
    /// The BIOS fixed-disk census at this boundary. All-zero unless
    /// `IZARRAVM_INT13_PROFILE` armed it. `Copy` and fixed-size, so unlike
    /// `cpu_profile` it costs the same at mark 1 and mark 1000 and cannot bias a
    /// late interval against an early one.
    pub int13: storage::Int13Profile,
    /// FastMap / direct-map whole-map wipe counters at this boundary. `Copy` and
    /// always-on in the CPU, so sampling it per mark costs a struct move.
    pub fast_map_audit: izarravm_cpu::FastMapAuditCounters,
    /// The sampled CPU census at this boundary, when `IZARRAVM_CPU_PROFILE`
    /// armed it; `None` otherwise, so an unprofiled run pays nothing and reports
    /// no empty tables. Differencing consecutive marks gives the per-phase
    /// census the whole-run snapshot cannot: read as "the idle loop", a
    /// whole-run census is an inference, and this makes it a measurement.
    pub cpu_profile: Option<izarravm_cpu::CpuProfileSnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MachineProfilePhaseKind {
    CpuBatch,
    AdvanceDevices,
    VideoConversion,
    AudioRender,
    SoftInt,
    ConsoleFlush,
    HaltFastForward,
}

impl MachineProfilePhaseKind {
    const ALL: [Self; MACHINE_PROFILE_PHASES] = [
        Self::CpuBatch,
        Self::AdvanceDevices,
        Self::VideoConversion,
        Self::AudioRender,
        Self::SoftInt,
        Self::ConsoleFlush,
        Self::HaltFastForward,
    ];

    const fn index(self) -> usize {
        match self {
            Self::CpuBatch => 0,
            Self::AdvanceDevices => 1,
            Self::VideoConversion => 2,
            Self::AudioRender => 3,
            Self::SoftInt => 4,
            Self::ConsoleFlush => 5,
            Self::HaltFastForward => 6,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::CpuBatch => "cpu_batch",
            Self::AdvanceDevices => "advance_devices",
            Self::VideoConversion => "video_conversion",
            Self::AudioRender => "audio_render",
            Self::SoftInt => "soft_int",
            Self::ConsoleFlush => "console_flush",
            Self::HaltFastForward => "halt_fast_forward",
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
    phases: std::cell::Cell<[MachineProfilePhaseState; MACHINE_PROFILE_PHASES]>,
}

impl Default for MachineHostProfile {
    fn default() -> Self {
        Self {
            enabled: false,
            phases: std::cell::Cell::new(
                [MachineProfilePhaseState::default(); MACHINE_PROFILE_PHASES],
            ),
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
        self.enabled = true;
        self.phases
            .set([MachineProfilePhaseState::default(); MACHINE_PROFILE_PHASES]);
    }

    fn disable(&mut self) {
        *self = Self::default();
    }

    #[inline]
    fn start(&self) -> Option<std::time::Instant> {
        self.enabled.then(std::time::Instant::now)
    }

    #[inline]
    fn record(&self, phase: MachineProfilePhaseKind, start: Option<std::time::Instant>) {
        let Some(start) = start else {
            return;
        };
        let mut phases = self.phases.get();
        let bucket = &mut phases[phase.index()];
        bucket.count += 1;
        bucket.wall_ns = bucket
            .wall_ns
            .saturating_add(duration_ns_u64(start.elapsed()));
        self.phases.set(phases);
    }

    fn snapshot(&self) -> MachineHostProfileSnapshot {
        MachineHostProfileSnapshot {
            machine_phase_timing_enabled: self.enabled,
            phases: MachineProfilePhaseKind::ALL
                .iter()
                .map(|&phase| {
                    let bucket = self.phases.get()[phase.index()];
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

/// Host-side tally of guest OPL (AdLib/OPL3) activity. Diagnostic ONLY: nothing
/// here is read by an emulation decision, nothing here is canonical state, and
/// the counters live on the bus rather than on `OplChip` because that chip
/// derives `PartialEq`/`Eq` for state comparison.
///
/// It exists to answer one question that no existing counter can: when music is
/// silent, is the guest failing to STRIKE notes, or striking them and losing
/// them downstream? `key_on_writes` splits those two cases directly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OplDiagnostics {
    /// Writes to a data port (base+1 / base+3), whatever the register.
    pub register_writes: u64,
    /// Status-byte reads (base+0 / base+2).
    pub status_reads: u64,
    /// Status reads that returned a SET timer-overflow flag (bit 6 or bit 5).
    /// A music driver paced by OPL timers advances its score on exactly these,
    /// so a run with `status_reads` high and this at zero is a driver polling a
    /// timer that never fires.
    pub status_reads_timer_expired: u64,
    /// Writes to primary register 0x04, the timer control/reset register.
    pub timer_control_writes: u64,
    /// Writes to 0xB0-0xB8 (either bank) with bit 5 SET: a voice being keyed on,
    /// which is the closest thing to "a note was played" the chip has.
    pub key_on_writes: u64,
    /// The same registers with bit 5 CLEAR: a voice released.
    pub key_off_writes: u64,
}

/// Host-side tally of guest Sound Blaster DSP activity. Diagnostic ONLY, and on
/// the bus rather than on `SbDsp` for the same reason as `OplDiagnostics`: that
/// chip derives `PartialEq` for state comparison.
///
/// `reset_acknowledges` is the one that matters. An SB detect writes 1 then 0 to
/// the reset port, waits ~100 us, and expects to read 0xAA back from the data
/// port. A run with resets but no acknowledges is a card the guest never found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SbDspDiagnostics {
    /// Writes of 0 to 0x226, each starting a reset settle.
    pub resets: u64,
    /// Bytes written to the command port 0x22C, arguments included.
    pub command_bytes: u64,
    /// Reads of the data port 0x22A.
    pub data_reads: u64,
    /// Data-port reads that returned 0xAA, the reset acknowledge.
    pub reset_acknowledges: u64,
    /// Reads of the read-buffer status ports 0x22E/0x22F.
    pub status_reads: u64,
}

/// One recorded guest OPL access, for `IZARRAVM_OPL_TRACE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OplTraceEntry {
    pub write: bool,
    /// The port the GUEST used, before Sound Blaster alias resolution, so a
    /// trace shows whether the game drove 0x388 or the SB mirror at 0x220.
    pub port: u16,
    /// Register bank, 0 or 1.
    pub bank: u8,
    /// Destination register for a data write, or `NO_REGISTER` for an address
    /// latch write and for a status read.
    pub register: u16,
    /// Byte written, or the status byte returned on a read.
    pub value: u8,
    /// CPU core clocks elapsed when the access happened, which is what makes
    /// the 80 us OPL timer window legible in the trace.
    pub core_clocks: u64,
    /// For a status read in the Approximate class: the microseconds of
    /// un-applied device time the prediction was taken at. Zero elsewhere.
    /// This is the number that says whether a read could POSSIBLY have seen an
    /// 80 us timer expire.
    pub pending_micros: u64,
}

impl OplTraceEntry {
    /// `register` value meaning "this access did not address a register".
    pub const NO_REGISTER: u16 = 0x100;
}

/// Counters plus an optional capped access trace. `IZARRAVM_OPL_TRACE=<n>`
/// records the first `n` accesses; unset records none and costs one `is_empty`
/// style capacity check per access.
///
/// The trace exists because the counters can say detection FAILED but not WHY:
/// AdLib detect turns on whether a status read returns clear before the timer
/// window and set after it, and only the ordered sequence of writes, reads and
/// elapsed clocks shows which half went wrong.
#[derive(Debug, Default)]
pub struct OplProbe {
    counters: OplDiagnostics,
    sb: SbDspDiagnostics,
    trace: Vec<OplTraceEntry>,
    cap: usize,
}

impl OplProbe {
    fn from_env() -> Self {
        let cap = std::env::var("IZARRAVM_OPL_TRACE")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(0);
        Self {
            counters: OplDiagnostics::default(),
            sb: SbDspDiagnostics::default(),
            trace: Vec::new(),
            cap,
        }
    }

    pub fn counters(&self) -> OplDiagnostics {
        self.counters
    }

    pub fn sb(&self) -> SbDspDiagnostics {
        self.sb
    }

    /// Record a Sound Blaster DSP port access. `value` is the byte written, or
    /// the byte the read returned.
    fn record_sb(&mut self, port: u16, write: bool, value: u8) {
        match (port, write) {
            // Only the write of 0 starts the settle; the preceding 1 arms it.
            (0x0226, true) if value == 0 => self.sb.resets += 1,
            (0x022c, true) => self.sb.command_bytes += 1,
            (0x022a, false) => {
                self.sb.data_reads += 1;
                if value == 0xaa {
                    self.sb.reset_acknowledges += 1;
                }
            }
            (0x022e | 0x022f, false) => self.sb.status_reads += 1,
            _ => {}
        }
    }

    pub fn trace(&self) -> &[OplTraceEntry] {
        &self.trace
    }

    fn push(&mut self, entry: OplTraceEntry) {
        if self.trace.len() < self.cap {
            self.trace.push(entry);
        }
    }

    /// Record a status read returning `value`.
    fn record_read(&mut self, port: u16, value: u8, core_clocks: u64, pending_micros: u64) {
        self.counters.status_reads += 1;
        // Bits 6 and 5 are the timer-1 and timer-2 overflow flags.
        if value & 0x60 != 0 {
            self.counters.status_reads_timer_expired += 1;
        }
        self.push(OplTraceEntry {
            write: false,
            port,
            bank: 0,
            register: OplTraceEntry::NO_REGISTER,
            value,
            core_clocks,
            pending_micros,
        });
    }

    /// Record a write of `value`. `register` is `None` for an address latch.
    fn record_write(
        &mut self,
        port: u16,
        bank: u8,
        register: Option<u8>,
        value: u8,
        core_clocks: u64,
    ) {
        if let Some(index) = register {
            self.counters.register_writes += 1;
            if bank == 0 && index == 0x04 {
                self.counters.timer_control_writes += 1;
            }
            if (0xb0..=0xb8).contains(&index) {
                if value & 0x20 != 0 {
                    self.counters.key_on_writes += 1;
                } else {
                    self.counters.key_off_writes += 1;
                }
            }
        }
        self.push(OplTraceEntry {
            write: true,
            port,
            bank,
            register: register.map_or(OplTraceEntry::NO_REGISTER, u16::from),
            value,
            core_clocks,
            pending_micros: 0,
        });
    }
}

/// One entry in the fatal-fault report latch: a raise site plus the error seen
/// there. The sentinel (no site, empty error) is pushed once at the cap to mark
/// that the suppression notice has been printed; it cannot collide with a real
/// entry, because a real one always carries an error string.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedFault {
    site: Option<(u16, u32)>,
    error: String,
}

impl ReportedFault {
    fn sentinel() -> Self {
        Self {
            site: None,
            error: String::new(),
        }
    }
}

#[derive(Debug)]
pub struct Machine {
    profile: MachineProfile,
    active_mode: GswMode,
    pending_mode: Option<GswMode>,
    timeline: Timeline,
    // Monotonic fixed-time duration advanced while HLT had parked the CPU. The
    // GUI uses this to distinguish an idle guest from a slow active CPU.
    halted_ticks: u64,
    // Fatal-fault reporting state. `reported_fault_sites` is the latch: a fatal
    // error leaves the machine resumable and the GUI resumes it, so a
    // re-faulting loop would otherwise print thousands of identical lines a
    // second. `last_fault_line` is what makes the reporting testable, since the
    // line itself goes to stderr.
    reported_fault_sites: Vec<ReportedFault>,
    last_fault_line: Option<String>,
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
    // Cumulative scaled bus clocks committed by successful CPU batches. This is
    // proof timing, separate from the mode-local fractional carry above.
    scaled_bus_clocks: u64,
    #[cfg(feature = "jit")]
    poll_skip_enabled: bool,
    #[cfg(feature = "jit")]
    poll_skip_diagnostics: run::PollSkipDiagnostics,
    memory: Memory,
    ram_lookup: RamPageLookup,
    vega: Vega,
    paradise_non_vga: bool,
    paradise_regs: [u8; 6],
    pci: PciConfig,
    text_scanline_override: Option<u16>,
    pending_soft_int: Option<u8>, // software-INT vector awaiting deferred dispatch
    pending_bios32: Option<Bios32Call>,
    // The vector of the last host-intercepted `INT n` opcode, stashed so the
    // legacy shared FF00:0000 chain target can attribute a landing there (that
    // address is shared by every vector, so the fetch seam cannot key it the
    // way it keys the per-vector stub table). Consumed by `note_stub_fetch`.
    last_int_vector: Option<u8>,
    // Set by MachineBus on any port I/O; the run loop's instruction batch reads
    // it to know when to stop and service devices (see run_until_tick). A field
    // rather than a loop local so make_bus's one-off host accesses share it.
    io_touched: bool,
    // Set by MachineBus on a port access that was EXEMPTED from `io_touched` (the
    // TOKAEMM ring-0-monitor carve-out in read_io/write_io). Such an access still
    // pokes real devices, so it can move a device schedule without ending the
    // batch -- which is exactly the case the device-edge deadline cache must not
    // miss. Reset per batch alongside `io_touched`.
    exempt_io_touched: bool,
    // Fixed ISA-bus time (in CPU clocks) accrued this batch by the OPL status poll,
    // added to the batch's device advance in the fast modes so a fast CPU
    // poll cannot outrun the 80 us OPL timer. See the batch-end use in
    // run_until_tick and the accrual in read_io. Consumed (zeroed) each batch via
    // mem::take.
    isa_io_batch_clocks: u64,
    // Master-timeline instant until which a PIT counter observer is assumed live,
    // set by any access to the counter data ports or the control port and read by
    // `fine_batch_grain_required`. Counter VALUES no longer depend on it:
    // `Counter::count_after` peeks the counting element at the in-batch instant of
    // the access, so a latch is exact at any batch grain. The window remains for the
    // one case that peek declines -- a BCD-programmed counter, which falls back to
    // the live (batch-start) field. Host scheduling only: never guest-visible state,
    // never canonical.
    pit_observer_fine_until: u64,
    // Maintained next-device-edge deadline for the batch cap (86Box-style push
    // model, see `Machine::event_batch_cap_cached`). Host scheduling only: it can
    // only ever shorten a batch, it is never guest-visible, and it is never part
    // of canonical state -- a restored machine simply re-scans.
    device_edge_cache: timing::DeviceEdgeCache,
    // Batch entries and pull-scans since power-on, for the deadline cache's own
    // hit-rate readout. Never an emulation input, and maintained only while
    // `host_profile.enabled` is set, so the batch path pays nothing for them on a
    // normal run. See `Machine::event_batch_cap_cached`.
    device_edge_batches: u64,
    device_edge_scans: u64,
    // Diagnostic-only OPL counters plus an optional access trace; see `OplProbe`.
    // Never read by an emulation decision and never part of canonical state, so
    // unlike `isa_io_batch_clocks` above it does not gate a canonical capture.
    opl_probe: OplProbe,
    // Set only when a bus-side DMA block copy writes guest RAM without exposing
    // its destination range. Range-aware HLE and device paths notify the CPU directly.
    device_wrote_memory: bool,
    // An exact RAM write performed while MachineBus owns the memory borrow. The CPU cannot be
    // notified until that borrow ends, so the run loop consumes this before another CPU entry.
    pending_device_memory_write_range: Option<(u32, u32)>,
    // Set when the RAM direct-map table changes, so cached host pointers in the CPU are dropped
    // before any later guest access can use a stale RAM page classification.
    direct_map_changed: bool,
    // Set when only a device data aperture changes. The CPU drops data pointers
    // and its FastMap while retaining decoded and compiled code.
    direct_data_map_changed: bool,
    // Generation stamped onto every direct host pointer. Any change that can
    // replace or reinterpret a mapping advances it before another CPU batch.
    direct_mapping_epoch: u64,
    // Env-gated attribution for the VGA direct-write-token wipe seam. Diagnostic only; its
    // PartialEq is unconditionally true so arming it cannot move canonical-state comparisons.
    vga_wipe_census: vga_wipe_census::VgaWipeCensus,
    host_profile: MachineHostProfile,
    // Boot-profiler phase boundaries, off unless `enable_phase_marks` armed them.
    // Diagnostic only, and recorded at most a handful of times per run (one INT
    // 19h, four guest marks), so no hot path pays for the check.
    phase_marks: Vec<PhaseMark>,
    phase_marks_enabled: bool,
    /// Master-tick deadline for the next periodic sample, or `u64::MAX` when disarmed.
    ///
    /// A SENTINEL rather than a separate enable flag, so the run loop's disabled path is one
    /// compare against a value it has already loaded. Gating at the call site rather than inside
    /// the callee is the project rule for default-off instruments, and it matters here because a
    /// non-inlined call would also block optimisation of the surrounding loop.
    next_phase_mark_ticks: u64,
    periodic_phase_mark_interval: u64,
    // Whether the POST->boot boundary has already been placed. A guest that
    // re-enters INT 19h must not overwrite the first, real POST measurement.
    post_phase_marked: bool,
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
    /// `handle_raw_program_int`.
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
    // Unclaimed-port accounting; see bus::OpenBusPorts. Carries the whole run's
    // port set, so it lives on the machine rather than the per-batch bus.
    open_bus: bus::OpenBusPorts,
    pic: pic::Pic8259Pair,
    pit: pit::Pit,
    keyboard: keyboard::Keyboard8042,
    gameport: gameport::GamePort,
    speaker: speaker::Speaker,
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
    sb16: Sb16Path,
    last_audio_ticks: u64,
    wavetable_mpu: Mpu401,
    midi_mpu: Mpu401,
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
    wss_render_phase: RatePhase,
    /// Resampled DAC-rate WSS frames produced but not yet claimed by a render
    /// window, carried across calls for the same reason the SB16 voice carries
    /// its own (see `sb16_path::Sb16Path::render_voice`): the codec's input
    /// count comes from bursty guest ticks while the window size comes from the
    /// host-paced OPL resampler, so surplus must queue rather than be dropped.
    wss_pending: VecDeque<(i32, i32)>,
    /// Last DAC-rate frame delivered by the WSS stream.
    wss_hold: (i32, i32),
    wss_base: u16, // I/O base of the 4-port config region (codec sits at base+4)
    // NB: the codec's IRQ line and DMA channel are deliberately NOT cached
    // here. They live in the Ad1848 itself and are read at the point of use
    // (`self.wss.irq()` / `self.wss.dma()`), because the WSS config register is
    // writable: a cached copy would leave the codec answering on the line it
    // just gave up. The SB16 path takes the same approach via the mixer.
    wss_enabled: bool, // false drops all WSS work (port decode, tick, IRQ, render)
    trace: BusTrace,
    // CPU-domain work accounting kept for compatibility counters and benchmarks.
    // Global device time lives in `timeline` and remains meaningful across mode changes.
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
    // BIOS fixed-disk census, and the bool that arms it. Both live here rather
    // than in `ata` because the census counts INT 13h CALLS, which the drive
    // never sees: it is handed sectors.
    int13_profile: storage::Int13Profile,
    int13_profile_enabled: bool,
    // ATAPI CD-ROM on the secondary IDE channel (0x170-0x177/0x376, IRQ15). It
    // owns the mounted disc image, the ATA register file, and the CD-audio
    // playback state the mixer streams.
    ide: ide::IdeChannel,
    // Parsed x86 El Torito initial/default entry for the mounted CD and the
    // optional drive-emulation state established when that entry boots.
    eltorito_boot: Option<storage::ElToritoBoot>,
    eltorito_emulation: Option<storage::ElToritoEmulation>,
    // MSCDEX/IZCDEX volume-descriptor preference. The default selects the primary
    // volume descriptor.
    icdex_vd_preference: u16,
    // ATA hard disk on the primary IDE channel (0x1F0-0x1F7/0x3F6, IRQ14). The
    // boot drive C:; None when no image is mounted. INT 13h DL>=0x80 and the
    // primary-channel ports drive it.
    ata: Option<ata::AtaDisk>,
    // PIIX4-compatible two-channel bus-master IDE register block. The primary
    // channel transfers the ATA disk; the secondary bank records legacy IDE
    // interrupts but does not advertise ATAPI DMA.
    bmide: bmide::BusMasterIde,
    // Synthesized read-only FAT32 volume serving drive C: to the DOS absolute-disk
    // interface (INT 25h read; INT 26h write is write-protected). Optional and
    // consulted only by INT 25h/26h for AL=2, so it does not touch the ATA / INT
    // 13h path. None until one is mounted. The eventual single C: backing (ATA
    // vs this) remains an install-layout decision.
    fat32_c: Option<Fat32Volume>,
    cd_accesses: u64,
    // Bytes transferred through the secondary IDE data phases. Unlike
    // cd_accesses, this excludes the legacy INT 2Fh compatibility path and lets
    // guest-stack tests prove TOKACD reached the real ATAPI transport.
    cd_pio_bytes: u64,
    // Fractional Red Book frames owed to the CD-audio mixer from the DAC clock.
    cd_audio_sample: usize,
    // Playback generation observed by the audio mixer. Only a new range, stop,
    // mount, or eject changes it; pause/resume keeps the intra-frame cursor.
    cd_audio_epoch: u64,
    // MC146818 RTC and CMOS NVRAM at ports 0x70/0x71.
    rtc: rtc::Rtc,
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
    // updates. One inner Vec per batch
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

/// Derive the DOS environment entries that advertise the Sound Blaster to
/// auto-detecting games. `BLASTER` and `SETSOUND` carry the same value:
/// `A220` (the fixed Resonique 2 base), `I`/`D`/`H` from the host config, `T6`
/// (the SB16 card type), and `P` for whichever MPU-401 port is advertised
/// (`0x300` wavetable header or `0x330` rear connector -- both stay decoded
/// either way). Returns an empty list when the card is disabled, so no `BLASTER`
/// leaks into a machine that has no SB16; the value always matches the routing
/// the CT1745 mixer answers, since both are derived from the same config.
fn sound_blaster_env_entries(config: &SoundBlasterConfig, mpu_port: u16) -> Vec<(String, String)> {
    if !config.enabled {
        return Vec::new();
    }
    let value = format!(
        "A220 I{} D{} H{} P{mpu_port:03X} T6",
        config.irq.line(),
        config.dma.channel(),
        config.high_dma.channel(),
    );
    vec![
        ("BLASTER".to_string(), value.clone()),
        ("SETSOUND".to_string(), value),
    ]
}

/// Files that are NOT overlaid in user-folder mode: the demo file and the two
/// config files the user owns on C:.
const USER_OWNED_OR_DEMO: &[&str] = &["HELLO.TXT", "CONFIG.SYS", "AUTOEXEC.BAT"];

/// The payload files overlaid in user-folder mode: the kernel, shell, licenses,
/// command-line tools, and drivers, but not the demo file or the user's
/// `CONFIG.SYS`/`AUTOEXEC.BAT`.
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

/// Default `(CONFIG.SYS, AUTOEXEC.BAT)` bytes from the committed image payload.
fn default_config_pair(sound_blaster: &SoundBlasterConfig, mpu_port: u16) -> (Vec<u8>, Vec<u8>) {
    let payload = katea_volume::extract_system_payload(izarravm_firmware::tokados_hdd_img());
    (
        payload_file(&payload, "CONFIG.SYS"),
        storage::stock_autoexec(
            &payload_file(&payload, "AUTOEXEC.BAT"),
            sound_blaster,
            mpu_port,
        ),
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
    fn base(
        profile: MachineProfile,
        mut cpu: CpuGsw,
        mut rom: Vec<u8>,
    ) -> Result<Self, MachineError> {
        let sb16 = Sb16Path::new(&profile.sound_blaster);
        // Build the AD1848 codec from the WSS board config. The codec's IRQ/DMA
        // jumper readback comes from the same WssConfig the env/detection use, so
        // the config region answers exactly what the codec is wired to. The base
        // and resource numbers are cached on the bus for the port decode and the
        // advance_devices DMA/IRQ feed (kept separate from the SB16's mixer).
        let wss_enabled = profile.wss.enabled;
        let wss_base = profile.wss.base;
        let wss = Ad1848::new(Ad1848Config {
            irq: profile.wss.irq.line(),
            dma: profile.wss.dma.channel() as u8,
        });
        let active_mode = profile.cpu;
        cpu.set_mode(active_mode);
        let vega = Vega::default();
        let pci = PciConfig::new();
        let memory = Memory::from_mib(profile.memory_mib)?;
        let ram_lookup = RamPageLookup::new(memory.len(), &vega);
        let execution_backend = process_execution_backend();
        patch_rom(&mut rom);
        let mut machine = Self {
            memory,
            ram_lookup,
            profile,
            active_mode,
            pending_mode: None,
            timeline: Timeline::new(active_mode),
            halted_ticks: 0,
            reported_fault_sites: Vec::new(),
            last_fault_line: None,
            cpu,
            cache_model: CacheModel::new(active_mode),
            bus_rem: 0,
            scaled_bus_clocks: 0,
            #[cfg(feature = "jit")]
            poll_skip_enabled: run::poll_skip_default(execution_backend),
            #[cfg(feature = "jit")]
            poll_skip_diagnostics: run::PollSkipDiagnostics::new(execution_backend),
            vega,
            paradise_non_vga: false,
            paradise_regs: [0; 6],
            pci,
            text_scanline_override: None,
            pending_soft_int: None,
            pending_bios32: None,
            last_int_vector: None,
            io_touched: false,
            exempt_io_touched: false,
            isa_io_batch_clocks: 0,
            pit_observer_fine_until: 0,
            device_edge_cache: timing::DeviceEdgeCache::Stale,
            device_edge_batches: 0,
            device_edge_scans: 0,
            opl_probe: OplProbe::from_env(),
            device_wrote_memory: false,
            pending_device_memory_write_range: None,
            direct_map_changed: false,
            direct_data_map_changed: false,
            vga_wipe_census: vga_wipe_census::VgaWipeCensus::default(),
            direct_mapping_epoch: 1,
            host_profile: MachineHostProfile::default(),
            phase_marks: Vec::new(),
            phase_marks_enabled: false,
            next_phase_mark_ticks: u64::MAX,
            periodic_phase_mark_interval: 0,
            post_phase_marked: false,
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
            open_bus: bus::OpenBusPorts::from_env(),
            pic: pic::Pic8259Pair::default(),
            pit: pit::Pit::default(),
            keyboard: keyboard::Keyboard8042::default(),
            gameport: gameport::GamePort::default(),
            speaker: speaker::Speaker::default(),
            speaker_transitions: Vec::new(),
            dma: dma::DmaController::default(),
            fdc: fdc::Fdc::default(),
            opl: OplChip::default(),
            resampler: Resampler::new(OPL_NATIVE_HZ, DAC_HZ),
            card_amp: 1.0,
            speaker_volume: 1.0,
            sb16,
            last_audio_ticks: 0,
            wavetable_mpu: Mpu401::default(),
            midi_mpu: Mpu401::default(),
            wss,
            // Placeholder; sync_wss_resampler rebuilds this for the live rate on
            // first use, so the value here never reaches the DAC as-is.
            wss_resampler: Resampler::new(OPL_NATIVE_HZ, DAC_HZ),
            wss_rate_hz: 0,
            wss_render_phase: RatePhase::default(),
            wss_pending: VecDeque::new(),
            wss_hold: (0, 0),
            wss_base,
            wss_enabled,
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
            int13_profile: storage::Int13Profile::default(),
            int13_profile_enabled: false,
            ide: ide::IdeChannel::new(),
            eltorito_boot: None,
            eltorito_emulation: None,
            icdex_vd_preference: 0x0100,
            ata: None,
            bmide: bmide::BusMasterIde::default(),
            fat32_c: None,
            cd_accesses: 0,
            cd_pio_bytes: 0,
            cd_audio_sample: 0,
            cd_audio_epoch: 0,
            rtc: rtc::Rtc::new(),
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
        #[cfg(feature = "jit")]
        machine
            .cpu
            .set_native_backend_enabled(matches!(execution_backend, ExecutionBackend::Automatic));
        machine.set_jit_auto_admit(run::jit_auto_admit_default(execution_backend));
        machine.set_cmos_byte(CMOS_PRIMARY_BOOT_DEVICE, BootDevice::Floppy as u8);
        // Seed NVRAM 0x12 (the GSW code the BIOS applies at POST) from the boot
        // profile so a fresh CMOS reproduces the profile's speed; a loaded
        // cmos.bin then overwrites it with the user's saved choice.
        machine.set_cmos_byte(CMOS_GSW_MODE, machine.active_mode.register_code());
        machine.rtc.set_memory_size(machine.profile.memory_mib);
        machine.seed_audio_cmos();
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
        install_boot_memory(&mut machine.memory, machine.active_mode)?;
        Ok(machine)
    }

    /// Control the cosmetic POST pacing the BIOS reads at port 0xE2. The default
    /// is fast (true): the ROM skips the ~8 s RAM count-up and the chime so
    /// headless runs and tests stay inside their cycle budgets. Pass false from
    /// the GUI to keep the full power-on screen and timing.
    pub fn set_fast_post(&mut self, fast: bool) {
        self.fast_post = fast;
    }

    pub fn drain_distira_fifo(&mut self) {
        self.vega.drain_distira_fifo();
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
        // Reseeding moves the update-ended / alarm instant, a cached cap term.
        self.invalidate_device_edge_cache();
    }

    /// The full 64-byte CMOS image (clock registers plus NVRAM) for persisting
    /// to cmos.bin.
    pub fn cmos_bytes(&self) -> [u8; 64] {
        self.rtc.nvram()
    }

    /// Write the machine profile's audio resource assignment into the CMOS block
    /// SNDCTRL.COM owns, and stamp the magic byte. Called at construction so a
    /// machine that has never run the tool still presents a complete, valid
    /// block rather than zeros.
    fn seed_audio_cmos(&mut self) {
        let sb = self.profile.sound_blaster;
        let wss = self.profile.wss;
        self.rtc.set_nvram(CMOS_AUDIO_MAGIC, CMOS_AUDIO_MAGIC_VALUE);
        self.rtc.set_nvram(CMOS_SB_IRQ, sb.irq.line());
        self.rtc.set_nvram(CMOS_SB_DMA8, sb.dma.channel() as u8);
        self.rtc
            .set_nvram(CMOS_SB_DMA16, sb.high_dma.channel() as u8);
        self.rtc.set_nvram(CMOS_WSS_IRQ, wss.irq.line());
        self.rtc.set_nvram(CMOS_WSS_DMA, wss.dma.channel() as u8);
        self.rtc.set_nvram(CMOS_MPU_PORT, 0);
        self.rtc.refresh_checksum();
    }

    /// Read the CMOS audio block back into typed config, or `None` if it is not
    /// a block this card could have written: no magic byte, a line or channel
    /// the hardware cannot route to, or an IRQ/DMA collision between the two
    /// devices. Nothing stops a guest from writing arbitrary bytes there, and
    /// the whole block is one setting, so a single bad byte rejects all of it
    /// rather than leaving half the card on stale routing.
    fn read_audio_cmos(&self) -> Option<(SoundBlasterConfig, WssConfig)> {
        if self.rtc.nvram_byte(CMOS_AUDIO_MAGIC) != CMOS_AUDIO_MAGIC_VALUE {
            return None;
        }
        let sb = SoundBlasterConfig {
            irq: izarravm_core::SbIrq::from_line(self.rtc.nvram_byte(CMOS_SB_IRQ))?,
            dma: izarravm_core::SbDma8::from_channel(usize::from(
                self.rtc.nvram_byte(CMOS_SB_DMA8),
            ))?,
            high_dma: izarravm_core::SbDma16::from_channel(usize::from(
                self.rtc.nvram_byte(CMOS_SB_DMA16),
            ))?,
            ..self.profile.sound_blaster
        };
        let wss = WssConfig {
            irq: izarravm_core::WssIrq::from_line(self.rtc.nvram_byte(CMOS_WSS_IRQ))?,
            dma: izarravm_core::SbDma8::from_channel(usize::from(
                self.rtc.nvram_byte(CMOS_WSS_DMA),
            ))?,
            ..self.profile.wss
        };
        // The same two collisions izarravm.conf rejects (ConfigError::WssSb*
        // Collision): the AD1848 and the SB16 cannot share a PIC line or a DMA
        // channel, and only matter when both devices are actually built.
        if sb.enabled && wss.enabled {
            if sb.irq.line() == wss.irq.line() {
                return None;
            }
            if sb.dma.channel() == wss.dma.channel() {
                return None;
            }
        }
        Some((sb, wss))
    }

    /// Apply the CMOS audio block to the live devices AND to the machine
    /// profile. The mixer and codec are built from the profile at construction,
    /// before any persisted NVRAM has been read, so whatever SNDCTRL.COM last
    /// saved has to be re-applied here or it would only take effect on the boot
    /// after next.
    ///
    /// The profile is updated too, not just the devices, because it is what
    /// every *description* of the card is derived from -- the `BLASTER` line
    /// `stock_autoexec` writes into an emulator-owned AUTOEXEC.BAT, and the
    /// environment `dos.rs` injects on the HLE path. Leaving it on the old
    /// value is how the card ends up answering on one IRQ while `BLASTER`
    /// advertises another.
    ///
    /// A block this card could not have written is treated as never configured
    /// and reseeded from the profile, so neither a CMOS image predating the tool
    /// nor a guest poking NVRAM directly can route the card somewhere it cannot
    /// answer.
    fn apply_audio_cmos(&mut self) {
        let Some((sb, wss)) = self.read_audio_cmos() else {
            self.seed_audio_cmos();
            return;
        };
        self.profile.sound_blaster = sb;
        self.profile.wss = wss;
        self.sb16
            .set_routing(sb.irq.line(), sb.dma.channel(), sb.high_dma.channel());
        self.wss.set_config(izarravm_audio::Ad1848Config {
            irq: wss.irq.line(),
            dma: wss.dma.channel() as u8,
        });
    }

    /// The IRQ line and the 8-bit/16-bit DMA channels the Sound Blaster mixer
    /// currently answers on, or `None` when the card is not built. Live state,
    /// not the profile: a guest write to mixer register `0x80`/`0x81` (which is
    /// how `SNDCTRL.COM` moves the card) is reflected here immediately.
    pub fn sound_blaster_routing(&self) -> Option<(u8, usize, usize)> {
        self.sb16.routing()
    }

    /// One CT1745 mixer register as the guest would read it back, or `None`
    /// when the card is not built. Live state with no side effect: unlike a
    /// guest read it does not disturb the latched index, so a host can look at
    /// what a setup tool programmed without becoming a second writer. This is
    /// the mixer's counterpart to [`Machine::cmos_bytes`], and it is what lets
    /// a test check that SNDMIXER.COM moved a level rather than only that it
    /// printed one.
    pub fn sb_mixer_register(&self, index: u8) -> Option<u8> {
        self.sb16.peek_mixer_register(index)
    }

    /// One AD1848 indexed register, or `None` when the codec is not built.
    /// Same contract as [`Machine::sb_mixer_register`]: the codec's own index
    /// latch is left alone.
    pub fn wss_register(&self, index: u8) -> Option<u8> {
        self.wss_enabled
            .then(|| self.wss.peek_register(usize::from(index)))
    }

    /// The IRQ line and DMA channel the AD1848 codec currently answers on, or
    /// `None` when it is not built. Live, for the same reason as
    /// [`Machine::sound_blaster_routing`].
    pub fn wss_routing(&self) -> Option<(u8, usize)> {
        self.wss_enabled.then(|| (self.wss.irq(), self.wss.dma()))
    }

    /// The MPU-401 port SNDCTRL.COM selected: `0x300` (wavetable header) or
    /// `0x330` (rear connector). Both ports stay decoded either way; this is
    /// the one advertised in `BLASTER`.
    pub fn cmos_mpu_port(&self) -> u16 {
        if self.rtc.nvram_byte(CMOS_MPU_PORT) == 0 {
            0x300
        } else {
            0x330
        }
    }

    /// Load a 64-byte CMOS image from a persisted cmos.bin. Returns false if its
    /// NVRAM checksum is bad; constructor-seeded defaults are retained and
    /// checksummed so the host can persist a safe replacement.
    pub fn load_cmos(&mut self, bytes: &[u8; 64]) -> bool {
        let valid = self.rtc.load_nvram(bytes);
        // Register B/A come back with the image, so the periodic-IRQ rate and the
        // update/alarm enables can both change under us.
        self.invalidate_device_edge_cache();
        // Installed RAM is a property of THIS machine, not of the saved image: a
        // real BIOS rewrites the memory-size bytes at POST every boot. Re-apply
        // them so a cmos.bin carried over from a different --memory-mib (or
        // written before those bytes were populated at all) cannot make the
        // guest see the wrong amount of extended memory.
        self.rtc.set_memory_size(self.profile.memory_mib);
        self.apply_audio_cmos();
        if let Some(mode) = GswMode::from_register_code(self.rtc.nvram_byte(0x12)) {
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
        self.invalidate_device_edge_cache();
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
        install_boot_memory(&mut machine.memory, machine.active_mode)?;

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

        // The fixture enters with DL=80h and its boot sector loads stage 2 through
        // INT 13h. Back that request with the supplied image instead of relying on
        // an absent fixed disk to inherit a pre-cleared carry flag. Stage 2 remains
        // preloaded above as a useful assertion aid, but the guest now performs a
        // real BIOS transfer over the same bytes.
        machine.mount_hdd(image.to_vec());
        Ok(machine)
    }

    pub fn profile(&self) -> &MachineProfile {
        &self.profile
    }

    pub fn cpu(&self) -> &CpuGsw {
        &self.cpu
    }

    /// Set the JIT hotness auto-admission policy. This is a no-op without feature `jit`, and the CPU
    /// keeps it disabled on unsupported hosts.
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

    /// Measure whole-machine batch phases without enabling the per-instruction CPU sampler.
    /// This keeps native block execution enabled while attributing host time around the VM's
    /// existing subsystem boundaries.
    pub fn enable_machine_profiling(&mut self) {
        self.host_profile.enable();
        self.cpu.disable_profiling();
    }

    pub fn disable_host_profiling(&mut self) {
        self.host_profile.disable();
        self.cpu.disable_profiling();
    }

    pub fn host_profile_snapshot(&self) -> MachineHostProfileSnapshot {
        self.host_profile.snapshot()
    }

    /// What the Katea host-folder read path has done since mount, or None when
    /// C: is not a mounted host folder.
    pub fn katea_storage_counters(&self) -> Option<katea_tree::KateaStorageCounters> {
        self.ata.as_ref().and_then(|d| d.katea_storage_counters())
    }

    /// Arm boot-profiler phase-boundary recording. Off by default so an ordinary
    /// run never allocates a mark or checks for one outside INT 19h and the unit
    /// tester's deferred-command path, both of which are already cold.
    pub fn enable_phase_marks(&mut self) {
        self.phase_marks_enabled = true;
        self.phase_marks.clear();
        self.post_phase_marked = false;
    }

    /// The synthesized Katea FAT32 geometry for C:, or None when C: is not a
    /// mounted host folder.
    pub fn katea_geometry_report(&self) -> Option<katea_tree::KateaGeometryReport> {
        self.ata.as_ref().and_then(|d| d.katea_geometry_report())
    }

    /// Host-side sector-cache hits and misses on the fixed disk since mount, or
    /// None with no disk. Always counted (two `u64` adds on a path that already
    /// synthesizes a sector), because without them a fallen `io_stall_ticks`
    /// cannot be attributed to the cache rather than to fewer reads.
    pub fn hdd_sector_cache_counters(&self) -> Option<(u64, u64)> {
        self.ata
            .as_ref()
            .map(|d| (d.sector_cache_hits(), d.sector_cache_misses()))
    }

    /// Arm the BIOS fixed-disk census. Off by default; the host CLI arms it from
    /// `IZARRAVM_INT13_PROFILE`, and every increment is behind this bool at its
    /// own call site rather than inside the helper it calls.
    pub fn enable_int13_profile(&mut self) {
        self.int13_profile_enabled = true;
    }

    /// The boundaries recorded so far, in the order they fired.
    pub fn phase_marks(&self) -> &[PhaseMark] {
        &self.phase_marks
    }

    /// Place a boundary the host decides, such as the end of the fixed idle
    /// window. Guest-decided boundaries arrive through `CMD_MARK` instead.
    pub fn record_host_phase_mark(&mut self, id: u8) {
        self.note_phase_mark(id);
    }

    /// Snapshot every counter the profiler attributes per phase, at the instant
    /// this boundary fired. Ignored unless `enable_phase_marks` armed recording.
    pub(crate) fn note_phase_mark(&mut self, id: u8) {
        if !self.phase_marks_enabled {
            return;
        }
        self.phase_marks.push(PhaseMark {
            id,
            wall: std::time::Instant::now(),
            master_ticks: self.master_ticks(),
            elapsed_clocks: self.elapsed_clocks(),
            perf: self.cpu.perf_counters().clone(),
            machine_phases: self.host_profile.snapshot(),
            katea: self.katea_storage_counters(),
            io_stall_ticks: self.timeline.io_stall_ticks(),
            halted_ticks: self.halted_ticks,
            int13: self.int13_profile,
            fast_map_audit: self.cpu.fast_map_audit_counters(),
            cpu_profile: self
                .cpu
                .profiling_enabled()
                .then(|| self.cpu.profile_snapshot()),
        });
    }

    /// Arm periodic sampling from inside the run loop, every `interval_clocks` CPU clocks.
    ///
    /// `interval_clocks` of 0 disarms. Also arms `enable_phase_marks`, since a periodic mark is a
    /// phase mark; call this INSTEAD of `enable_phase_marks`, never as well.
    ///
    /// Capacity is reserved up front from the caller's budget: a `Vec` realloc inside the run loop
    /// is a memcpy that lands in whichever interval it falls in, which is exactly the kind of
    /// cost that biases one phase against another.
    pub fn arm_periodic_phase_marks(&mut self, interval_clocks: u64, budget_clocks: u64) {
        self.phase_marks_enabled = true;
        self.phase_marks.clear();
        self.post_phase_marked = false;
        // Converted HERE rather than by the caller: `master_ticks_for_cpu_clocks` is the
        // timeline's own scaling and is not public, and the caller thinking in CPU clocks (the
        // currency `run_until_halt_or_cycles` already takes) keeps one unit in the CLI.
        let interval_ticks = self.timeline.master_ticks_for_cpu_clocks(interval_clocks);
        let budget_ticks = self.timeline.master_ticks_for_cpu_clocks(budget_clocks);
        if interval_ticks == 0 {
            self.periodic_phase_mark_interval = 0;
            self.next_phase_mark_ticks = u64::MAX;
            return;
        }
        self.periodic_phase_mark_interval = interval_ticks;
        self.next_phase_mark_ticks = self.timeline.now_ticks().saturating_add(interval_ticks);
        let expected = (budget_ticks / interval_ticks).saturating_add(8) as usize;
        self.phase_marks.reserve(expected.min(65_536));
    }

    /// The armed interval in master ticks. Test seam: the spacing assertion has to compare against
    /// the value the sampler actually uses, not a re-derivation of it.
    #[cfg(test)]
    pub(crate) fn periodic_phase_mark_interval_for_test(&self) -> u64 {
        self.periodic_phase_mark_interval
    }

    /// The periodic sample itself. COLD and never inlined: it sits one branch off the run loop,
    /// which `run_until_tick` documents as layout-sensitive, and this project has measured
    /// double-digit swings from layout in that loop.
    ///
    /// Deliberately LEANER than `note_phase_mark`, and the difference is load-bearing rather than
    /// tidiness. That one takes a full `cpu_profile` snapshot whenever `IZARRAVM_CPU_PROFILE` is
    /// armed -- which the hdd-folder benchmark path arms -- and that snapshot sorts `hot_addrs`,
    /// an UNTRUNCATED map of every distinct sampled address, which only grows across a run. Since
    /// `wall` is sampled first, mark k's snapshot cost is charged to interval k and grows
    /// monotonically, so late intervals would carry more overhead than early ones. On a fixture
    /// that loads and then renders, that reads as the render phase being slower than it is: the
    /// instrument would amplify the very knee it exists to find. So `cpu_profile` is None here
    /// unconditionally, and `machine_phases` (a per-mark Vec, all zero unless host profiling is
    /// armed) is left empty.
    #[cold]
    #[inline(never)]
    fn fire_periodic_phase_mark(&mut self) {
        let now = self.timeline.now_ticks();
        // A HLT fast-forward can jump several intervals at once. Advance past all of them and
        // sample ONCE: firing per skipped interval would emit zero-wall duplicates, which read
        // as infinite rt to any consumer that divides.
        let interval = self.periodic_phase_mark_interval.max(1);
        while self.next_phase_mark_ticks <= now {
            self.next_phase_mark_ticks = self.next_phase_mark_ticks.saturating_add(interval);
        }
        self.phase_marks.push(PhaseMark {
            id: phase_mark::PERIODIC,
            wall: std::time::Instant::now(),
            master_ticks: now,
            elapsed_clocks: self.elapsed_clocks(),
            perf: self.cpu.perf_counters().clone(),
            machine_phases: MachineHostProfileSnapshot::default(),
            katea: self.katea_storage_counters(),
            io_stall_ticks: self.timeline.io_stall_ticks(),
            halted_ticks: self.halted_ticks,
            int13: self.int13_profile,
            fast_map_audit: self.cpu.fast_map_audit_counters(),
            cpu_profile: None,
        });
    }

    /// Place the POST->boot boundary, once, at the first INT 19h.
    pub(crate) fn note_post_phase_mark(&mut self) {
        if !self.phase_marks_enabled || self.post_phase_marked {
            return;
        }
        self.post_phase_marked = true;
        self.note_phase_mark(phase_mark::POST_END);
    }

    /// Raw bus clocks charged by instruction fetches and data accesses since reset.
    /// The machine timeline applies the active mode's bus ratio separately.
    pub fn raw_bus_clocks(&self) -> u64 {
        self.trace.elapsed_clocks()
    }

    /// Scaled bus clocks committed by successful CPU batches since reset.
    pub fn scaled_bus_clocks(&self) -> u64 {
        self.scaled_bus_clocks
    }

    pub fn memory(&self) -> &Memory {
        &self.memory
    }

    pub fn serial_output(&self) -> &[u8] {
        self.serial.output()
    }

    pub fn serial2_output(&self) -> &[u8] {
        self.serial2.output()
    }

    /// Bytes captured by the LPT1 printer port (strobed prints, in order).
    pub fn lpt_output(&self) -> &[u8] {
        self.lpt.output()
    }

    pub fn lpt2_output(&self) -> &[u8] {
        self.lpt2.output()
    }

    /// The LPT1 capture decoded as text, the printer-side mirror of serial_text.
    pub fn lpt_text(&self) -> String {
        String::from_utf8_lossy(self.lpt_output()).into_owned()
    }

    /// Feed Set 1 scancodes to the keyboard controller (make on press, break on
    /// release). The controller schedules the wire transfer and IRQ1 deadline.
    /// Unclaimed-port accounting for this run: how many reads floated to
    /// all-ones, how many writes went nowhere, and which ports. A guest probing
    /// hardware this machine does not model shows up here instead of stopping
    /// the run; see `bus::OpenBusPorts`.
    /// Put `ports` back on the fatal path instead of floating them. The
    /// programmatic twin of `IZARRAVM_PORT_FATAL`, for chasing which
    /// instruction probes one specific port: the fatal path records a
    /// `fault_site`, and open bus by design does not stop to.
    pub fn set_fatal_ports(&mut self, ports: &[u16]) {
        self.open_bus.set_fatal(ports);
    }

    pub fn open_bus_ports(&self) -> &bus::OpenBusPorts {
        &self.open_bus
    }

    pub fn inject_key_scancodes(&mut self, codes: &[u8]) {
        self.keyboard.push_scancodes(codes);
        // Arms the 8042's output-buffer delivery timer, a `next_timed_io_deadline`
        // term. See `Machine::event_batch_cap_cached`.
        self.invalidate_device_edge_cache();
    }

    /// Feed a host mouse delta and button mask to the PS/2 aux device. `dx`/`dy`
    /// are host pixels (y down positive); `buttons` is bit0 left, bit1 right,
    /// bit2 middle. The aux device queues a movement packet and, when data
    /// reporting is enabled, this requests IRQ12 so a guest ISR runs.
    pub fn inject_mouse(&mut self, dx: i32, dy: i32, buttons: u8) {
        let _ = self.keyboard.inject_mouse(dx, dy, buttons);
        self.invalidate_device_edge_cache();
    }

    /// Inject a scroll-wheel detent as a PS/2 packet (IntelliMouse 4-byte mode).
    pub fn inject_mouse_wheel(&mut self, dz: i32) {
        let _ = self.keyboard.inject_mouse_wheel(dz);
        self.invalidate_device_edge_cache();
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

    /// Replace the state presented by joystick A on the ISA gameport. `None`
    /// electrically detaches the joystick and clears any charged RC timers.
    pub fn set_joystick_state(&mut self, state: Option<JoystickState>) {
        self.gameport.set_state(state);
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
        self.keyboard.set_mouse_irq(true);
        self.pic.set_irq_level(12, self.keyboard.irq12_level());
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
        self.keyboard.set_mouse_irq(true);
        self.pic.set_irq_level(12, self.keyboard.irq12_level());
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

    /// Whether the guest is executing in virtual-8086 mode under the TOKAEMM
    /// ring-0 monitor. The default-boot test uses this to verify CONFIG.SYS.
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
            let _ = self.write_guest_ram_u16(flags_addr, flags);
        }
    }

    fn guest_ram_scalar_physical(&self, address: usize, width: usize) -> Result<u32, BusError> {
        let end = address.saturating_add(width);
        if end > self.memory.len() {
            return Err(BusError::MemoryOutOfBounds {
                address,
                end,
                len: self.memory.len(),
            });
        }
        u32::try_from(address).map_err(|_| BusError::MemoryOutOfBounds {
            address,
            end,
            len: self.memory.len(),
        })
    }

    fn write_guest_ram_u8(&mut self, address: usize, value: u8) -> Result<(), BusError> {
        let physical = self.guest_ram_scalar_physical(address, 1)?;
        self.memory.write_u8(address, value)?;
        self.cpu.note_device_memory_write_range(physical, 1);
        Ok(())
    }

    fn write_guest_ram_u16(&mut self, address: usize, value: u16) -> Result<(), BusError> {
        let physical = self.guest_ram_scalar_physical(address, 2)?;
        self.memory.write_u16(address, value)?;
        self.cpu.note_device_memory_write_range(physical, 2);
        Ok(())
    }

    fn read_guest_block(&mut self, addr: u32, len: usize) -> Vec<u8> {
        (0..len)
            .map(|index| self.read_physical_u8(addr + index as u32))
            .collect()
    }

    /// Switch the active compatibility mode live. The CPU installs the core-table
    /// row once, while Machine refreshes its retained timing and cache state.
    pub fn set_mode(&mut self, mode: GswMode) {
        self.active_mode = mode;
        self.timeline.set_mode(mode);
        self.cpu.set_mode(mode);
        if let Ok(mut equipment) = self.memory.read_u16(0x410) {
            if mode.persona().has_fpu() {
                equipment |= BIOS_EQUIPMENT_FPU;
            } else {
                equipment &= !BIOS_EQUIPMENT_FPU;
            }
            let _ = self.write_guest_ram_u16(0x410, equipment);
        }
        // The modeled cache contents are per-mode, so a mode switch starts cold.
        self.cache_model.set_mode(mode);
        // The bus scaler's fractional carry is per-mode (the ratio changes); start
        // a new mode with no carried remainder, exactly like the CPU does for its
        // instruction-clock scaler.
        self.bus_rem = 0;
        // The cache holds master ticks, which survive a mode change, but the cap
        // conversion and the fallback grain are both per-mode; drop it so nothing
        // depends on that distinction.
        self.invalidate_device_edge_cache();
        self.advance_direct_mapping_epoch();
    }

    fn advance_direct_mapping_epoch(&mut self) {
        advance_direct_mapping_epoch(&mut self.direct_mapping_epoch);
    }

    fn mark_direct_map_changed(&mut self) {
        self.advance_direct_mapping_epoch();
        self.direct_map_changed = true;
    }

    /// The host-side twin of `MachineBus::mark_direct_data_map_changed`, for the INT 10h HLE seam.
    /// Same contract, including that it does not advance the direct-mapping epoch.
    fn mark_direct_data_map_changed(&mut self) {
        self.direct_data_map_changed = true;
    }

    /// Record one batch-boundary application of the direct-data-map wipe, when the census is armed.
    /// Called from the run loop's two apply sites, immediately before the CPU is told.
    pub(crate) fn note_vga_wipe_apply(&mut self) {
        if !self.vga_wipe_census.enabled {
            return;
        }
        let token = self.vega.direct_write_token();
        let instructions = self.cpu.perf_counters().instructions;
        self.vga_wipe_census.record_apply(token, instructions);
    }

    /// The VGA wipe census, or `None` when `IZARRAVM_VGA_WIPE_CENSUS` was not set.
    pub fn vga_wipe_census_snapshot(&self) -> Option<vga_wipe_census::VgaWipeCensusSnapshot> {
        self.vga_wipe_census.snapshot()
    }

    /// Guest OPL activity since power-on. Always collected: the counters are six
    /// increments on a port path that already ends the CPU batch, which is many
    /// orders of magnitude more expensive than the count.
    pub fn opl_diagnostics(&self) -> OplDiagnostics {
        self.opl_probe.counters()
    }

    /// Guest Sound Blaster DSP activity since power-on.
    pub fn sb_dsp_diagnostics(&self) -> SbDspDiagnostics {
        self.opl_probe.sb()
    }

    /// The recorded OPL access trace, empty unless `IZARRAVM_OPL_TRACE` was set.
    pub fn opl_trace(&self) -> &[OplTraceEntry] {
        self.opl_probe.trace()
    }

    /// Arm the OPL access trace directly, bypassing `IZARRAVM_OPL_TRACE`.
    ///
    /// For tests: the environment is process-global, so a test that set the
    /// variable would race every other test in the same binary.
    #[doc(hidden)]
    pub fn set_opl_trace_cap(&mut self, cap: usize) {
        self.opl_probe.cap = cap;
    }

    fn set_a20_gate(&mut self, enabled: bool) {
        if self.keyboard.a20_enabled() != enabled {
            self.keyboard.set_a20(enabled);
            self.advance_direct_mapping_epoch();
        }
    }

    /// The reported (L1 KB, L2 KB) cache for the live mode (the L2 models a
    /// motherboard cache module). Feeds the BIOS setup and GUI readout, and the same
    /// per-mode geometry (`cache_geometry`) also drives the `CacheModel` tiering, so
    /// this readout tracks the live data-access timing.
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

    /// The output-stage gain [`render_audio`](Self::render_audio) will apply.
    /// Exposed so a host that renders audio can be checked to have STAGED it:
    /// this is host loudness that leaves no trace in the samples of a silent
    /// machine, and the headless capture ran an entire investigation at the
    /// default 1.0 while the GUI ran at 12.0.
    pub fn card_amp(&self) -> f32 {
        self.card_amp
    }

    /// The PC speaker attenuation [`render_audio`](Self::render_audio) will
    /// apply. The speaker's counterpart to [`card_amp`](Self::card_amp).
    pub fn speaker_volume(&self) -> f32 {
        self.speaker_volume
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
    /// "amp gain") is applied to the card's own sources: OPL, SB DSP, the WSS
    /// codec, CD-audio through the card's CD-in, and the PC speaker through the
    /// card's PC-SPK input. The beeper is motherboard hardware, but its output
    /// is wired INTO the card, which is what mixer register `0x3B` attenuates;
    /// it used to be added after the amp and after the summing node's headroom
    /// reserve, which left it hot by that reserve (6 dB) on top of the 7 dB the
    /// card's own power-on PC-SPK level takes off.
    pub fn render_audio(&mut self, native_samples: usize) -> Vec<(i16, i16)> {
        let render_start = self.host_profile.start();
        let card_amp = self.card_amp;
        let speaker_volume = self.speaker_volume;
        let opl_native: Vec<(i32, i32)> = (0..native_samples)
            .map(|_| self.opl.render_sample())
            .collect();
        let opl_out = self.resampler.process(&opl_native);

        // DSP and WSS rings are produced by global guest time. Keep the drain
        // phases in the same fixed tick domain so a live CPU switch cannot
        // reinterpret time accumulated under the previous mode.
        let now_ticks = self.timeline.now_ticks();
        let delta_ticks = now_ticks.saturating_sub(self.last_audio_ticks);
        self.last_audio_ticks = now_ticks;
        let dsp_out = self.sb16.render_voice(Sb16RenderWindow {
            elapsed_master_ticks: delta_ticks,
            fallback_opl_samples: native_samples,
            output_frames: opl_out.len(),
        });

        // AD1848 / WSS: the same wall-clock window's worth of codec frames,
        // resampled to the DAC rate. The codec is independent of the CT1745 mixer
        // (its I6/I7 DAC attenuation is already applied inside the frames), so it
        // is summed directly below WITHOUT the SB16 master/voice/outgain scaling.
        // The mix window is the OPL resampler's count for this wall-clock
        // window; every other source has to land on it exactly.
        let len = opl_out.len();
        let wss_out: Vec<(i32, i32)> = if self.wss_enabled {
            self.sync_wss_resampler();
            let wss_native_count = if delta_ticks > 0 {
                self.wss_render_phase
                    .advance(delta_ticks, u64::from(self.wss.output_frame_rate()))
                    as usize
            } else {
                (native_samples as f64 * self.wss.output_frame_rate() as f64 / OPL_NATIVE_HZ as f64)
                    .round() as usize
            };
            let wss_stereo: Vec<(i32, i32)> = self
                .render_wss_audio(wss_native_count)
                .iter()
                .map(|&(l, r)| (i32::from(l), i32::from(r)))
                .collect();
            let produced = self.wss_resampler.process(&wss_stereo);
            // Same carry-over the SB16 voice uses: queue the surplus instead of
            // letting the positional read below drop it, so per-window jitter
            // costs a few frames of latency rather than a discarded frame and a
            // repeated one. Oldest frames go first once the cap is reached.
            let overflow =
                (self.wss_pending.len() + produced.len()).saturating_sub(DAC_PENDING_FRAME_CAP);
            for _ in 0..overflow {
                self.wss_pending.pop_front();
            }
            self.wss_pending.extend(produced);
            let take = len.min(self.wss_pending.len());
            self.wss_pending.drain(..take).collect()
        } else {
            // A disabled codec must not replay frames queued before it was
            // switched off.
            self.wss_pending.clear();
            Vec::new()
        };

        // Apply the CT1745 snapshot to OPL, DSP voice, and CD input. WSS remains
        // independent and is summed in raw afterward.
        let ct1745: Ct1745Mix = self.sb16.mix_snapshot();
        // The mix window is the WALL-clock window the OPL was rendered for, since
        // that is what the host sink consumes in real time. The DSP and WSS
        // streams are produced and drained on the GUEST clock, so a guest a few
        // percent short of real time hands back a few percent fewer frames every
        // call. Reading a short stream positionally with
        // `get(i).unwrap_or((0, 0))` appended a hole of hard silence to the DAC
        // stream on every render -- a full-scale impulse ~1000 times a second at
        // the frontend's pump rate, which is exactly the "crackling" a 486 persona
        // at 96-99% of real time produced. Hold the last frame instead (what the
        // DAC latch does on real hardware when its data is late).
        //
        // The hold used to run constantly, because both guest-clocked streams
        // were also forced to consume exactly one window's worth per call: the
        // surplus from a long window was discarded and the next short window was
        // padded out with repeats. A Quake capture measured ~14k frames dropped
        // and ~14k repeated per second against 44.1k rendered -- the documented
        // "zero-order hold spreads a sustained shortfall as repeated samples"
        // ceiling, made far worse by throwing the other half away. Both streams
        // now queue their surplus (`Sb16Path::render_voice` and `wss_pending`
        // above), so the hold below is what it was meant to be: cover for a
        // genuine underrun, not routine per-window jitter.
        // An idle source must fall silent rather than hold its last level forever,
        // so the latch is armed only while the channel still has output to make.
        let mut wss_hold = if self.wss_enabled && self.wss.is_playing() {
            self.wss_hold
        } else {
            (0, 0)
        };
        let spk = self.speaker.drain(len);
        // CD-Audio: pull the matching count of Red Book samples (44.1 kHz, the
        // DAC rate, so no resample) and attenuate by the CT1745 CD volume. A drive
        // that is not playing returns silence, so this is a no-op when no PLAY
        // AUDIO is active. This realizes CD audio through the ReSonique 2 DAC.
        let cd = self.pull_cd_audio_samples(len);
        let mixed = (0..len)
            .map(|i| {
                let (ol, or) = opl_out.get(i).copied().unwrap_or((0, 0));
                let (dl, dr) = dsp_out.get(i).copied().unwrap_or((0, 0));
                let (wl, wr) = hold_frame(&mut wss_hold, wss_out.get(i).copied());
                // Host PC speaker volume: a straight attenuation on the beeper,
                // taken before the card's PC-SPK input (0.0 mutes it). This is
                // the host's control; register 0x3B below is the guest's.
                let s = (f32::from(spk[i]) * speaker_volume) as i32;
                let (cl, cr) = cd.get(i).copied().unwrap_or((0, 0));
                let (sb_l, sb_r) = ct1745.mix_opl_voice((ol, or), (dl, dr));
                let (cl, cr) = ct1745.mix_cd((cl, cr));
                let (sl, sr) = ct1745.mix_speaker(s);
                // OPL + SB16 DSP take the CT1745 master/outgain; the PC speaker
                // takes the PC-SPK level and the same master; the WSS codec and
                // CD are summed in raw (their own attenuation already applied). All
                // of these are ReSonique 2 card sources, so the analog output-stage
                // gain (`card_amp`) scales their sum.
                // Sum the card sources, reserve the summing node's headroom, then
                // scale by the analog amp.
                //
                // `MIX_HEADROOM` is one scalar applied AFTER every card leg has
                // been summed, so it cannot disturb the relative FM/voice/CD
                // balance the CT1745 decode fix established, and it never touches
                // a hardware register. It exists because those legs all power on
                // at unity and therefore sum past full scale on their own; see the
                // constant for the full argument.
                let card_l = (sb_l + wl + cl + sl) as f32 * MIX_HEADROOM * card_amp;
                let card_r = (sb_r + wr + cr + sr) as f32 * MIX_HEADROOM * card_amp;
                let l = clamp_i16(card_l as i32);
                let r = clamp_i16(card_r as i32);
                (l, r)
            })
            .collect();
        self.wss_hold = wss_hold;
        self.host_profile
            .record(MachineProfilePhaseKind::AudioRender, render_start);
        mixed
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
        let epoch = self.ide.device().playback_epoch();
        if epoch != self.cd_audio_epoch {
            self.cd_audio_epoch = epoch;
            self.cd_audio_sample = 0;
        }
        if !self.ide.device().mixer_audio_active() {
            return out;
        }
        let [left_volume, right_volume] = self.ide.device().audio_volume();
        // cd_audio_sample is the next sample index within the current frame, carried
        // across render calls so the stream stays continuous. Peek the current
        // frame, drain its remaining samples, then step to the next frame.
        let mut sample_in_frame = self.cd_audio_sample;
        while out.len() < count {
            let Some(buf) = self.ide.device().peek_mixer_audio_frame() else {
                break; // playback reached its end mid-window
            };
            while sample_in_frame < SAMPLES_PER_FRAME && out.len() < count {
                let base = sample_in_frame * 4;
                let l = i16::from_le_bytes([buf[base], buf[base + 1]]);
                let r = i16::from_le_bytes([buf[base + 2], buf[base + 3]]);
                out.push((
                    i32::from(l) * i32::from(left_volume) / 255,
                    i32::from(r) * i32::from(right_volume) / 255,
                ));
                sample_in_frame += 1;
            }
            if sample_in_frame >= SAMPLES_PER_FRAME {
                // Consumed the whole frame: step the play position forward.
                self.ide.device_mut().advance_mixer_audio(1);
                sample_in_frame = 0;
            }
        }
        self.cd_audio_sample = sample_in_frame;
        out
    }

    /// Raise a hardware interrupt request line into the PIC.
    pub fn request_irq(&mut self, line: u8) {
        self.pic.request(line);
    }

    /// Pull one byte from a DMA channel's memory transfer (memory->device read).
    /// Returns None when the channel is masked or has reached terminal count. The
    /// The SB16 DSP uses this for 8-bit playback.
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

    /// Take the next complete message written to the wavetable MPU at
    /// 0x300/0x301. The MIDI engine drains this after each emulation pass.
    pub fn take_wavetable_midi_message(&mut self) -> Option<TimedMidiMessage> {
        self.wavetable_mpu.take_message()
    }

    /// Take the next complete message written to the MIDI MPU at 0x330/0x331.
    pub fn take_midi_message(&mut self) -> Option<TimedMidiMessage> {
        self.midi_mpu.take_message()
    }

    /// (Left, Right) linear gain for the MIDI legs, from the ReSonique 2
    /// wavetable volume registers `0x50`/`0x51`.
    ///
    /// Native MIDI synthesis is mixed by the frontend AFTER
    /// [`render_audio`](Self::render_audio) returns -- the synth runs on the
    /// host, not on the guest's clock -- so this leg cannot be folded into the
    /// summing node above. The caller applies it to the MIDI engines before
    /// they add themselves to the mix. It is a plain scalar read out of a guest
    /// device register, so the value is a pure function of canonical state.
    pub fn midi_gain(&self) -> (f32, f32) {
        self.sb16.wavetable_gain()
    }
}

struct MachineBus<'a> {
    memory: &'a mut Memory,
    ram_lookup: &'a mut RamPageLookup,
    vega: &'a mut Vega,
    pci: &'a mut PciConfig,
    rom: &'a [u8],
    serial: &'a mut uart::Uart16450,
    serial2: &'a mut uart::Uart16450,
    lpt: &'a mut lpt::Lpt,
    lpt2: &'a mut lpt::Lpt,
    device_ports: &'a mut DevicePorts,
    open_bus: &'a mut bus::OpenBusPorts,
    pic: &'a mut pic::Pic8259Pair,
    pit: &'a mut pit::Pit,
    keyboard: &'a mut keyboard::Keyboard8042,
    gameport: &'a mut gameport::GamePort,
    speaker: &'a mut speaker::Speaker,
    rtc: &'a mut rtc::Rtc,
    dma: &'a mut dma::DmaController,
    fdc: &'a mut fdc::Fdc,
    opl: &'a mut OplChip,
    sb16: &'a mut Sb16Path,
    wavetable_mpu: &'a mut Mpu401,
    midi_mpu: &'a mut Mpu401,
    // The AD1848 codec and its config-region base. The port decode routes the 8
    // ports in [wss_base, wss_base+8) to read_port/write_port when enabled; the
    // DMA/IRQ feed lives on the owning Machine in advance_devices, not here.
    wss: &'a mut Ad1848,
    wss_base: u16,
    wss_enabled: bool,
    ide: &'a mut ide::IdeChannel,
    ata: &'a mut Option<ata::AtaDisk>,
    bmide: &'a mut bmide::BusMasterIde,
    trace: &'a mut BusTrace,
    pending_soft_int: &'a mut Option<u8>,
    pending_bios32: &'a mut Option<Bios32Call>,
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
    /// 386 class and forced false by the bandwidth diagnostic so its tier
    /// curve stays on the accurate model.
    flat_data_cost: bool,
    /// True for the approximate-timing 486/586 modes, computed identically to
    /// `flat_data_cost` (same `active_mode.uses_approximate_timing()` check, same
    /// construction sites). Gates the lazy 3DA/3BA/3C2 dispatch in `read_io`:
    /// when true (or when `lazy_ports_386` is), a status-port read does not set
    /// `io_touched`, so a poll loop chains instead of ending its batch.
    ///
    /// What it does NOT decide any more is the VALUE. Since the beam peek landed
    /// (42721631) BOTH arms compute their bits from `predicted_beam()`; the
    /// Accurate arm merely sets `io_touched` first and ends the batch as it
    /// always has. So this bool no longer buys byte-identical Accurate-class
    /// behavior -- it buys the batch-ending behavior alone.
    ///
    /// Nor is it the sole test at that dispatch: every shared use site reads
    /// `lazy_port_reads || lazy_ports_386` (see `lazy_ports_386` below), so
    /// "one arm branches on it" describes the shape of the dispatch -- a static
    /// per-port landing plus a bool test, never a per-access classification --
    /// not a single reader.
    lazy_port_reads: bool,
    /// The Accurate-class (386) extension of the lazy time-derived port reads:
    /// 3DA/3BA/3C2 (VGA status), 0x61 (PIT channel 1/2 OUT), and 0x200-0x207
    /// (the gameport RC one-shots) answer WITHOUT ending the CPU batch.
    /// `IZARRAVM_LAZY_PORT_386`, DEFAULT OFF.
    ///
    /// It is false for the whole Approximate class BY CONSTRUCTION, not merely
    /// by default: 486/586 already get the 3DA and 0x61 arms from
    /// `lazy_port_reads`, and the gameport arm is new, so letting this bool go
    /// true there would silently move a pinned 486/586 fixture. Every use site
    /// is therefore `lazy_port_reads || lazy_ports_386` (the two shared arms) or
    /// `lazy_ports_386` alone (the gameport).
    ///
    /// Deliberately NOT folded into `lazy_port_reads`, which also gates the
    /// ring-0-monitor `io_touched` exemption, the OPL ISA-I/O charge, and
    /// `predicted_opl_status`. Those three are Approximate-class policy and do
    /// not move with this switch: the Accurate class's ISA-I/O charging rules
    /// stay byte-identical either way, and an OPL status poll stays
    /// batch-ending in both classes.
    ///
    /// THE DRIFT, stated exactly, because it is why the default is OFF.
    /// A lazy read is exact against the contract "end the batch HERE, advance
    /// devices, then read": `predicted_beam` and `Pit::out_after` are pinned to
    /// agree with a real `advance_devices` of the same clock total. But the
    /// batch-ending path the Accurate class uses today is the OTHER order -- it
    /// reads the LIVE device state, which is the state as of BATCH START, and
    /// only then ends the batch. So moving a port from batch-ending to lazy
    /// moves its answer forward by the batch clocks already elapsed at the read
    /// instant. That is strictly closer to hardware and strictly not
    /// byte-identical in general, and on a retrace poll it can change how many
    /// iterations the loop spins.
    ///
    /// 0x200-0x207 is the exception and the one port where the value provably
    /// cannot move: it already comes from `guest_tick_now()` (the in-batch
    /// instant) in BOTH classes today, so only the batch boundary moves, not
    /// the function of time being sampled.
    lazy_ports_386: bool,
    // Set true by any port I/O this batch. The run loop batches straight-line
    // instructions and services devices once per batch; a port access (a PIT
    // latch read, 0x3DA retrace poll, RTC read, a PIT/PIC/DSP/mode write) reads
    // or changes time-dependent device state, so it ends the batch to keep that
    // state exact. Memory/MMIO (framebuffer blits, the hot path) does not set it.
    io_touched: &'a mut bool,
    // Points at `Machine::exempt_io_touched`; see the field there.
    exempt_io_touched: &'a mut bool,
    // Accrues fixed ISA-bus time (CPU clocks) for the OPL status poll in the
    // Approximate class; the run loop folds it into the batch's device advance.
    // Points at `Machine::isa_io_batch_clocks`.
    isa_io_clocks: &'a mut u64,
    // Points at `Machine::pit_observer_fine_until`. Armed by any PIT counter or
    // control port access; see that field.
    pit_observer_fine_until: &'a mut u64,
    // Diagnostic-only OPL counters and trace. Points at `Machine::opl_probe`.
    // Never read by any emulation decision; see `OplProbe`.
    opl_probe: &'a mut OplProbe,
    device_wrote_memory: &'a mut bool,
    pending_device_memory_write_range: &'a mut Option<(u32, u32)>,
    direct_map_changed: &'a mut bool,
    direct_data_map_changed: &'a mut bool,
    direct_mapping_epoch: &'a mut u64,
    // Env-gated attribution for the direct-write-token seam; see `vga_wipe_census`.
    vga_wipe_census: &'a mut crate::vga_wipe_census::VgaWipeCensus,
    // A copy of the current read_io call's core_clocks_so_far argument (CPU core
    // clocks charged by prior instructions in this straight-line run, not
    // including the in-flight IN). Written at the top of every read_io call so a
    // lazy-port arm can read it without extra plumbing. Initialized to 0 at bus construction; the
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
    // Fixed-time snapshot used by lazy PIT and beam reads. Predictions clone the
    // same integer phases that the batch-end advance consumes.
    timeline_at_batch_start: Timeline,
    master_ticks_at_batch_start: u64,
    beam_at_batch_start: u64,
    trace_elapsed_at_batch_start: u64,
    bus_rem_at_batch_start: u64,
    // bus_timing(cpu.level())'s (num, den) ratio, copied at bus construction from
    // the same authoritative CPU mode `scale_bus` reads. `predicted_beam` must
    // scale in-batch bus clocks with exactly this ratio to match the real
    // end-of-batch `scale_bus` call.
    bus_num_at_batch_start: u32,
    bus_den_at_batch_start: u32,
}

fn advance_direct_mapping_epoch(epoch: &mut u64) {
    *epoch = epoch.wrapping_add(1);
    if *epoch == 0 {
        *epoch = 1;
    }
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

/// Take the next frame of a guest-clocked DAC stream, latching it as the value
/// to hold if the stream runs out before the wall-clock render window does.
/// Holding the last level is what a real DAC's output latch does when its next
/// sample is late; substituting silence would put a full-scale step in the
/// stream on every render call the guest fell short on.
fn hold_frame(hold: &mut (i32, i32), frame: Option<(i32, i32)>) -> (i32, i32) {
    if let Some(frame) = frame {
        *hold = frame;
    }
    *hold
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
    let is_leap = |y: u16| (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400);
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
/// (LPT1 and LPT2 are emulated). Bit 1 is added for the 486 and 586 modes,
/// whose CPU personas include an x87 unit, and stays clear for both 386 modes.
/// The bit layout follows RBIL's INT 11h equipment word.
const BIOS_EQUIPMENT_WORD: u16 = 0x8421;
const BIOS_EQUIPMENT_FPU: u16 = 0x0002;

/// Conventional memory size in KiB reported by INT 12h (BDA 0040:0013). A PC
/// caps usable low memory at 640 KiB no matter how much RAM is installed; the
/// rest is extended memory above 1 MiB (reported by INT 15h AH=88h).
const BIOS_BASE_MEMORY_KIB: u16 = 640;

#[cfg(test)]
#[path = "machine_code_write_coherence_test.rs"]
mod code_write_coherence_tests;

#[cfg(test)]
#[path = "machine_fault_site_test.rs"]
mod fault_site_tests;

#[cfg(test)]
#[path = "phase_mark_test.rs"]
mod phase_mark_tests;

#[cfg(test)]
#[path = "machine_test.rs"]
mod tests;
