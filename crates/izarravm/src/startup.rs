// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use crate::{Cli, cmos, prefs};
use izarravm_audio::AudioSubsystem;
use izarravm_core::{
    AppConfig, ConfigError, ConfigOverrides, HardwareProfile, MidiConfig, MidiPortId,
};
use izarravm_input::InputState;
use izarravm_machine::MachineProfile;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

#[derive(Debug, Clone)]
struct StartupLocations {
    state_dir: PathBuf,
    executable_dir: PathBuf,
}

impl StartupLocations {
    fn from_host() -> Self {
        let executable_dir = std::env::current_exe()
            .ok()
            .and_then(|executable| executable.parent().map(Path::to_path_buf))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        #[allow(deprecated)]
        let state_dir = std::env::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".izarravm");
        Self {
            state_dir,
            executable_dir,
        }
    }

    fn default_c_root(&self, portable: bool) -> PathBuf {
        if portable {
            self.executable_dir.join("c_drive")
        } else {
            self.state_dir.join("c_drive")
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ConfigPresence {
    c_drive: bool,
    midi: MidiConfigPresence,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct MidiConfigPresence {
    backend: bool,
    external_port: bool,
    soundfont: bool,
    mt32_control_rom: bool,
    mt32_pcm_rom: bool,
}

fn load_config_snapshot(
    path: Option<&Path>,
    read_text: &mut impl FnMut(&Path) -> io::Result<String>,
) -> Result<(AppConfig, ConfigPresence), ConfigError> {
    let Some(path) = path else {
        return Ok((AppConfig::default(), ConfigPresence::default()));
    };
    let text = read_text(path).map_err(|source| ConfigError::Read {
        path: path.to_owned(),
        source,
    })?;
    let value = toml::from_str::<toml::Value>(&text).map_err(|source| ConfigError::Parse {
        path: path.to_owned(),
        source: Box::new(source),
    })?;
    let midi = value
        .get("audio")
        .and_then(|audio| audio.get("midi"))
        .and_then(toml::Value::as_table);
    let presence = ConfigPresence {
        c_drive: value
            .get("dos")
            .and_then(|dos| dos.get("c_drive"))
            .is_some(),
        midi: MidiConfigPresence {
            backend: midi.is_some_and(|table| table.contains_key("backend")),
            external_port: midi.is_some_and(|table| table.contains_key("external_port")),
            soundfont: midi.is_some_and(|table| table.contains_key("soundfont")),
            mt32_control_rom: midi.is_some_and(|table| table.contains_key("mt32_control_rom")),
            mt32_pcm_rom: midi.is_some_and(|table| table.contains_key("mt32_pcm_rom")),
        },
    };
    let config = value
        .try_into::<AppConfig>()
        .map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
            source: Box::new(source),
        })?;
    Ok((config, presence))
}

fn merge_saved_midi(config: &mut MidiConfig, saved: &MidiConfig, presence: MidiConfigPresence) {
    if !presence.backend {
        config.backend = saved.backend;
    }
    if !presence.external_port {
        config.external_port.clone_from(&saved.external_port);
    }
    if !presence.soundfont {
        config.soundfont.clone_from(&saved.soundfont);
    }
    if !presence.mt32_control_rom {
        config.mt32_control_rom.clone_from(&saved.mt32_control_rom);
    }
    if !presence.mt32_pcm_rom {
        config.mt32_pcm_rom.clone_from(&saved.mt32_pcm_rom);
    }
}

fn discover_munt_roms(config: &mut MidiConfig, state_dir: &Path) {
    if config.mt32_control_rom.is_some() || config.mt32_pcm_rom.is_some() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(state_dir) else {
        return;
    };
    let files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    let named = |name: &str| {
        files.iter().find(|path| {
            path.file_name()
                .is_some_and(|file| file.to_string_lossy().eq_ignore_ascii_case(name))
        })
    };
    for (control_name, pcm_name) in [
        ("MT32_CONTROL.ROM", "MT32_PCM.ROM"),
        ("CM32L_CONTROL.ROM", "CM32L_PCM.ROM"),
    ] {
        if let (Some(control), Some(pcm)) = (named(control_name), named(pcm_name)) {
            config.mt32_control_rom = Some(control.clone());
            config.mt32_pcm_rom = Some(pcm.clone());
            return;
        }
    }
}

#[derive(Debug)]
pub(super) struct ResolvedStartup {
    config: AppConfig,
    hardware: HardwareProfile,
    prefs: prefs::GuiPrefs,
    prefs_path: PathBuf,
    state_dir: PathBuf,
}

pub(super) struct GuiLaunch {
    pub(super) profile: MachineProfile,
    pub(super) rom: Vec<u8>,
    pub(super) c_drive: PathBuf,
    pub(super) cd_image: Option<PathBuf>,
    pub(super) midi_config: MidiConfig,
    pub(super) glide_ovl: Option<Vec<u8>>,
    pub(super) test_pattern: bool,
    pub(super) rtc_setup: cmos::RtcSetup,
    pub(super) joystick_enabled: bool,
    pub(super) prefs: prefs::GuiPrefs,
    pub(super) prefs_path: PathBuf,
}

#[derive(Debug)]
pub(super) struct StartupError(ConfigError);

impl fmt::Display for StartupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for StartupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

impl From<ConfigError> for StartupError {
    fn from(error: ConfigError) -> Self {
        Self(error)
    }
}

impl ResolvedStartup {
    pub(super) fn from_cli(cli: &Cli) -> Result<Self, StartupError> {
        resolve_with(cli, &StartupLocations::from_host(), |path| {
            std::fs::read_to_string(path)
        })
    }

    pub(super) fn hardware(&self) -> &HardwareProfile {
        &self.hardware
    }

    pub(super) fn load_global_glide_ovl(&self) -> Option<Vec<u8>> {
        load_state_glide_ovl(&self.state_dir)
    }

    pub(super) fn into_gui(self, rom: Vec<u8>, test_pattern: bool) -> GuiLaunch {
        let rtc_setup = cmos::RtcSetup::from_c_root(&self.config.dos.c_drive);
        let glide_ovl = self.load_global_glide_ovl();
        GuiLaunch {
            profile: MachineProfile::from_hardware_profile(&self.hardware),
            rom,
            c_drive: self.config.dos.c_drive,
            cd_image: self.config.dos.cd_image,
            midi_config: self.config.audio.midi,
            glide_ovl,
            test_pattern,
            rtc_setup,
            joystick_enabled: self.config.input.joystick,
            prefs: self.prefs,
            prefs_path: self.prefs_path,
        }
    }
}

fn load_state_glide_ovl(state_dir: &Path) -> Option<Vec<u8>> {
    let canonical = state_dir.join("GLIDE2X.OVL");
    let path = canonical.is_file().then_some(canonical).or_else(|| {
        std::fs::read_dir(state_dir)
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file()
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.eq_ignore_ascii_case("GLIDE2X.OVL"))
            })
            .min()
    })?;
    match std::fs::read(&path) {
        Ok(bytes) => {
            info!(path = %path.display(), "using global GLIDE2X.OVL fallback");
            Some(bytes)
        }
        Err(error) => {
            warn!(%error, path = %path.display(), "could not read global GLIDE2X.OVL fallback");
            None
        }
    }
}

