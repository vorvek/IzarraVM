// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use crate::{Cli, cmos, host_input::HostInputPolicy, prefs};
use izarravm_core::{
    AppConfig, ConfigError, ConfigOverrides, HardwareProfile, MidiConfig, MidiPortId,
};
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
    let mut value = toml::from_str::<toml::Value>(&text).map_err(|source| ConfigError::Parse {
        path: path.to_owned(),
        source: Box::new(source),
    })?;
    if let Some(key) = izarravm_core::gui_prefs_marker(&value) {
        return Err(ConfigError::GuiPrefsGivenAsMachineConfig {
            path: path.to_owned(),
            key,
        });
    }
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
    // Drop the settings CMOS owns now, and say so: a user who edited one of
    // them is otherwise left watching the machine ignore their file, which is
    // exactly the confusion that moving them to CMOS was meant to end.
    let dropped = izarravm_core::strip_retired_keys(&mut value);
    if !dropped.is_empty() {
        warn!(
            path = %path.display(),
            keys = %dropped.join(", "),
            "ignoring config keys the machine's CMOS now owns; \
             set the CPU speed in the BIOS setup panel (Del) and the sound \
             card's resources with SNDCTRL in DOS"
        );
    }
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

/// Folders under the state directory a ROM set is conventionally dropped into.
/// Matched case-insensitively.
const MUNT_ROM_FOLDERS: &[&str] = &["mt32", "cm32l", "roms", "mt32-roms"];

/// Find an MT-32 ROM set the user has dropped into their state directory, when
/// they have not named one themselves.
///
/// Two layouts, and neither depends on the files being NAMED anything: the
/// canonical pair sitting loose in the state directory, or a folder that looks
/// like it holds a ROM set. A folder hint is handed to the loader whole, which
/// is what lets it identify the images by content and merge split halves --
/// the pair of names below is only how a loose set is recognised as one.
fn discover_munt_roms(config: &mut MidiConfig, state_dir: &Path) {
    if config.mt32_control_rom.is_some() || config.mt32_pcm_rom.is_some() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(state_dir) else {
        return;
    };
    let (files, folders): (Vec<PathBuf>, Vec<PathBuf>) = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() || path.is_dir())
        .partition(|path| path.is_file());
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
    // A ROM-set folder, both hints pointing at it: the loader takes it from
    // there, whatever the files inside are called.
    let mut candidates: Vec<&PathBuf> = folders
        .iter()
        .filter(|path| {
            path.file_name().is_some_and(|name| {
                let name = name.to_string_lossy();
                MUNT_ROM_FOLDERS
                    .iter()
                    .any(|known| name.eq_ignore_ascii_case(known))
            })
        })
        .collect();
    candidates.sort();
    if let Some(folder) = candidates.first() {
        config.mt32_control_rom = Some((*folder).clone());
        config.mt32_pcm_rom = Some((*folder).clone());
    }
}

#[derive(Debug)]
pub(super) struct ResolvedStartup {
    config: AppConfig,
    hardware: HardwareProfile,
    host_input: HostInputPolicy,
    /// The CMOS-owned hardware settings a flag asked for this run, kept apart
    /// from `config` so the CMOS load can tell "the user typed this" from "this
    /// is the built-in default".
    requested: cmos::RequestedHardware,
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
    pub(super) host_input: HostInputPolicy,
    pub(super) prefs: prefs::GuiPrefs,
    pub(super) prefs_path: PathBuf,
}

pub(super) struct StartupError(ConfigError);

impl fmt::Display for StartupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Deliberately not derived. `fn main() -> Result<_, E>` reports the error with
/// `Debug`, so a derived one prints the struct and hides the message -- and
/// every one of these messages exists to tell the user which file to fix.
impl fmt::Debug for StartupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Explicitly Display: an unqualified `self.0.fmt` inside a Debug impl
        // resolves to Debug and prints the struct, which is the whole problem.
        fmt::Display::fmt(&self.0, formatter)
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

    /// The folder the GUI would mount as C:, after --c-drive, the config file,
    /// and the portable/per-user default have all been applied. The boot
    /// profiler mounts exactly this, so its numbers describe the machine the
    /// user actually runs rather than a stand-in for it.
    pub(super) fn c_drive(&self) -> &Path {
        &self.config.dos.c_drive
    }

    pub(super) fn load_global_glide_ovl(&self) -> Option<Vec<u8>> {
        load_state_glide_ovl(&self.state_dir)
    }

    pub(super) fn into_gui(self, rom: Vec<u8>, test_pattern: bool) -> GuiLaunch {
        let mut rtc_setup = cmos::RtcSetup::from_c_root(&self.config.dos.c_drive);
        rtc_setup.requested = self.requested;
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
            host_input: self.host_input,
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

/// The C: root the command line asks for: `--c-drive` wins, then `--dosroot`.
///
/// An EMPTY `--dosroot` (which is what a stray `IZARRAVM_DOSROOT=` in the environment produces)
/// is absent, NOT a root at the current directory. The value parser accepts the empty string
/// only so that stray variable cannot fail every invocation at parse time; this is where it is
/// dropped.
pub(crate) fn c_drive_override(cli: &Cli) -> Option<PathBuf> {
    cli.c_drive.clone().or_else(|| {
        cli.dosroot
            .clone()
            .filter(|path| !path.as_os_str().is_empty())
    })
}

fn resolve_with(
    cli: &Cli,
    locations: &StartupLocations,
    mut read_text: impl FnMut(&Path) -> io::Result<String>,
) -> Result<ResolvedStartup, StartupError> {
    let (mut config, presence) = load_config_snapshot(cli.config.as_deref(), &mut read_text)?;
    let c_drive = c_drive_override(cli);
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
    let host_input = HostInputPolicy::from_config(&config.input);
    let requested = cmos::RequestedHardware {
        cpu: cli.cpu,
        sb_irq: cli.sb_irq,
        sb_dma: cli.sb_dma,
        sb_high_dma: cli.sb_high_dma,
    };
    info!(
        cpu = %config.machine.cpu,
        hz = hardware.cpu.clock_rate().as_hz_f64(),
        memory_mib = config.machine.memory_mib,
        video = %config.machine.video,
        c_drive = %config.dos.c_drive.display(),
        sound_blaster = config.audio.sound_blaster.enabled,
        wss = config.audio.wss.enabled,
        midi = %config.audio.midi.backend,
        keyboard = host_input.keyboard_enabled(),
        mouse = host_input.mouse_enabled(),
        joystick = host_input.joystick_enabled(),
        "configuration validated"
    );
    Ok(ResolvedStartup {
        config,
        hardware,
        host_input,
        requested,
        prefs: saved_prefs,
        prefs_path,
        state_dir: locations.state_dir.clone(),
    })
}

#[cfg(test)]
#[path = "startup_test.rs"]
mod tests;
