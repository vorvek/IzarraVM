// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

#![forbid(unsafe_code)]

mod canonical_state;
mod clock;
mod gsw;

pub use canonical_state::{
    CANONICAL_STATE_CONTAINER_MAGIC, CANONICAL_STATE_CONTAINER_MAJOR,
    CANONICAL_STATE_CONTAINER_MINOR, CanonicalFieldWriter, CanonicalSection, CanonicalSectionId,
    CanonicalSectionRequirement, CanonicalSectionVersion, CanonicalStateError, CanonicalStateView,
    CanonicalStateWriter,
};
pub use clock::{ClockRate, MASTER_CLOCK_HZ};
pub use gsw::{CacheGeometry, CpuPersona, GSW_MODE_SPECS, GswMode, GswModeSpec, L1Cache};

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
    #[error("CPU preset '286' was removed; use '386-slow'")]
    RemovedCpu286,
    #[error(
        "audio.wss.base {0:#06x} places the 8-port WSS window [{0:#06x}, {1:#06x}) over a fixed chipset/device port range; use a documented WSS base (0x530, 0x604, 0xE80, or 0xF40)"
    )]
    InvalidWssBase(u16, u16),
    #[error(
        "audio.wss.dma {0} collides with audio.sound_blaster.dma {0}; the AD1848 and SB16 must use distinct 8237 DMA channels (real combo cards jumper them apart, e.g. WSS DMA0 vs SB16 DMA1)"
    )]
    WssSbDmaCollision(usize),
    #[error(
        "audio.wss.irq {0} collides with audio.sound_blaster.irq {0}; the AD1848 and SB16 must use distinct PIC lines (real combo cards jumper them apart, e.g. SB16 IRQ7 vs WSS IRQ11)"
    )]
    WssSbIrqCollision(u8),
    #[error(
        "{path} holds the GUI's own preferences (it has a top-level `{key}` key), not a machine config. Those are two different files that have both been called izarravm.conf: the GUI writes its preferences next to the C: drive folder, while --config takes a machine description you write yourself (see examples/machine.toml). Point --config at the latter, or drop the flag to use the built-in defaults"
    )]
    GuiPrefsGivenAsMachineConfig { path: PathBuf, key: &'static str },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCard {
    #[serde(
        rename = "vega",
        alias = "et4000ax",
        alias = "et4000_ax",
        alias = "s3virgedx",
        alias = "s3_virge_dx",
        alias = "distira",
        alias = "voodoo1",
        alias = "voodoo_graphics",
        alias = "voodoo2"
    )]
    #[default]
    Vega,
}

impl VideoCard {
    pub const fn canonical_name(self) -> &'static str {
        match self {
            Self::Vega => "vega",
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
            "vega" | "et4000ax" | "et4000_ax" | "tsenget4000ax" | "s3virgedx" | "s3_virge_dx"
            | "virgedx" | "distira" | "voodoo1" | "voodoo_graphics" | "voodoographics"
            | "3dfxvoodoo" | "voodoo2" | "3dfxvoodoo2" => Ok(Self::Vega),
            _ => Err(ConfigError::UnknownPreset {
                kind: "video",
                value: value.to_owned(),
            }),
        }
    }
}

fn split_device_path_and_args(rest: &str) -> (&str, &str) {
    let rest = rest.trim_start();
    if let Some(quoted) = rest.strip_prefix('"')
        && let Some(end) = quoted.find('"')
    {
        return (&quoted[..end], quoted[end + 1..].trim_start());
    }
    let path_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    (&rest[..path_end], rest[path_end..].trim_start())
}

/// One DEVICE=/DEVICEHIGH= line from CONFIG.SYS, in file order, with the path and
/// argument tail kept in their original case. Uppercased internally for
/// matching, but a driver path and its switches must keep case to load and run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigDeviceLine {
    pub path: String,
    pub args: String,
    pub high: bool,
}