fn resolve_with(
    cli: &Cli,
    locations: &StartupLocations,
    mut read_text: impl FnMut(&Path) -> io::Result<String>,
) -> Result<ResolvedStartup, StartupError> {
    let (mut config, presence) = load_config_snapshot(cli.config.as_deref(), &mut read_text)?;
    let c_drive = cli.c_drive.clone().or_else(|| cli.dosroot.clone());
    let uses_default_c_root = c_drive.is_none() && !presence.c_drive;
    let external_midi_port = cli.midi_port.as_ref().map(|name| MidiPortId {
        name: name.clone(),
        ordinal: cli.midi_port_ordinal.unwrap_or(0),
    });
    config.apply_overrides(ConfigOverrides {
        cpu: cli.cpu,
        memory_mib: cli.memory_mib,
        video: cli.video,
        c_drive,
        soundfont: cli.soundfont.clone(),
        midi_backend: cli.midi_backend,
        external_midi_port,
        mt32_control_rom: cli.mt32_control_rom.clone(),
        mt32_pcm_rom: cli.mt32_pcm_rom.clone(),
        sb_irq: cli.sb_irq,
        sb_dma: cli.sb_dma,
        sb_high_dma: cli.sb_high_dma,
    });
    if uses_default_c_root {
        config.dos.c_drive = locations.default_c_root(cli.portable);
        let _ = std::fs::create_dir_all(&config.dos.c_drive);
    }
    let prefs_path = prefs::prefs_path(&config.dos.c_drive);
    let saved_prefs = prefs::GuiPrefs::load_with(&prefs_path, &mut read_text);
    let mut midi_presence = presence.midi;
    midi_presence.backend |= cli.midi_backend.is_some();
    midi_presence.external_port |= cli.midi_port.is_some();
    midi_presence.soundfont |= cli.soundfont.is_some();
    midi_presence.mt32_control_rom |= cli.mt32_control_rom.is_some();
    midi_presence.mt32_pcm_rom |= cli.mt32_pcm_rom.is_some();
    merge_saved_midi(&mut config.audio.midi, &saved_prefs.midi, midi_presence);
    if !cli.portable {
        discover_munt_roms(&mut config.audio.midi, &locations.state_dir);
    }
    let hardware = HardwareProfile::from_config(&config)?;
    let audio = AudioSubsystem::from_config(&config.audio);
    let input = InputState {
        keyboard_enabled: config.input.keyboard,
        mouse_enabled: config.input.mouse,
        joystick_enabled: config.input.joystick,
    };
    info!(
        cpu = %config.machine.cpu,
        hz = hardware.cpu.clock_rate().as_hz_f64(),
        memory_mib = config.machine.memory_mib,
        video = %config.machine.video,
        c_drive = %config.dos.c_drive.display(),
        audio_devices = audio.devices.len(),
        keyboard = input.keyboard_enabled,
        mouse = input.mouse_enabled,
        joystick = input.joystick_enabled,
        "configuration validated"
    );
    Ok(ResolvedStartup {
        config,
        hardware,
        prefs: saved_prefs,
        prefs_path,
        state_dir: locations.state_dir.clone(),
    })
}

#[cfg(test)]
#[path = "startup_test.rs"]
mod tests;
