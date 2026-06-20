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
}

impl Default for MachineConfig {
    fn default() -> Self {
        Self {
            cpu: GswMode::Gsw386,
            memory_mib: 24, // Izarra 3000: 24 MB, 3 x 8 MB DIMMs
            video: VideoCard::Et4000Ax,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudioConfig {
    pub pc_speaker: bool,
    pub sound_blaster: SoundBlasterConfig,
    pub opl3: bool,
    pub midi: MidiConfig,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            pc_speaker: true,
            sound_blaster: SoundBlasterConfig::default(),
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
        })
    }
}

fn default_true() -> bool {
    true
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
