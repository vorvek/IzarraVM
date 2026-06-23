use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use thiserror::Error;

pub const MIN_MEMORY_MIB: u16 = 2;
pub const MAX_MEMORY_MIB: u16 = 64;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse config {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: Box<toml::de::Error>,
    },
    #[error("memory_mib must be between {MIN_MEMORY_MIB} and {MAX_MEMORY_MIB}, got {0}")]
    InvalidMemory(u16),
    #[error("unknown {kind} preset '{value}'")]
    UnknownPreset { kind: &'static str, value: String },
    #[error(
        "audio.wss.base {0:#06x} places the 8-port WSS window [{0:#06x}, {1:#06x}) over a fixed chipset/device port range; use a documented WSS base (0x530, 0x604, 0xE80, or 0xF40)"
    )]
    InvalidWssBase(u16, u16),
    #[error(
        "audio.wss.dma {0} collides with audio.sound_blaster.dma {0}; the AD1848 and SB16 must use distinct 8237 DMA channels (real combo cards jumper them apart, e.g. WSS DMA0 vs SB16 DMA1)"
    )]
    WssSbDmaCollision(usize),
    #[error(
        "audio.wss.irq {0} collides with audio.sound_blaster.irq {0}; the AD1848 and SB16 must use distinct PIC lines (real combo cards jumper them apart, e.g. WSS IRQ7 vs SB16 IRQ5)"
    )]
    WssSbIrqCollision(u8),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GswMode {
    #[serde(rename = "286")]
    Gsw286,
    #[serde(rename = "386")]
    #[default]
    Gsw386,
    #[serde(rename = "486")]
    Gsw486,
    #[serde(rename = "586")]
    Gsw586,
}

impl GswMode {
    /// The throttled core clock per compatibility mode: 8.33 MHz (286 mode, the Super
    /// Slow setting), 22 MHz (386 mode, picked for the early-386 game range), 66 MHz
    /// (486 mode), and 266 MHz native (the K6-266 bin on the 66 MHz bus).
    pub const fn clock_hz(self) -> u64 {
        match self {
            Self::Gsw286 => 8_333_333,
            Self::Gsw386 => 22_000_000,
            Self::Gsw486 => 66_000_000,
            Self::Gsw586 => 266_000_000,
        }
    }

    /// Reported cache sizes per compatibility mode as (L1 KB, L2 KB). The L2 is a
    /// motherboard cache module; the whole table is cosmetic and feeds the cache
    /// readout only, with no timing effect.
    pub const fn cache_kb(self) -> (u16, u16) {
        match self {
            Self::Gsw286 => (0, 0),
            Self::Gsw386 => (0, 64),
            Self::Gsw486 => (16, 128),
            Self::Gsw586 => (32, 512),
        }
    }

    pub const fn canonical_name(self) -> &'static str {
        match self {
            Self::Gsw286 => "286",
            Self::Gsw386 => "386",
            Self::Gsw486 => "486",
            Self::Gsw586 => "586",
        }
    }
}

impl fmt::Display for GswMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.canonical_name())
    }
}

impl FromStr for GswMode {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match normalize(value).as_str() {
            // Primary GSW names plus legacy Intel aliases so old configs parse.
            "286" | "gsw286" | "286_8" | "i286" | "286super" | "superslow" => Ok(Self::Gsw286),
            "386" | "gsw386" | "386dx25" | "i386dx25" | "i386dx_25" | "386_25" => Ok(Self::Gsw386),
            "486" | "gsw486" | "486dx266" | "i486dx266" | "i486dx2_66" | "486dx2_66" => {
                Ok(Self::Gsw486)
            }
            "586" | "gsw586" => Ok(Self::Gsw586),
            _ => Err(ConfigError::UnknownPreset {
                kind: "CPU",
                value: value.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCard {
    #[serde(rename = "et4000_ax")]
    #[default]
    Et4000Ax,
    #[serde(rename = "s3_virge_dx")]
    S3VirgeDx,
    #[serde(rename = "voodoo2")]
    Voodoo2,
}

impl VideoCard {
    pub const fn canonical_name(self) -> &'static str {
        match self {
            Self::Et4000Ax => "et4000_ax",
            Self::S3VirgeDx => "s3_virge_dx",
            Self::Voodoo2 => "voodoo2",
        }
    }
}

impl fmt::Display for VideoCard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.canonical_name())
    }
}

impl FromStr for VideoCard {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match normalize(value).as_str() {
            "et4000ax" | "et4000_ax" | "tsenget4000ax" => Ok(Self::Et4000Ax),
            "s3virgedx" | "s3_virge_dx" | "virgedx" => Ok(Self::S3VirgeDx),
            "voodoo2" | "3dfxvoodoo2" => Ok(Self::Voodoo2),
            _ => Err(ConfigError::UnknownPreset {
                kind: "video",
                value: value.to_owned(),
            }),
        }
    }
}

/// The state of the EMM386-equivalent role of the IEMM memory manager, the way a
/// period CONFIG.SYS selects it. HIMEM (XMS + the HMA) is a separate, always-on
/// facility; this governs only upper memory and expanded memory, the part a 386
/// needs the manager's address remapping for:
/// - `Unloaded`: HIMEM only, no EMM386. No UMBs and no EMS (a 386 cannot map the
///   upper area without the manager). This is the `DEVICE=HIMEM.SYS` block.
/// - `NoEms`: EMM386 with NOEMS. UMBs plus a frameless EMS manager: the
///   EMMXXXX0 device and INT 67h answer (status present, version 4.0), but there
///   is no page frame and zero backing pages, so allocation fails.
/// - `Ram`: EMM386 with RAM. UMBs plus the LIM EMS 4.0 page frame and INT 67h.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Emm386Mode {
    #[serde(rename = "unloaded")]
    Unloaded,
    #[serde(rename = "noems")]
    NoEms,
    #[serde(rename = "ram")]
    #[default]
    Ram,
}