/// The filename of a DOS path, after the last `\` or `/`.
pub fn dos_basename(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// Every DEVICE=/DEVICEHIGH= line in order. Memory-manager lines (HIMEM/IZEMM/
/// EMM386) are included; the caller decides which basenames it handles itself.
pub fn parse_device_lines(text: &str) -> Vec<ConfigDeviceLine> {
    let mut lines = Vec::new();
    for raw in text.lines() {
        let trimmed = raw.trim();
        let upper = trimmed.to_ascii_uppercase();
        let (cased_rest, high) = if let Some(rest) = upper.strip_prefix("DEVICEHIGH=") {
            (&trimmed[trimmed.len() - rest.len()..], true)
        } else if let Some(rest) = upper.strip_prefix("DEVICE=") {
            (&trimmed[trimmed.len() - rest.len()..], false)
        } else {
            continue;
        };
        // `to_ascii_uppercase` preserves byte length, so the uppercased remainder's
        // length re-slices the original cased line at the same point.
        let (path, args) = split_device_path_and_args(cased_rest);
        lines.push(ConfigDeviceLine {
            path: path.to_string(),
            args: args.to_string(),
            high,
        });
    }
    lines
}

/// IRQ line for the Sound Blaster DSP.
///
/// **Defaults to 7, not to the SB16-era 5.** Two populations of DOS software have
/// to be satisfied at once, and only 7 satisfies both:
///
/// * Titles that HARDWIRE an IRQ overwhelmingly hardwire 7, because 7 was the
///   factory default on the Sound Blaster 1.x/2.0 that their drivers were written
///   against. Chess Housers is one: its driver hooks vector 0x0F and never looks
///   at `BLASTER`, so on IRQ 5 its ISR never runs, nothing re-arms the DSP after
///   the first high-speed block, and all you get is a ~15 ms click.
/// * Titles that READ `BLASTER` follow whatever we advertise, so they are happy
///   either way (`stock_autoexec` generates the line from this value).
///
/// Hardwiring 5 is rare, because 5 only became a default with SB16-class cards,
/// by which time reading `BLASTER` was standard practice. DOSBox reached the same
/// conclusion and ships `irq = 7`. A card exposing two IRQ lines at once would
/// dodge the choice, but no real card does that, so it is not an option here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SbIrq {
    #[serde(rename = "2")]
    I2,
    #[serde(rename = "5")]
    I5,
    #[serde(rename = "7")]
    #[default]
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

    /// The inverse of `line`: recover the variant from a raw line number, or
    /// `None` if the card cannot route to it. Used to read the assignment back
    /// out of the CMOS block `SNDCTRL.COM` writes, where the value is a plain
    /// byte that a guest could have set to anything.
    pub const fn from_line(line: u8) -> Option<Self> {
        match line {
            2 => Some(Self::I2),
            5 => Some(Self::I5),
            7 => Some(Self::I7),
            10 => Some(Self::I10),
            _ => None,
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
/// documents IRQ 7/9/10/11, a set that
/// only partially overlaps `SbIrq` (which carries 2/5/7/10): WSS cannot use 2 or
/// 5, and `SbIrq` cannot express 9 or 11. A dedicated enum keeps the codec's
/// configurable lines faithful to the documented set.
///
/// **Defaults to 11 and yields 7 to the Sound Blaster.** WSS's own standard
/// default is 7, and this used to take it precisely because `SbIrq` defaulted to
/// 5 -- but that is the wrong way round for a DOS library. Far more titles hardwire
/// the SB on 7 than hardwire the codec, and the two cannot share a line (see
/// `ConfigError::WssSbIrqCollision`; real combo cards jumper them apart). 11 is
/// chosen over 9 because IRQ 9 is the cascaded IRQ 2 and still catches software
/// that pokes the old XT line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum WssIrq {
    #[serde(rename = "7")]
    I7,
    #[serde(rename = "9")]
    I9,
    #[serde(rename = "10")]
    I10,
    #[serde(rename = "11")]
    #[default]
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

    /// The inverse of `line`, for reading the CMOS block back. See
    /// [`SbIrq::from_line`].
    pub const fn from_line(line: u8) -> Option<Self> {
        match line {
            7 => Some(Self::I7),
            9 => Some(Self::I9),
            10 => Some(Self::I10),
            11 => Some(Self::I11),
            _ => None,
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

    /// The inverse of `channel`, for reading the CMOS block back. See
    /// [`SbIrq::from_line`].
    pub const fn from_channel(channel: usize) -> Option<Self> {
        match channel {
            0 => Some(Self::D0),
            1 => Some(Self::D1),
            3 => Some(Self::D3),
            _ => None,
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

    /// The inverse of `channel`, for reading the CMOS block back. See
    /// [`SbIrq::from_line`].
    pub const fn from_channel(channel: usize) -> Option<Self> {
        match channel {
            5 => Some(Self::D5),
            6 => Some(Self::D6),
            7 => Some(Self::D7),
            _ => None,
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

pub const WAVETABLE_MPU_BASE: u16 = 0x300;
pub const MIDI_MPU_BASE: u16 = 0x330;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MidiBackend {
    #[serde(
        alias = "none",
        alias = "fluid_synth",
        alias = "fluidsynth",
        alias = "fluid",
        alias = "sf2"
    )]
    #[default]
    Off,
    External,
    Munt,
}

impl MidiBackend {
    pub const fn canonical_name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::External => "external",
            Self::Munt => "munt",
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
            "off" | "none" | "fluid_synth" | "fluidsynth" | "fluid" | "sf2" => Ok(Self::Off),
            "external" | "midir" | "midiout" => Ok(Self::External),
            "munt" | "mt32" => Ok(Self::Munt),
            _ => Err(ConfigError::UnknownPreset {
                kind: "P330 MIDI receiver (off, external, or munt)",
                value: value.to_owned(),
            }),
        }
    }
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

/// Settings that used to live in the config file and are now owned by the
/// machine's CMOS instead.
///
/// They were removed because a config file cannot win an argument with NVRAM.
/// CMOS is what the machine actually boots from, and it is written by the BIOS
/// setup panel and by `SNDCTRL.COM`; once either of those has saved anything,
/// editing the matching key here changed nothing at all. A setting that
/// silently does nothing is worse than one that is absent, so these are absent.
///
/// They are *stripped* rather than rejected: an existing config file keeps
/// loading, with a warning naming what was ignored. Each entry is the path to
/// one key, outermost table first.
pub const RETIRED_CMOS_KEYS: &[&[&str]] = &[
    &["machine", "cpu"],
    &["audio", "sound_blaster", "irq"],
    &["audio", "sound_blaster", "dma"],
    &["audio", "sound_blaster", "high_dma"],
    &["audio", "wss", "irq"],
    &["audio", "wss", "dma"],
];

/// Top-level keys that only ever appear in the GUI's preferences file. Used to
/// recognise `--config <the GUI's own izarravm.conf>` and say so, rather than
/// failing with a bare unknown-field error that names one key and explains
/// nothing. They are `GuiPrefs` fields with no `AppConfig` counterpart, so a
/// real machine config can never carry one.
const GUI_PREFS_MARKER_KEYS: &[&str] = &[
    "master_volume",
    "amp_gain",
    "pc_speaker_volume",
    "crt_style",
    "input_release",
    "fullscreen_toggle",
];

/// `Some(key)` when a parsed document is the GUI's preferences file rather than
/// a machine config.
pub fn gui_prefs_marker(value: &toml::Value) -> Option<&'static str> {
    let table = value.as_table()?;
    GUI_PREFS_MARKER_KEYS
        .iter()
        .find(|key| table.contains_key(**key))
        .copied()
}

/// Remove every [`RETIRED_CMOS_KEYS`] entry from a parsed config document,
/// returning the dotted names of the ones that were actually present so the
/// caller can say which keys it ignored.
///
/// This has to happen before deserialization, not after: the retired fields are
/// `#[serde(skip)]`, which makes their names unknown, and `AppConfig` denies
/// unknown fields. Without this pass an existing config file would stop loading
/// altogether rather than quietly losing the keys it no longer owns.
pub fn strip_retired_keys(value: &mut toml::Value) -> Vec<String> {
    let mut dropped = Vec::new();
    for path in RETIRED_CMOS_KEYS {
        let (leaf, tables) = path.split_last().expect("retired key path is never empty");
        let mut table = value.as_table_mut();
        for name in tables {
            table = match table.and_then(|t| t.get_mut(*name)) {
                Some(next) => next.as_table_mut(),
                None => None,
            };
        }
        if let Some(table) = table
            && table.remove(*leaf).is_some()
        {
            dropped.push(path.join("."));
        }
    }
    dropped
}

impl AppConfig {
    pub fn from_toml_path(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let text = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        let parse_error = |source: toml::de::Error| ConfigError::Parse {
            path: path.to_owned(),
            source: Box::new(source),
        };
        let mut value = toml::from_str::<toml::Value>(&text).map_err(parse_error)?;
        if let Some(key) = gui_prefs_marker(&value) {
            return Err(ConfigError::GuiPrefsGivenAsMachineConfig {
                path: path.to_owned(),
                key,
            });
        }
        strip_retired_keys(&mut value);
        value.try_into::<Self>().map_err(parse_error)
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
        if let Some(external_port) = overrides.external_midi_port {
            self.audio.midi.external_port = Some(external_port);
        }
        if let Some(control_rom) = overrides.mt32_control_rom {
            self.audio.midi.mt32_control_rom = Some(control_rom);
        }
        if let Some(pcm_rom) = overrides.mt32_pcm_rom {
            self.audio.midi.mt32_pcm_rom = Some(pcm_rom);
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
    /// Power-on CPU speed class.
    ///
    /// **Not read from the config file** — see [`RETIRED_CMOS_KEYS`]. It seeds
    /// CMOS on a machine that has never been configured; after that the BIOS
    /// setup panel (Del) and the Tab boot menu own it, and `GSWMODE` changes it
    /// for the running session. `--cpu` still overrides this for headless runs,
    /// which have no CMOS at all.
    #[serde(skip)]
    pub cpu: GswMode,
    pub memory_mib: u16,
    pub video: VideoCard,
    /// Retired: the TOKAEMM guest driver provides XMS/UMB/EMS from the default
    /// CONFIG.SYS. Accepted and ignored so older configuration files
    /// still parse; never written back.
    #[serde(default, skip_serializing)]
    pub emm386: Option<String>,
}

impl Default for MachineConfig {
    fn default() -> Self {
        Self {
            cpu: GswMode::Gsw586,
            memory_mib: 24, // Izarra 3000: 24 MB, 3 x 8 MB DIMMs
            video: VideoCard::Vega,
            emm386: None,
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
#[serde(default, deny_unknown_fields)]
pub struct SoundBlasterConfig {
    /// Whether the host constructs the SB16 audio path. Whether the card is
    /// fitted at all is a property of the machine, not a setting the guest can
    /// change, so unlike the resources below it stays in the config file.
    pub enabled: bool,
    /// Power-on IRQ line the CT1745 mixer selects (register 0x80).
    ///
    /// **Not read from the config file** — see [`RETIRED_CMOS_KEYS`]. The value
    /// here is the power-on default that seeds CMOS on a machine that has never
    /// been configured; after that, CMOS is what the machine boots with and
    /// `SNDCTRL.COM` is what changes it. `--sb-irq` still overrides this for
    /// headless runs, which have no CMOS at all.
    #[serde(skip)]
    pub irq: SbIrq,
    /// Power-on 8-bit DMA channel (register 0x81 low bits). Not read from the
    /// config file; see `irq`.
    #[serde(skip)]
    pub dma: SbDma8,
    /// Power-on 16-bit DMA channel (register 0x81 high bits). Not read from the
    /// config file; see `irq`.
    #[serde(skip)]
    pub high_dma: SbDma16,
}

impl Default for SoundBlasterConfig {
    fn default() -> Self {
        // IRQ7, not the SB16-era IRQ5: see the SbIrq doc for why 7 is the value
        // that satisfies both the titles that hardwire an IRQ and the ones that
        // read BLASTER. WssConfig yields IRQ11 so the two do not collide.
        Self {
            enabled: true,
            irq: SbIrq::I7,
            dma: SbDma8::D1,
            high_dma: SbDma16::D5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WssConfig {
    /// Whether the host constructs the Windows Sound System (AD1848 codec) path.
    /// The codec is always present on the ReSonique 2 combo card, so this defaults
    /// to enabled.
    pub enabled: bool,
    /// I/O base port of the four-port WSS config region (the AD1848 codec sits at
    /// base+4). Defaults to 0x530, the de-facto WSS standard base. Stays in the
    /// config file: the base is fixed board wiring, not a resource the guest can
    /// select, so nothing else can disagree with it.
    pub base: u16,
    /// Power-on IRQ line read back from the board config region. Defaults to IRQ11,
    /// leaving IRQ7 to the Sound Blaster, which many DOS titles hardwire.
    ///
    /// **Not read from the config file** — see [`RETIRED_CMOS_KEYS`] and
    /// [`SoundBlasterConfig::irq`].
    #[serde(skip)]
    pub irq: WssIrq,
    /// Power-on 8-bit DMA channel read back from the board config region. Defaults
    /// to DMA0, chosen to avoid the SB16 default (DMA1).
    ///
    /// **Not read from the config file** — see [`RETIRED_CMOS_KEYS`]. Note that
    /// the container-level `#[serde(default)]` is load-bearing here: this field's
    /// default has to come from `WssConfig::default()` (DMA0), not from
    /// `SbDma8::default()` (DMA1), which is the channel the Sound Blaster holds.
    #[serde(skip)]
    pub dma: SbDma8,
}

impl Default for WssConfig {
    fn default() -> Self {
        // base 0x530, IRQ11, DMA0 -- IRQ11 rather than the WSS standard IRQ7 so
        // the Sound Blaster keeps 7, which far more DOS titles hardwire; DMA0
        // avoids the SB16 default (DMA1).
        Self {
            enabled: true,
            base: 0x530,
            irq: WssIrq::I11,
            dma: SbDma8::D0,
        }
    }
}

/// Fixed I/O port ranges that a configurable device base (WSS, ...) must not
/// shadow. These mirror the fixed-port dispatch arms checked
/// ahead of the configurable-base decoders in `MachineBus::read_io`/`write_io`
/// (crates/izarravm-machine/src/lib.rs): the 8237 DMA controllers, PIT, PIC,
/// 8042 keyboard controller, RTC, Lotura system controller, IDE/ATA, Sound
/// Blaster + CT1745 mixer, LPT1/LPT2, COM1/COM2, OPL2/OPL3, and VGA/FDC. A
/// configurable window overlapping any of these would silently steal those
/// ports, with the winner decided by if-chain arm order rather than by the
/// config. Validating every configurable base against this table at load time
/// turns that into a load-time error instead.
const RESERVED_PORT_RANGES: &[(u16, u16)] = &[
    (0x0000, 0x001f), // 8237 DMA controller 1 + aliases
    (0x0020, 0x003f), // PIC 1
    (0x0040, 0x005f), // PIT
    (0x0060, 0x006f), // 8042 keyboard controller / system control ports
    (0x0070, 0x007f), // RTC / NMI mask
    (0x0080, 0x009f), // DMA page registers (covers port 0x92, system control A)
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

/// Whether the half-open port window `[a_start, a_end)` overlaps `[b_start, b_end)`.
const fn windows_overlap(a_start: u32, a_end: u32, b_start: u32, b_end: u32) -> bool {
    a_start < b_end && b_start < a_end
}

/// Whether the half-open port window `[start, end)` overlaps any fixed
/// chipset/device port range (see `RESERVED_PORT_RANGES`).
fn overlaps_reserved_range(start: u32, end: u32) -> bool {
    RESERVED_PORT_RANGES
        .iter()
        .any(|&(lo, hi)| windows_overlap(start, end, u32::from(lo), u32::from(hi) + 1))
}

impl WssConfig {
    /// The eight-port WSS window `[base, base + 8)`, saturating at 0xFFFF.
    pub const fn window(&self) -> (u16, u16) {
        (self.base, self.base.saturating_add(8))
    }

    /// Reject a `base` whose eight-port window overlaps any fixed chipset/device
    /// port range (see `RESERVED_PORT_RANGES`). The documented WSS bases (0x530,
    /// 0x604, 0xE80, 0xF40) all pass; a low or occupied base does not.
    pub fn validate_base(&self) -> Result<(), ConfigError> {
        let win_start = u32::from(self.base);
        let win_end = win_start + 8; // exclusive; cannot overflow u32
        if overlaps_reserved_range(win_start, win_end) {
            return Err(ConfigError::InvalidWssBase(
                self.base,
                self.base.saturating_add(8),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudioConfig {
    #[serde(default = "default_true", rename = "pc_speaker", skip_serializing)]
    _pc_speaker: bool,
    pub sound_blaster: SoundBlasterConfig,
    pub wss: WssConfig,
    #[serde(default = "default_true", rename = "opl3", skip_serializing)]
    _opl3: bool,
    /// Retired: the standalone Yamaha ADPCM-B DAC was removed once the SB16 DSP
    /// gained native Creative ADPCM. Accepted and ignored so conf files with an
    /// old `[audio.yamaha_adpcm]` section still parse; never written back.
    #[serde(default, skip_serializing)]
    pub yamaha_adpcm: RetiredAudioSection,
    pub midi: MidiConfig,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            _pc_speaker: true,
            sound_blaster: SoundBlasterConfig::default(),
            wss: WssConfig::default(),
            _opl3: true,
            yamaha_adpcm: RetiredAudioSection::default(),
            midi: MidiConfig::default(),
        }
    }
}

/// A config section that no longer maps to any device. It deserializes any
/// leftover keys and drops them (no `deny_unknown_fields`), so retiring a device
/// does not break older conf files. Never serialized back.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetiredAudioSection {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MidiConfig {
    /// Receiver for guest MIDI written to the MPU at 0x330.
    pub backend: MidiBackend,
    /// Exact host destination used when `backend` is External.
    pub external_port: Option<MidiPortId>,
    /// Optional bank for the FluidSynth wavetable fixed at 0x300.
    pub soundfont: Option<PathBuf>,
    pub mt32_control_rom: Option<PathBuf>,
    pub mt32_pcm_rom: Option<PathBuf>,
}

impl Default for MidiConfig {
    fn default() -> Self {
        Self {
            backend: MidiBackend::Off,
            external_port: None,
            soundfont: None,
            mt32_control_rom: None,
            mt32_pcm_rom: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MidiPortId {
    pub name: String,
    pub ordinal: u16,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MidiStatus {
    #[default]
    Ready,
    MissingPort,
    MissingSoundFont,
    MissingRoms,
    InitializationFailed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RetiredSteamInputMode {
    #[default]
    Off,
    OptionalBackend,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InputConfig {
    pub keyboard: bool,
    pub mouse: bool,
    pub joystick: bool,
    #[serde(default, rename = "steam_input", skip_serializing)]
    _steam_input: RetiredSteamInputMode,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            keyboard: true,
            mouse: true,
            joystick: true,
            _steam_input: RetiredSteamInputMode::Off,
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
    pub external_midi_port: Option<MidiPortId>,
    pub mt32_control_rom: Option<PathBuf>,
    pub mt32_pcm_rom: Option<PathBuf>,
    pub sb_irq: Option<SbIrq>,
    pub sb_dma: Option<SbDma8>,
    pub sb_high_dma: Option<SbDma16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareProfile {
    pub cpu: GswMode,
    pub memory_mib: u16,
    pub video: VideoCard,
    pub sound_blaster: SoundBlasterConfig,
    pub wss: WssConfig,
}

impl HardwareProfile {
    pub fn from_config(config: &AppConfig) -> Result<Self, ConfigError> {
        config.validate()?;

        Ok(Self {
            cpu: config.machine.cpu,
            memory_mib: config.machine.memory_mib,
            video: config.machine.video,
            sound_blaster: config.audio.sound_blaster,
            wss: config.audio.wss,
        })
    }
}

fn default_true() -> bool {
    true
}

// The per-field default helpers for the WSS base and DMA are gone: both
// sections now take their defaults from the container's `Default` impl, which
// is what a `#[serde(skip)]` field needs to inherit the right value (SbDma8's
// own default is DMA1, the Sound Blaster's channel, not the codec's DMA0).

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|character| !matches!(character, '-' | ' '))
        .collect::<String>()
        .to_ascii_lowercase()
}

#[cfg(test)]
#[path = "core_test.rs"]
mod tests;
