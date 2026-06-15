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
pub enum CpuPreset {
    #[serde(rename = "i386dx_25")]
    #[default]
    I386Dx25,
    #[serde(rename = "i486dx2_66")]
    I486Dx2_66,
    #[serde(rename = "pentium_133")]
    Pentium133,
    #[serde(rename = "pentium_mmx_233")]
    PentiumMmx233,
}

impl CpuPreset {
    pub const fn clock_hz(self) -> u64 {
        match self {
            Self::I386Dx25 => 25_000_000,
            Self::I486Dx2_66 => 66_000_000,
            Self::Pentium133 => 133_000_000,
            Self::PentiumMmx233 => 233_000_000,
        }
    }

    pub const fn canonical_name(self) -> &'static str {
        match self {
            Self::I386Dx25 => "i386dx_25",
            Self::I486Dx2_66 => "i486dx2_66",
            Self::Pentium133 => "pentium_133",
            Self::PentiumMmx233 => "pentium_mmx_233",
        }
    }
}

impl fmt::Display for CpuPreset {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.canonical_name())
    }
}

impl FromStr for CpuPreset {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match normalize(value).as_str() {
            "i386dx25" | "i386dx_25" | "386dx25" | "386_25" => Ok(Self::I386Dx25),
            "i486dx266" | "i486dx2_66" | "486dx266" | "486dx2_66" => Ok(Self::I486Dx2_66),
            "pentium133" | "pentium_133" | "p133" => Ok(Self::Pentium133),
            "pentiummmx233" | "pentium_mmx_233" | "pmmx233" => Ok(Self::PentiumMmx233),
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
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MachineConfig {
    pub cpu: CpuPreset,
    pub memory_mib: u16,
    pub video: VideoCard,
}

impl Default for MachineConfig {
    fn default() -> Self {
        Self {
            cpu: CpuPreset::I386Dx25,
            memory_mib: 16,
            video: VideoCard::Et4000Ax,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DosConfig {
    pub c_drive: PathBuf,
}

impl Default for DosConfig {
    fn default() -> Self {
        Self {
            c_drive: PathBuf::from("."),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudioConfig {
    pub pc_speaker: bool,
    pub sound_blaster: bool,
    pub opl3: bool,
    pub midi: MidiConfig,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            pc_speaker: true,
            sound_blaster: true,
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
    pub cpu: Option<CpuPreset>,
    pub memory_mib: Option<u16>,
    pub video: Option<VideoCard>,
    pub c_drive: Option<PathBuf>,
    pub soundfont: Option<PathBuf>,
    pub midi_backend: Option<MidiBackend>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareProfile {
    pub cpu: CpuPreset,
    pub clock_hz: u64,
    pub memory_mib: u16,
    pub video: VideoCard,
}

impl HardwareProfile {
    pub fn from_config(config: &MachineConfig) -> Result<Self, ConfigError> {
        AppConfig {
            machine: config.clone(),
            ..AppConfig::default()
        }
        .validate()?;

        Ok(Self {
            cpu: config.cpu,
            clock_hz: config.cpu.clock_hz(),
            memory_mib: config.memory_mib,
            video: config.video,
        })
    }
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
    fn cpu_presets_parse_common_aliases() {
        assert_eq!("386dx25".parse::<CpuPreset>().unwrap(), CpuPreset::I386Dx25);
        assert_eq!(
            "486dx2-66".parse::<CpuPreset>().unwrap(),
            CpuPreset::I486Dx2_66
        );
        assert_eq!(
            "pentium_mmx_233".parse::<CpuPreset>().unwrap(),
            CpuPreset::PentiumMmx233
        );
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
            cpu: Some(CpuPreset::Pentium133),
            memory_mib: Some(32),
            video: Some(VideoCard::S3VirgeDx),
            c_drive: Some(PathBuf::from("games")),
            soundfont: Some(PathBuf::from("gm.sf2")),
            midi_backend: Some(MidiBackend::FluidSynth),
        });

        assert_eq!(config.machine.cpu, CpuPreset::Pentium133);
        assert_eq!(config.machine.memory_mib, 32);
        assert_eq!(config.machine.video, VideoCard::S3VirgeDx);
        assert_eq!(config.dos.c_drive, PathBuf::from("games"));
        assert_eq!(config.audio.midi.soundfont, Some(PathBuf::from("gm.sf2")));
        assert_eq!(config.audio.midi.backend, MidiBackend::FluidSynth);
    }

    #[test]
    fn loads_toml_config() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("izarravm.toml");
        fs::write(
            &path,
            r#"
                [machine]
                cpu = "i386dx_25"
                memory_mib = 16
                video = "et4000_ax"
            "#,
        )
        .unwrap();

        let config = AppConfig::from_toml_path(path).unwrap();
        assert_eq!(config.machine.cpu, CpuPreset::I386Dx25);
        assert_eq!(config.machine.video, VideoCard::Et4000Ax);
        assert_eq!(config.dos.c_drive, PathBuf::from("."));
    }
}