impl Emm386Mode {
    pub const fn canonical_name(self) -> &'static str {
        match self {
            Self::Unloaded => "unloaded",
            Self::NoEms => "noems",
            Self::Ram => "ram",
        }
    }

    /// Whether upper memory blocks are provided (EMM386 loaded in either mode).
    pub const fn provides_umb(self) -> bool {
        matches!(self, Self::NoEms | Self::Ram)
    }

    /// Whether the EMS page frame and INT 67h expanded memory are provided.
    pub const fn provides_ems(self) -> bool {
        matches!(self, Self::Ram)
    }
}

impl fmt::Display for Emm386Mode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.canonical_name())
    }
}

impl FromStr for Emm386Mode {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match normalize(value).as_str() {
            "unloaded" | "off" | "none" | "himemonly" => Ok(Self::Unloaded),
            "noems" => Ok(Self::NoEms),
            "ram" | "ems" | "on" => Ok(Self::Ram),
            _ => Err(ConfigError::UnknownPreset {
                kind: "emm386",
                value: value.to_owned(),
            }),
        }
    }
}

/// The memory-manager configuration a CONFIG.SYS declares: the EMM386 mode its
/// DEVICE= lines select, and whether DOS=UMB asks for the UMB area to be linked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigSysMemory {
    pub emm386: Emm386Mode,
    pub dos_umb: bool,
    /// Optional EMS page-frame segment from an IEMM/EMM386 `FRAME=` token.
    /// The token is written in hexadecimal like real EMM386 CONFIG.SYS lines.
    pub ems_frame_seg: Option<u16>,
    /// Optional expanded-memory backing-pool size in KiB. Real EMM386 accepts a
    /// bare decimal number on its DEVICE line; IEMM follows that convention.
    pub ems_pool_kb: Option<u32>,
}

fn parse_iemm_frame_token(token: &str) -> Option<u16> {
    let value = token.strip_prefix("FRAME=")?;
    let value = value.strip_prefix("0X").unwrap_or(value);
    u16::from_str_radix(value, 16).ok()
}

fn parse_iemm_pool_token(token: &str) -> Option<u32> {
    if token.chars().all(|character| character.is_ascii_digit()) {
        token.parse().ok()
    } else {
        None
    }
}

/// Read the memory-manager intent out of a CONFIG.SYS. The IEMM memory manager
/// (filed as IEMM.EXE in Toka-DOS, or the real-DOS EMM386.EXE) provides upper
/// memory only when HIMEM.SYS loads first, matching real DOS, so an IEMM/EMM386
/// line with no preceding HIMEM line (or no such line at all) yields Unloaded:
/// - HIMEM.SYS only -> Unloaded (XMS, no UMB/EMS)
/// - HIMEM.SYS + IEMM.EXE NOEMS -> NoEms (UMBs, no EMS frame)
/// - HIMEM.SYS + IEMM.EXE RAM (or no arg) -> Ram (UMBs + EMS frame)
///
/// EMM386.EXE is accepted as an alias so a pasted real-DOS CONFIG.SYS still works.
///
/// `FRAME=hhhh` selects the EMS page-frame segment and a bare decimal number on
/// the manager line selects the EMS backing-pool size in KiB. DOS=UMB (or
/// DOS=HIGH,UMB) sets `dos_umb`.
pub fn parse_config_sys(text: &str) -> ConfigSysMemory {
    let mut himem = false;
    let mut emm386 = None;
    let mut dos_umb = false;
    let mut ems_frame_seg = None;
    let mut ems_pool_kb = None;
    for line in text.lines() {
        let upper = line.trim().to_ascii_uppercase();
        let device = upper
            .strip_prefix("DEVICEHIGH=")
            .or_else(|| upper.strip_prefix("DEVICE="));
        if let Some(rest) = device {
            let rest = rest.trim_start();
            let path = rest.split_whitespace().next().unwrap_or("");
            let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
            if name == "HIMEM.SYS" {
                himem = true;
            } else if (name == "IEMM.EXE"
                || name == "IEMM"
                || name == "EMM386.EXE"
                || name == "EMM386")
                && himem
            {
                // IEMM (the Toka-DOS manager) or its real-DOS alias EMM386 loads
                // only with a prior HIMEM. NOEMS omits the EMS frame.
                let mut line_frame_seg = None;
                let mut line_pool_kb = None;
                let noems = rest
                    .split_whitespace()
                    .skip(1)
                    .any(|token| token == "NOEMS");
                for token in rest.split_whitespace().skip(1) {
                    if let Some(frame_seg) = parse_iemm_frame_token(token) {
                        line_frame_seg = Some(frame_seg);
                    }
                    if let Some(pool_kb) = parse_iemm_pool_token(token) {
                        line_pool_kb = Some(pool_kb);
                    }
                }
                ems_frame_seg = line_frame_seg;
                ems_pool_kb = line_pool_kb;
                emm386 = Some(if noems {
                    Emm386Mode::NoEms
                } else {
                    Emm386Mode::Ram
                });
            }
        } else if let Some(rest) = upper.strip_prefix("DOS=") {
            if rest.split([',', ' ']).any(|token| token.trim() == "UMB") {
                dos_umb = true;
            }
        }
    }
    ConfigSysMemory {
        emm386: emm386.unwrap_or(Emm386Mode::Unloaded),
        dos_umb,
        ems_frame_seg,
        ems_pool_kb,
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SbIrq {
    #[serde(rename = "2")]
    I2,
    #[serde(rename = "5")]
    #[default]
    I5,
    #[serde(rename = "7")]
    I7,
    #[serde(rename = "10")]
    I10,
}

impl SbIrq {
    /// The PC AT IRQ line number the CT1745 mixer routes the DSP interrupt to.
    pub const fn line(self) -> u8 {
        match self {
            Self::I2 => 2,
            Self::I5 => 5,
            Self::I7 => 7,
            Self::I10 => 10,
        }
    }

    pub const fn canonical_name(self) -> &'static str {
        match self {
            Self::I2 => "2",
            Self::I5 => "5",
            Self::I7 => "7",
            Self::I10 => "10",
        }
    }
}

impl fmt::Display for SbIrq {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.canonical_name())
    }
}

impl FromStr for SbIrq {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match normalize(value).as_str() {
            "2" | "irq2" => Ok(Self::I2),
            "5" | "irq5" => Ok(Self::I5),
            "7" | "irq7" => Ok(Self::I7),
            "10" | "irq10" => Ok(Self::I10),
            _ => Err(ConfigError::UnknownPreset {
                kind: "Sound Blaster IRQ",
                value: value.to_owned(),
            }),
        }
    }
}

/// IRQ line for the Windows Sound System (AD1848) codec. The WSS standard
/// documents IRQ 7/9/10/11 (see `dev_docs/reference/wss/README.md`), a set that
/// only partially overlaps `SbIrq` (which carries 2/5/7/10): WSS cannot use 2 or
/// 5, and `SbIrq` cannot express 9 or 11. A dedicated enum keeps the codec's
/// configurable lines faithful to the documented set.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum WssIrq {
    #[serde(rename = "7")]
    #[default]
    I7,
    #[serde(rename = "9")]
    I9,
    #[serde(rename = "10")]
    I10,
    #[serde(rename = "11")]
    I11,
}

impl WssIrq {
    /// The PC AT IRQ line number the codec's terminal-count interrupt forwards to.
    pub const fn line(self) -> u8 {
        match self {
            Self::I7 => 7,
            Self::I9 => 9,
            Self::I10 => 10,
            Self::I11 => 11,
        }
    }

    pub const fn canonical_name(self) -> &'static str {
        match self {
            Self::I7 => "7",
            Self::I9 => "9",
            Self::I10 => "10",
            Self::I11 => "11",
        }
    }
}

impl fmt::Display for WssIrq {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.canonical_name())
    }
}

impl FromStr for WssIrq {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match normalize(value).as_str() {
            "7" | "irq7" => Ok(Self::I7),
            "9" | "irq9" => Ok(Self::I9),
            "10" | "irq10" => Ok(Self::I10),
            "11" | "irq11" => Ok(Self::I11),
            _ => Err(ConfigError::UnknownPreset {
                kind: "Windows Sound System IRQ",
                value: value.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SbDma8 {
    #[serde(rename = "0")]
    D0,
    #[serde(rename = "1")]
    #[default]
    D1,
    #[serde(rename = "3")]
    D3,
}

impl SbDma8 {
    /// The 8237A master channel number (0/1/3) the CT1745 routes 8-bit DMA to.
    pub const fn channel(self) -> usize {
        match self {
            Self::D0 => 0,
            Self::D1 => 1,
            Self::D3 => 3,
        }
    }

    pub const fn canonical_name(self) -> &'static str {
        match self {
            Self::D0 => "0",
            Self::D1 => "1",
            Self::D3 => "3",
        }
    }
}

impl fmt::Display for SbDma8 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.canonical_name())
    }
}

impl FromStr for SbDma8 {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match normalize(value).as_str() {
            "0" | "dma0" => Ok(Self::D0),
            "1" | "dma1" => Ok(Self::D1),
            "3" | "dma3" => Ok(Self::D3),
            _ => Err(ConfigError::UnknownPreset {
                kind: "Sound Blaster 8-bit DMA",
                value: value.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SbDma16 {
    #[serde(rename = "5")]
    #[default]
    D5,
    #[serde(rename = "6")]
    D6,
    #[serde(rename = "7")]
    D7,
}

impl SbDma16 {
    /// The 8237A slave channel number (5/6/7) the CT1745 routes 16-bit DMA to.
    pub const fn channel(self) -> usize {
        match self {
            Self::D5 => 5,
            Self::D6 => 6,
            Self::D7 => 7,
        }
    }

    pub const fn canonical_name(self) -> &'static str {
        match self {
            Self::D5 => "5",
            Self::D6 => "6",
            Self::D7 => "7",
        }
    }
}

impl fmt::Display for SbDma16 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.canonical_name())
    }
}

impl FromStr for SbDma16 {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match normalize(value).as_str() {
            "5" | "dma5" => Ok(Self::D5),
            "6" | "dma6" => Ok(Self::D6),
            "7" | "dma7" => Ok(Self::D7),
            _ => Err(ConfigError::UnknownPreset {
                kind: "Sound Blaster 16-bit DMA",
                value: value.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MidiBackend {
    Off,
    #[default]
    External,
    FluidSynth,
}

impl MidiBackend {
    pub const fn canonical_name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::External => "external",
            Self::FluidSynth => "fluidsynth",
        }
    }
}

impl fmt::Display for MidiBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.canonical_name())
    }
}

impl FromStr for MidiBackend {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match normalize(value).as_str() {
            "off" | "none" => Ok(Self::Off),
            "external" | "midir" | "midiout" => Ok(Self::External),
            "fluidsynth" | "fluid" | "sf2" => Ok(Self::FluidSynth),
            _ => Err(ConfigError::UnknownPreset {
                kind: "MIDI backend",
                value: value.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SteamInputMode {
    #[default]
    Off,
    OptionalBackend,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub machine: MachineConfig,
    pub dos: DosConfig,
    pub audio: AudioConfig,
    pub input: InputConfig,
    pub diagnostics: DiagnosticsConfig,
}

impl AppConfig {
    pub fn from_toml_path(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let text = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;

        toml::from_str::<Self>(&text).map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
            source: Box::new(source),
        })
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if !(MIN_MEMORY_MIB..=MAX_MEMORY_MIB).contains(&self.machine.memory_mib) {
            return Err(ConfigError::InvalidMemory(self.machine.memory_mib));
        }

        if self.audio.wss.enabled {
            self.audio.wss.validate_base()?;

            // On a real multi-standard combo card the AD1848 (WSS) and the SB16
            // are jumpered to distinct IRQ/DMA resources; two devices cannot share
            // an 8237 channel or a PIC line. Reject a config that points them at
            // the same one (the defaults -- WSS IRQ7/DMA0 vs SB16 IRQ5/DMA1 -- are
            // disjoint). The 16-bit SB16 channel cannot collide with the WSS 8-bit
            // channel (SbDma16 is 5/6/7, SbDma8 is 0/1/3), so only the 8-bit DMA
            // and the IRQ line need checking.
            if self.audio.sound_blaster.enabled {
                let wss_dma = self.audio.wss.dma.channel();
                if wss_dma == self.audio.sound_blaster.dma.channel() {
                    return Err(ConfigError::WssSbDmaCollision(wss_dma));
                }
                let wss_irq = self.audio.wss.irq.line();
                if wss_irq == self.audio.sound_blaster.irq.line() {
                    return Err(ConfigError::WssSbIrqCollision(wss_irq));
                }
            }
        }

        Ok(())
    }

    pub fn apply_overrides(&mut self, overrides: ConfigOverrides) {
        if let Some(cpu) = overrides.cpu {
            self.machine.cpu = cpu;
        }
        if let Some(memory_mib) = overrides.memory_mib {
            self.machine.memory_mib = memory_mib;
        }
        if let Some(video) = overrides.video {
            self.machine.video = video;
        }
        if let Some(c_drive) = overrides.c_drive {
            self.dos.c_drive = c_drive;
        }
        if let Some(soundfont) = overrides.soundfont {
            self.audio.midi.soundfont = Some(soundfont);
        }
        if let Some(midi_backend) = overrides.midi_backend {
            self.audio.midi.backend = midi_backend;
        }
        if let Some(sb_irq) = overrides.sb_irq {
            self.audio.sound_blaster.irq = sb_irq;
        }
        if let Some(sb_dma) = overrides.sb_dma {
            self.audio.sound_blaster.dma = sb_dma;
        }
        if let Some(sb_high_dma) = overrides.sb_high_dma {
            self.audio.sound_blaster.high_dma = sb_high_dma;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MachineConfig {
    pub cpu: GswMode,
    pub memory_mib: u16,
    pub video: VideoCard,
    /// The IEMM EMM386-role state: unloaded (HIMEM only), noems (UMBs only), or
    /// ram (UMBs plus the EMS page frame). Defaults to the do-it-right `ram` box.
    pub emm386: Emm386Mode,
}

impl Default for MachineConfig {
    fn default() -> Self {
        Self {
            cpu: GswMode::Gsw386,
            memory_mib: 24, // Izarra 3000: 24 MB, 3 x 8 MB DIMMs
            video: VideoCard::Et4000Ax,
            emm386: Emm386Mode::Ram,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DosConfig {
    pub c_drive: PathBuf,
    /// Optional CD image (an `.iso` or a `.cue`) mounted into the ATAPI drive at
    /// startup. None leaves the optical drive empty; the GUI can still mount a
    /// disc live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cd_image: Option<PathBuf>,
}

impl Default for DosConfig {
    fn default() -> Self {
        Self {
            c_drive: PathBuf::from("."),
            cd_image: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SoundBlasterConfig {
    /// Whether the host constructs the SB16 audio path. Mirrors the former
    /// `AudioConfig.sound_blaster: bool`.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Power-on IRQ line the CT1745 mixer selects (register 0x80). Applied once
    /// at boot like `SBCONFIG`; a guest mixer reset restores the hardware
    /// factory default (IRQ5).
    #[serde(default)]
    pub irq: SbIrq,
    /// Power-on 8-bit DMA channel (register 0x81 low bits).
    #[serde(default)]
    pub dma: SbDma8,
    /// Power-on 16-bit DMA channel (register 0x81 high bits).
    #[serde(default)]
    pub high_dma: SbDma16,
}

impl Default for SoundBlasterConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            irq: SbIrq::I5,
            dma: SbDma8::D1,
            high_dma: SbDma16::D5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WssConfig {
    /// Whether the host constructs the Windows Sound System (AD1848 codec) path.
    /// The codec is always present on the ReSonique 2 combo card, so this defaults
    /// to enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// I/O base port of the four-port WSS config region (the AD1848 codec sits at
    /// base+4). Defaults to 0x530, the de-facto WSS standard base.
    #[serde(default = "default_wss_base")]
    pub base: u16,
    /// Power-on IRQ line read back from the board config region. Defaults to IRQ7,
    /// chosen to avoid the SB16 default (IRQ5). Uses `WssIrq`, which carries the
    /// documented WSS lines 7/9/10/11.
    #[serde(default)]
    pub irq: WssIrq,
    /// Power-on 8-bit DMA channel read back from the board config region. Defaults
    /// to DMA0, chosen to avoid the SB16 default (DMA1). Reuses `SbDma8` (whose own
    /// `Default` is DMA1, so the WSS default is supplied explicitly).
    #[serde(default = "default_wss_dma")]
    pub dma: SbDma8,
}

impl Default for WssConfig {
    fn default() -> Self {
        // base 0x530, IRQ7, DMA0 -- chosen to avoid the SB16 defaults (IRQ5/DMA1).
        Self {
            enabled: true,
            base: 0x530,
            irq: WssIrq::I7,
            dma: SbDma8::D0,
        }
    }
}

impl WssConfig {
    /// Fixed I/O port ranges the WSS window must not shadow. The codec decode is
    /// checked before the 8237 DMA controller, PIT, PIC, and the SB16/OPL/IDE/FDC
    /// decoders in `MachineBus::read_io`/`write_io`, so a window overlapping any of
    /// these would silently steal those ports with no diagnostic. Validating the
    /// base here turns a dangerous config into a load-time error instead.
    const RESERVED_RANGES: &'static [(u16, u16)] = &[
        (0x0000, 0x001f), // 8237 DMA controller 1 + aliases
        (0x0020, 0x003f), // PIC 1
        (0x0040, 0x005f), // PIT
        (0x0060, 0x006f), // 8042 keyboard controller / system control ports
        (0x0070, 0x007f), // RTC / NMI mask
        (0x0080, 0x009f), // DMA page registers
        (0x00a0, 0x00bf), // PIC 2
        (0x00c0, 0x00df), // 8237 DMA controller 2
        (0x00e0, 0x00ef), // Lotura system controller
        (0x01f0, 0x01f7), // IDE/ATA primary task file
        (0x0220, 0x022f), // Sound Blaster base + CT1745 mixer
        (0x0278, 0x027f), // LPT2 parallel port
        (0x02f8, 0x02ff), // COM2 serial port (16450 UART)
        (0x0378, 0x037f), // LPT1 parallel port
        (0x0388, 0x038b), // OPL2/OPL3
        (0x03b0, 0x03df), // MDA/CGA/EGA/VGA registers
        (0x03f0, 0x03f7), // FDC + IDE alias
        (0x03f8, 0x03ff), // COM1 serial port (16450 UART)
    ];

    /// The eight-port WSS window `[base, base + 8)`, saturating at 0xFFFF.
    pub const fn window(&self) -> (u16, u16) {
        (self.base, self.base.saturating_add(8))
    }

    /// Reject a `base` whose eight-port window overlaps any fixed chipset/device
    /// port range (see `RESERVED_RANGES`). The documented WSS bases (0x530,
    /// 0x604, 0xE80, 0xF40) all pass; a low or occupied base does not.
    pub fn validate_base(&self) -> Result<(), ConfigError> {
        let win_start = u32::from(self.base);
        let win_end = win_start + 8; // exclusive; cannot overflow u32
        for &(lo, hi) in Self::RESERVED_RANGES {
            // Two half-open ranges overlap iff start < other_end && other_start < end.
            if win_start <= u32::from(hi) && u32::from(lo) < win_end {
                return Err(ConfigError::InvalidWssBase(
                    self.base,
                    self.base.saturating_add(8),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudioConfig {
    pub pc_speaker: bool,
    pub sound_blaster: SoundBlasterConfig,
    pub wss: WssConfig,
    pub opl3: bool,
    pub midi: MidiConfig,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            pc_speaker: true,
            sound_blaster: SoundBlasterConfig::default(),
            wss: WssConfig::default(),
            opl3: true,
            midi: MidiConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MidiConfig {
    pub backend: MidiBackend,
    pub soundfont: Option<PathBuf>,
}

impl Default for MidiConfig {
    fn default() -> Self {
        Self {
            backend: MidiBackend::External,
            soundfont: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InputConfig {
    pub keyboard: bool,
    pub mouse: bool,
    pub joystick: bool,
    pub steam_input: SteamInputMode,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            keyboard: true,
            mouse: true,
            joystick: true,
            steam_input: SteamInputMode::Off,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct DiagnosticsConfig {
    pub trace_devices: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ConfigOverrides {
    pub cpu: Option<GswMode>,
    pub memory_mib: Option<u16>,
    pub video: Option<VideoCard>,
    pub c_drive: Option<PathBuf>,
    pub soundfont: Option<PathBuf>,
    pub midi_backend: Option<MidiBackend>,
    pub sb_irq: Option<SbIrq>,
    pub sb_dma: Option<SbDma8>,
    pub sb_high_dma: Option<SbDma16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareProfile {
    pub cpu: GswMode,
    pub clock_hz: u64,
    pub memory_mib: u16,
    pub video: VideoCard,
    pub sound_blaster: SoundBlasterConfig,
    pub wss: WssConfig,
    pub emm386: Emm386Mode,
}

impl HardwareProfile {
    pub fn from_config(config: &AppConfig) -> Result<Self, ConfigError> {
        config.validate()?;

        Ok(Self {
            cpu: config.machine.cpu,
            clock_hz: config.machine.cpu.clock_hz(),
            memory_mib: config.machine.memory_mib,
            video: config.machine.video,
            sound_blaster: config.audio.sound_blaster,
            wss: config.audio.wss,
            emm386: config.machine.emm386,
        })
    }
}

fn default_true() -> bool {
    true
}

fn default_wss_base() -> u16 {
    0x530
}

fn default_wss_dma() -> SbDma8 {
    SbDma8::D0
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|character| !matches!(character, '-' | ' '))
        .collect::<String>()
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gsw_mode_clocks_and_names() {
        assert_eq!(GswMode::Gsw286.clock_hz(), 8_333_333);
        assert_eq!(GswMode::Gsw386.clock_hz(), 22_000_000);
        assert_eq!(GswMode::Gsw486.clock_hz(), 66_000_000);
        assert_eq!(GswMode::Gsw586.clock_hz(), 266_000_000);
        assert_eq!(GswMode::Gsw286.canonical_name(), "286");
        assert_eq!(GswMode::Gsw586.canonical_name(), "586");
        assert_eq!(GswMode::default(), GswMode::Gsw386);
    }

    #[test]
    fn gsw_mode_cache_table_per_mode() {
        assert_eq!(GswMode::Gsw286.cache_kb(), (0, 0));
        assert_eq!(GswMode::Gsw386.cache_kb(), (0, 64));
        assert_eq!(GswMode::Gsw486.cache_kb(), (16, 128));
        assert_eq!(GswMode::Gsw586.cache_kb(), (32, 512));
    }

    #[test]
    fn gsw_mode_parses_primary_and_legacy_names() {
        assert_eq!("286".parse::<GswMode>().unwrap(), GswMode::Gsw286);
        assert_eq!("386".parse::<GswMode>().unwrap(), GswMode::Gsw386);
        assert_eq!("486".parse::<GswMode>().unwrap(), GswMode::Gsw486);
        assert_eq!("586".parse::<GswMode>().unwrap(), GswMode::Gsw586);
        assert_eq!("i386dx_25".parse::<GswMode>().unwrap(), GswMode::Gsw386);
        assert!("pentium_133".parse::<GswMode>().is_err());
    }

    #[test]
    fn emm386_mode_parses_and_gates_features() {
        assert_eq!(Emm386Mode::default(), Emm386Mode::Ram);
        assert_eq!("noems".parse::<Emm386Mode>().unwrap(), Emm386Mode::NoEms);
        assert_eq!(
            "unloaded".parse::<Emm386Mode>().unwrap(),
            Emm386Mode::Unloaded
        );
        assert!("bogus".parse::<Emm386Mode>().is_err());
        // RAM provides both UMBs and EMS; NOEMS only UMBs; unloaded neither.
        assert!(Emm386Mode::Ram.provides_ems() && Emm386Mode::Ram.provides_umb());
        assert!(!Emm386Mode::NoEms.provides_ems() && Emm386Mode::NoEms.provides_umb());
        assert!(!Emm386Mode::Unloaded.provides_umb());
        // The [machine] table accepts the mode; a config without it defaults to ram.
        let cfg: AppConfig = toml::from_str("[machine]\nemm386 = \"noems\"\n").unwrap();
        assert_eq!(cfg.machine.emm386, Emm386Mode::NoEms);
        assert_eq!(AppConfig::default().machine.emm386, Emm386Mode::Ram);
    }

    #[test]
    fn config_sys_drives_the_emm386_mode() {
        let ram = parse_config_sys(
            "DEVICE=C:\\DOS\\HIMEM.SYS /TESTMEM:OFF\r\nDEVICE=C:\\DOS\\EMM386.EXE RAM\r\nDOS=HIGH,UMB\r\n",
        );
        assert_eq!(ram.emm386, Emm386Mode::Ram);
        assert!(ram.dos_umb);

        let noems = parse_config_sys("DEVICE=HIMEM.SYS\r\nDEVICE=EMM386.EXE NOEMS\r\nDOS=HIGH\r\n");
        assert_eq!(noems.emm386, Emm386Mode::NoEms);
        assert!(!noems.dos_umb);

        // HIMEM alone provides no UMB/EMS manager.
        assert_eq!(
            parse_config_sys("DEVICE=C:\\DOS\\HIMEM.SYS\r\n").emm386,
            Emm386Mode::Unloaded
        );
        // EMM386 without a preceding HIMEM cannot load.
        assert_eq!(
            parse_config_sys("DEVICE=EMM386.EXE RAM\r\n").emm386,
            Emm386Mode::Unloaded
        );
        // An empty CONFIG.SYS is the no-manager case.
        assert_eq!(
            parse_config_sys("FILES=40\r\n").emm386,
            Emm386Mode::Unloaded
        );

        // IEMM.EXE is the Toka-DOS memory manager name; it drives the mode the
        // same way EMM386.EXE does. Both the full and the bare name work.
        let iemm_ram = parse_config_sys("DEVICE=HIMEM.SYS\r\nDEVICE=IEMM.EXE RAM\r\n");
        assert_eq!(iemm_ram.emm386, Emm386Mode::Ram);
        let iemm_noems = parse_config_sys("DEVICE=HIMEM.SYS\r\nDEVICE=IEMM.EXE NOEMS\r\n");
        assert_eq!(iemm_noems.emm386, Emm386Mode::NoEms);
        let iemm_bare = parse_config_sys("DEVICE=HIMEM.SYS\r\nDEVICE=IEMM\r\n");
        assert_eq!(iemm_bare.emm386, Emm386Mode::Ram);
        // The real-DOS EMM386 name stays accepted (a pasted real-DOS config works).
        let emm386_bare = parse_config_sys("DEVICE=HIMEM.SYS\r\nDEVICE=EMM386\r\n");
        assert_eq!(emm386_bare.emm386, Emm386Mode::Ram);
        // IEMM without a preceding HIMEM cannot load either.
        assert_eq!(
            parse_config_sys("DEVICE=IEMM.EXE RAM\r\n").emm386,
            Emm386Mode::Unloaded
        );
    }

    #[test]
    fn config_sys_parses_iemm_frame_and_pool_size_knobs() {
        let config = parse_config_sys(
            "DEVICE=C:\\DOS\\HIMEM.SYS /TESTMEM:OFF\r\nDEVICE=C:\\DOS\\IEMM.EXE RAM FRAME=D000 4096\r\nDOS=HIGH,UMB\r\n",
        );

        assert_eq!(config.emm386, Emm386Mode::Ram);
        assert_eq!(config.ems_frame_seg, Some(0xd000));
        assert_eq!(config.ems_pool_kb, Some(4096));
        assert!(config.dos_umb);
    }

    #[test]
    fn rejects_memory_outside_supported_range() {
        let mut config = AppConfig::default();
        config.machine.memory_mib = 1;
        assert!(matches!(
            config.validate(),
            Err(ConfigError::InvalidMemory(1))
        ));

        config.machine.memory_mib = 65;
        assert!(matches!(
            config.validate(),
            Err(ConfigError::InvalidMemory(65))
        ));
    }

    #[test]
    fn applies_cli_style_overrides() {
        let mut config = AppConfig::default();
        config.apply_overrides(ConfigOverrides {
            cpu: Some(GswMode::Gsw386),
            memory_mib: Some(32),
            video: Some(VideoCard::S3VirgeDx),
            c_drive: Some(PathBuf::from("games")),
            soundfont: Some(PathBuf::from("gm.sf2")),
            midi_backend: Some(MidiBackend::FluidSynth),
            sb_irq: Some(SbIrq::I7),
            sb_dma: Some(SbDma8::D3),
            sb_high_dma: Some(SbDma16::D6),
        });

        assert_eq!(config.machine.cpu, GswMode::Gsw386);
        assert_eq!(config.machine.memory_mib, 32);
        assert_eq!(config.machine.video, VideoCard::S3VirgeDx);
        assert_eq!(config.dos.c_drive, PathBuf::from("games"));
        assert_eq!(config.audio.midi.soundfont, Some(PathBuf::from("gm.sf2")));
        assert_eq!(config.audio.midi.backend, MidiBackend::FluidSynth);
        assert_eq!(config.audio.sound_blaster.irq, SbIrq::I7);
        assert_eq!(config.audio.sound_blaster.dma, SbDma8::D3);
        assert_eq!(config.audio.sound_blaster.high_dma, SbDma16::D6);
    }

    #[test]
    fn sound_blaster_overrides_and_aliases_parse() {
        assert_eq!("7".parse::<SbIrq>().unwrap(), SbIrq::I7);
        assert_eq!("irq10".parse::<SbIrq>().unwrap(), SbIrq::I10);
        assert_eq!("3".parse::<SbDma8>().unwrap(), SbDma8::D3);
        assert_eq!("dma6".parse::<SbDma16>().unwrap(), SbDma16::D6);
        assert_eq!(SbIrq::I10.line(), 10);
        assert_eq!(SbDma8::D3.channel(), 3);
        assert_eq!(SbDma16::D7.channel(), 7);
    }

    #[test]
    fn sound_blaster_config_defaults_when_absent_or_partial() {
        // No [audio.sound_blaster] table: the hardware default (IRQ5/DMA1/DMA5).
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("izarravm.toml");
        fs::write(
            &path,
            r#"
                [machine]
                cpu = "386"
                memory_mib = 16
                video = "et4000_ax"
            "#,
        )
        .unwrap();
        let config = AppConfig::from_toml_path(path).unwrap();
        assert_eq!(
            config.audio.sound_blaster,
            SoundBlasterConfig {
                enabled: true,
                irq: SbIrq::I5,
                dma: SbDma8::D1,
                high_dma: SbDma16::D5
            }
        );

        // A partial table fills the omitted fields from their defaults.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("izarravm.toml");
        fs::write(
            &path,
            r#"
                [audio.sound_blaster]
                enabled = true
                irq = "7"
            "#,
        )
        .unwrap();
        let config = AppConfig::from_toml_path(path).unwrap();
        assert!(config.audio.sound_blaster.enabled);
        assert_eq!(config.audio.sound_blaster.irq, SbIrq::I7);
        assert_eq!(config.audio.sound_blaster.dma, SbDma8::D1);
        assert_eq!(config.audio.sound_blaster.high_dma, SbDma16::D5);
    }

    #[test]
    fn wss_config_defaults_when_absent_or_partial() {
        // No [audio.wss] table: the codec is always present (enabled), at the
        // WSS standard base 0x530 with IRQ7/DMA0 (chosen to dodge SB16 IRQ5/DMA1).
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("izarravm.toml");
        fs::write(
            &path,
            r#"
                [machine]
                cpu = "386"
                memory_mib = 16
                video = "et4000_ax"
            "#,
        )
        .unwrap();
        let config = AppConfig::from_toml_path(path).unwrap();
        assert_eq!(
            config.audio.wss,
            WssConfig {
                enabled: true,
                base: 0x530,
                irq: WssIrq::I7,
                dma: SbDma8::D0,
            }
        );

        // A partial table fills the omitted fields from their defaults.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("izarravm.toml");
        fs::write(
            &path,
            r#"
                [audio.wss]
                enabled = true
                irq = "10"
            "#,
        )
        .unwrap();
        let config = AppConfig::from_toml_path(path).unwrap();
        assert!(config.audio.wss.enabled);
        assert_eq!(config.audio.wss.base, 0x530);
        assert_eq!(config.audio.wss.irq, WssIrq::I10);
        assert_eq!(config.audio.wss.dma, SbDma8::D0);
    }

    #[test]
    fn wss_config_parses_overrides_when_present() {
        // A full [audio.wss] table overrides every field, including disabling the
        // codec, picking a non-default base, and the alias-driven IRQ/DMA enums.
        // IRQ11 is one of the two documented WSS lines (9/11) that the SB16's
        // `SbIrq` cannot express, so it also pins the dedicated `WssIrq` parse.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("izarravm.toml");
        fs::write(
            &path,
            r#"
                [audio.wss]
                enabled = false
                base = 0x604
                irq = "11"
                dma = "3"
            "#,
        )
        .unwrap();
        let config = AppConfig::from_toml_path(path).unwrap();
        assert_eq!(
            config.audio.wss,
            WssConfig {
                enabled: false,
                base: 0x604,
                irq: WssIrq::I11,
                dma: SbDma8::D3,
            }
        );
    }

    #[test]
    fn wss_irq_parses_documented_lines_and_rejects_others() {
        // The documented WSS lines are 7/9/10/11; anything else (e.g. the SB16's
        // IRQ5, which `SbIrq` carried but the codec cannot) must be rejected.
        assert_eq!("7".parse::<WssIrq>().unwrap(), WssIrq::I7);
        assert_eq!("irq9".parse::<WssIrq>().unwrap(), WssIrq::I9);
        assert_eq!("10".parse::<WssIrq>().unwrap(), WssIrq::I10);
        assert_eq!("11".parse::<WssIrq>().unwrap(), WssIrq::I11);
        assert_eq!(WssIrq::I9.line(), 9);
        assert_eq!(WssIrq::I11.line(), 11);
        assert!("5".parse::<WssIrq>().is_err(), "IRQ5 is not a WSS line");
    }

    #[test]
    fn rejects_wss_base_that_shadows_fixed_ports() {
        // The documented WSS bases all pass validation.
        for base in [0x530u16, 0x604, 0xE80, 0xF40] {
            let mut config = AppConfig::default();
            config.audio.wss.base = base;
            assert!(
                config.validate().is_ok(),
                "documented WSS base {base:#06x} must validate"
            );
        }
        // A base whose window shadows the 8237 DMA controller (0x000-0x00F) is
        // rejected so it cannot silently steal those ports at the WSS decode.
        let mut config = AppConfig::default();
        config.audio.wss.base = 0x0004;
        assert!(matches!(
            config.validate(),
            Err(ConfigError::InvalidWssBase(0x0004, 0x000C))
        ));
        // base 0x000 (full overlap with DMA ch1) is likewise rejected.
        config.audio.wss.base = 0x0000;
        assert!(matches!(
            config.validate(),
            Err(ConfigError::InvalidWssBase(0x0000, 0x0008))
        ));
        // A window straddling the SB16 base (0x21C..0x224 overlaps 0x220) is caught.
        config.audio.wss.base = 0x021C;
        assert!(matches!(
            config.validate(),
            Err(ConfigError::InvalidWssBase(0x021C, 0x0224))
        ));
        // A disabled codec is not validated, so even a dangerous base is allowed.
        config.audio.wss.enabled = false;
        config.audio.wss.base = 0x0000;
        assert!(
            config.validate().is_ok(),
            "disabled WSS skips base validation"
        );
    }

    #[test]
    fn rejects_wss_base_over_serial_or_parallel_ports() {
        // read_io decodes the COM/LPT UARTs before the WSS window, so a base over
        // them would be silently shadowed. validate_base must reject those too.
        // COM2 (0x2F8): window 0x2F8..0x300 overlaps the 0x2F8-0x2FF UART.
        let mut config = AppConfig::default();
        config.audio.wss.base = 0x02F8;
        assert!(
            matches!(
                config.validate(),
                Err(ConfigError::InvalidWssBase(0x02F8, _))
            ),
            "a WSS base over COM2 must be rejected"
        );
        // LPT1 (0x378): window 0x378..0x380 overlaps the 0x378-0x37F parallel port.
        config.audio.wss.base = 0x0378;
        assert!(
            matches!(
                config.validate(),
                Err(ConfigError::InvalidWssBase(0x0378, _))
            ),
            "a WSS base over LPT1 must be rejected"
        );
        // COM1 (0x3F8): window overlaps the 0x3F8-0x3FF UART.
        config.audio.wss.base = 0x03F8;
        assert!(
            matches!(
                config.validate(),
                Err(ConfigError::InvalidWssBase(0x03F8, _))
            ),
            "a WSS base over COM1 must be rejected"
        );
        // LPT2 (0x278): window overlaps the 0x278-0x27F parallel port.
        config.audio.wss.base = 0x0278;
        assert!(
            matches!(
                config.validate(),
                Err(ConfigError::InvalidWssBase(0x0278, _))
            ),
            "a WSS base over LPT2 must be rejected"
        );
    }

    #[test]
    fn rejects_wss_sb16_irq_or_dma_collision() {
        // On a real combo card the AD1848 and SB16 are jumpered to distinct IRQ/DMA
        // resources. The defaults are disjoint (WSS IRQ7/DMA0 vs SB16 IRQ5/DMA1), so
        // a default config validates.
        let config = AppConfig::default();
        assert!(config.validate().is_ok(), "disjoint defaults validate");

        // Pointing the WSS at the SB16's DMA channel (DMA1) is rejected.
        let mut config = AppConfig::default();
        config.audio.wss.dma = SbDma8::D1; // == SB16 default DMA1
        assert!(matches!(
            config.validate(),
            Err(ConfigError::WssSbDmaCollision(1))
        ));

        // Pointing the WSS at the SB16's IRQ line (both IRQ7) is rejected.
        let mut config = AppConfig::default();
        config.audio.wss.irq = WssIrq::I7;
        config.audio.sound_blaster.irq = SbIrq::I7;
        assert!(matches!(
            config.validate(),
            Err(ConfigError::WssSbIrqCollision(7))
        ));

        // With the SB16 disabled there is no contention, so a "colliding" config is
        // allowed (the SB16 is not present to fight over the resource).
        let mut config = AppConfig::default();
        config.audio.sound_blaster.enabled = false;
        config.audio.wss.dma = SbDma8::D1;
        config.audio.wss.irq = WssIrq::I7;
        config.audio.sound_blaster.irq = SbIrq::I7;
        assert!(
            config.validate().is_ok(),
            "a disabled SB16 cannot collide with the WSS"
        );
    }

    #[test]
    fn loads_toml_config() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("izarravm.toml");
        fs::write(
            &path,
            r#"
                [machine]
                cpu = "386"
                memory_mib = 16
                video = "et4000_ax"
            "#,
        )
        .unwrap();

        let config = AppConfig::from_toml_path(path).unwrap();
        assert_eq!(config.machine.cpu, GswMode::Gsw386);
        assert_eq!(config.machine.video, VideoCard::Et4000Ax);
        assert_eq!(config.dos.c_drive, PathBuf::from("."));
    }
}
