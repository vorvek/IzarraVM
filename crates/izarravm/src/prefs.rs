// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! GUI preferences, persisted as a small `izarravm.conf` TOML file next to the
//! C: root (in the directory that contains the c_drive folder). This is separate
//! from `AppConfig`: it holds host-side GUI state (master volume, last mounts)
//! that the machine config has no place for.
//!
//! Every load and save is best-effort: an IO or parse error logs a warning and
//! falls back to defaults rather than aborting the run.

use izarravm_core::MidiConfig;
use izarravm_input::{ControllerConfig, JoystickBinding};
use serde::{Deserialize, Deserializer, Serialize};
use std::path::{Path, PathBuf};
use tracing::warn;

/// File name for the GUI prefs, written next to the C: root.
const PREFS_FILE: &str = "izarravm.conf";

/// Default master volume. 0.8 sits comfortably below clipping for most
/// material while still being plainly audible.
const DEFAULT_VOLUME: f32 = 0.8;

/// Highest accepted `master_volume`: five times line level, +14 dB.
///
/// The knob is the host's powered speakers, and a speaker knob has travel past
/// line level. The figure is the worst well-behaved case measured back: a title
/// whose own mixer is maxed still writes a CT1745 output level of 27 (-8 dB) and
/// the mix reserves a further -6 dB of headroom, so its peaks arrive 14 dB below
/// full scale. Five times puts them back at it.
///
/// Widening the accepted range retires nothing. The 0..1 values older builds
/// wrote are inside it and keep their meaning, so an existing `izarravm.conf`
/// loads to the same level it always did.
pub const MAX_VOLUME: f32 = 5.0;

/// GUI preference keys that are no longer read or written.
///
/// All three named a level INSIDE the machine's audio chain, and the machine's
/// chain belongs to the guest: the ReSonique II's output stage and the PC
/// speaker's leg are CT1745 registers, set from DOS with SNDMIXER.COM and read
/// back by any program that asks. A second copy of those controls in the host
/// GUI could not be seen by the guest, was not saved with the machine, and had
/// to be kept in step with a mixer that had every right to disagree with it.
/// The host's own level -- the volume knob, standing for the powered speakers
/// the line-out feeds -- is `master_volume`, and it stays.
///
/// They are IGNORED, not rejected, and never written back: an existing
/// `izarravm.conf` keeps loading and quietly loses them on the next save. One
/// warning names whichever were present, the same courtesy `strip_retired_keys`
/// extends to the config keys CMOS took over. `amp_gain` was already retired
/// once (it was `output_gain`'s old spelling); it is listed here so a file old
/// enough to carry it gets the same one line about it.
///
/// No value is carried over into anything. There is no rescale that is right
/// for both a default nobody chose and a level a user picked by ear, and this
/// is prerelease: the honest move is to let the machine's own mixer be the
/// answer from here.
const RETIRED_KEYS: &[&str] = &["amp_gain", "output_gain", "pc_speaker_volume"];

/// What a label calls the Super key. Windows names it after itself, and a
/// Windows user looks for that name on the key cap; the Linux desktops name it
/// Super. Only the label changes. The field, the `super` key in
/// `izarravm.conf`, and the matcher are the same everywhere, so a preferences
/// file stays portable between the two hosts.
#[cfg(windows)]
pub const SUPER_KEY_NAME: &str = "Win";
#[cfg(not(windows))]
pub const SUPER_KEY_NAME: &str = "Super";

/// A host hotkey: modifier flags plus a key name. `key` is the winit `KeyCode`
/// debug name (e.g. "F2", "KeyA"), which the GUI compares against the live key
/// and renders prettily. Kept winit-free so prefs stays plain data.
///
/// `super_key` is the Super key. See `SUPER_KEY_NAME` for the name the label
/// gives it. A file written before it became a modifier has no `super` in the
/// table, so the field carries `serde(default)`: without it one missing key
/// fails the whole parse and every preference falls back to its default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyBinding {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    #[serde(rename = "super", default)]
    pub super_key: bool,
    pub key: String,
}

impl KeyBinding {
    pub fn new(ctrl: bool, shift: bool, alt: bool, super_key: bool, key: &str) -> Self {
        Self {
            ctrl,
            shift,
            alt,
            super_key,
            key: key.to_string(),
        }
    }

    /// True when the live key name and modifier state match this binding.
    pub fn matches(&self, key: &str, ctrl: bool, shift: bool, alt: bool, super_key: bool) -> bool {
        self.ctrl == ctrl
            && self.shift == shift
            && self.alt == alt
            && self.super_key == super_key
            && self.key == key
    }

    /// Human label like "Win+F2" on Windows and "Super+F2" on Linux. Strips the
    /// winit "Key"/"Digit" prefixes so a letter or number reads naturally.
    pub fn display(&self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push_str("Ctrl+");
        }
        if self.shift {
            s.push_str("Shift+");
        }
        if self.alt {
            s.push_str("Alt+");
        }
        if self.super_key {
            s.push_str(SUPER_KEY_NAME);
            s.push('+');
        }
        let key = self
            .key
            .strip_prefix("Key")
            .or_else(|| self.key.strip_prefix("Digit"))
            .unwrap_or(&self.key);
        s.push_str(key);
        s
    }
}

/// The hotkey that releases captured input.
fn default_input_release() -> KeyBinding {
    KeyBinding::new(false, false, false, true, "F2")
}

/// The hotkey that toggles fullscreen.
fn default_fullscreen() -> KeyBinding {
    KeyBinding::new(false, false, false, true, "F4")
}

/// The two defaults these replaced, as (retired, current) pairs. A file written
/// before Super was a modifier carries the retired combination, which nobody
/// picked: it was whatever the build shipped. `migrate_retired_hotkeys` moves
/// such a binding to the current default, so a change of default reaches an
/// existing installation. A user who chose the retired combination by hand
/// loses it once and rebinds it in the config modal.
fn retired_hotkey_defaults() -> [(KeyBinding, KeyBinding); 2] {
    [
        (
            KeyBinding::new(true, false, false, false, "F2"),
            default_input_release(),
        ),
        (
            KeyBinding::new(true, false, false, false, "F11"),
            default_fullscreen(),
        ),
    ]
}

/// CRT presentation style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CrtStyle {
    /// No CRT pass: plain aspect-corrected output.
    Off,
    /// The default subtle high-res SVGA look.
    #[default]
    Subtle,
    /// Heavier "Ye Olde Screene": visible scanlines + shadow mask, curvature,
    /// softer focus, and faint animated grain.
    YeOlde,
}

impl CrtStyle {
    /// Shader style selector: 0 off, 1 subtle, 2 Ye Olde.
    pub fn as_u32(self) -> u32 {
        match self {
            CrtStyle::Off => 0,
            CrtStyle::Subtle => 1,
            CrtStyle::YeOlde => 2,
        }
    }
}

/// Host-side GUI preferences. Fields are optional where a "not set yet" state is
/// meaningful, so an older or hand-edited file with missing keys still loads.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GuiPrefs {
    /// Master output volume, 0.0..[`MAX_VOLUME`]. The HOST's level: the powered
    /// speakers the machine's line-out feeds, applied to the finished mix on its
    /// way to the sound device. 1.0 is unity and anything above it amplifies.
    /// Every level inside the machine is the guest's, on the card's own
    /// registers (see `RETIRED_KEYS`).
    pub master_volume: f32,
    /// CRT presentation style: off, subtle (default), or Ye Olde Screene.
    pub crt_style: CrtStyle,
    /// Hotkey that releases captured input. Default Super+F2.
    pub input_release: KeyBinding,
    /// Hotkey that toggles fullscreen. Default Super+F4.
    pub fullscreen: KeyBinding,
    /// Host controller mappings for guest keys and the Izarra gameport.
    pub controller: Option<ControllerConfig>,
    /// Last floppy IMG mounted, re-mounted on startup if it still exists.
    pub last_floppy_image: Option<PathBuf>,
    /// Last CD image (.iso/.cue/.bin) mounted, re-mounted on startup if it still
    /// exists. A config-file `cd_image` takes priority when both are present.
    pub last_cd_image: Option<PathBuf>,
    /// Last host folder mounted as a CD, re-mounted (rebuilt) on startup if it
    /// still exists. Mutually exclusive with `last_cd_image`: setting one
    /// clears the other, since the CD drive holds one medium at a time.
    pub last_cd_folder: Option<PathBuf>,
    /// Whether the beige control panel is expanded. Persisted so the collapse
    /// state survives a restart. Defaults to open.
    pub panel_open: bool,
    /// P330 receiver and P300 SoundFont selected in the configuration dialog.
    pub midi: MidiConfig,
}

impl Default for GuiPrefs {
    fn default() -> Self {
        Self {
            master_volume: DEFAULT_VOLUME,
            crt_style: CrtStyle::Subtle,
            input_release: default_input_release(),
            fullscreen: default_fullscreen(),
            controller: None,
            last_floppy_image: None,
            last_cd_image: None,
            last_cd_folder: None,
            panel_open: true,
            midi: MidiConfig::default(),
        }
    }
}

#[derive(Deserialize)]
#[serde(default)]
struct GuiPrefsWire {
    master_volume: f32,
    crt_style: CrtStyle,
    input_release: KeyBinding,
    fullscreen: KeyBinding,
    controller: Option<ControllerConfig>,
    joystick_binding: Option<JoystickBinding>,
    last_floppy_image: Option<PathBuf>,
    last_cd_image: Option<PathBuf>,
    last_cd_folder: Option<PathBuf>,
    panel_open: bool,
    midi: MidiConfig,
}

impl Default for GuiPrefsWire {
    fn default() -> Self {
        let prefs = GuiPrefs::default();
        Self {
            master_volume: prefs.master_volume,
            crt_style: prefs.crt_style,
            input_release: prefs.input_release,
            fullscreen: prefs.fullscreen,
            controller: None,
            joystick_binding: None,
            last_floppy_image: prefs.last_floppy_image,
            last_cd_image: prefs.last_cd_image,
            last_cd_folder: prefs.last_cd_folder,
            panel_open: prefs.panel_open,
            midi: prefs.midi,
        }
    }
}

impl<'de> Deserialize<'de> for GuiPrefs {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = GuiPrefsWire::deserialize(deserializer)?;
        Ok(Self {
            master_volume: wire.master_volume,
            crt_style: wire.crt_style,
            input_release: wire.input_release,
            fullscreen: wire.fullscreen,
            controller: wire
                .controller
                .or_else(|| wire.joystick_binding.map(ControllerConfig::from_legacy)),
            last_floppy_image: wire.last_floppy_image,
            last_cd_image: wire.last_cd_image,
            last_cd_folder: wire.last_cd_folder,
            panel_open: wire.panel_open,
            midi: wire.midi,
        })
    }
}

/// Resolve the prefs file path from the C: root: the file sits in the directory
/// that contains the c_drive folder, so it survives alongside cmos.bin and is
/// shared by both the portable and home-directory C: layouts.
pub fn prefs_path(c_root: &Path) -> PathBuf {
    let dir = c_root.parent().unwrap_or(c_root);
    dir.join(PREFS_FILE)
}

impl GuiPrefs {
    /// A missing, unreadable, or unparseable file yields defaults.
    pub(super) fn load_with(
        path: &Path,
        mut read_text: impl FnMut(&Path) -> std::io::Result<String>,
    ) -> Self {
        let text = match read_text(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(err) => {
                warn!(%err, path = %path.display(), "could not read izarravm.conf; using defaults");
                return Self::default();
            }
        };
        // Parse to a document first so the retired key can be SEEN. Unknown
        // fields are dropped silently by the struct deserializer, which is what
        // lets an older file keep loading, but a knob whose meaning changed
        // deserves one line saying its value was not carried over.
        let value = match toml::from_str::<toml::Value>(&text) {
            Ok(value) => value,
            Err(err) => {
                warn!(%err, path = %path.display(), "could not parse izarravm.conf; using defaults");
                return Self::default();
            }
        };
        let retired = retired_keys_present(&value);
        if !retired.is_empty() {
            warn!(
                path = %path.display(),
                keys = %retired.join(", "),
                "ignoring retired keys in izarravm.conf; the machine's own mixer owns those \
                 levels now -- set them in DOS with SNDMIXER, and use the panel's volume knob \
                 for the host speakers"
            );
        }
        match value.try_into::<Self>() {
            Ok(mut prefs) => {
                prefs.master_volume = prefs.master_volume.clamp(0.0, MAX_VOLUME);
                prefs.migrate_retired_hotkeys(path);
                prefs
            }
            Err(err) => {
                warn!(%err, path = %path.display(), "could not parse izarravm.conf; using defaults");
                Self::default()
            }
        }
    }

    /// Move a hotkey that still holds a retired default to the current default.
    /// See `retired_hotkey_defaults`. One log line names each move, because the
    /// next save writes the new combination to the file.
    fn migrate_retired_hotkeys(&mut self, path: &Path) {
        let [release, fullscreen] = retired_hotkey_defaults();
        let slots = [
            (&mut self.input_release, release),
            (&mut self.fullscreen, fullscreen),
        ];
        for (binding, (retired, current)) in slots {
            if *binding == retired {
                warn!(
                    path = %path.display(),
                    from = %retired.display(),
                    to = %current.display(),
                    "moving a hotkey off a retired default in izarravm.conf; \
                     set another combination in the config modal if you want one"
                );
                *binding = current;
            }
        }
    }

    /// Write the prefs to `path`. A serialize or IO failure logs a warning and is
    /// otherwise ignored: losing a prefs write is not worth interrupting the run.
    pub fn save(&self, path: &Path) {
        let text = match toml::to_string_pretty(self) {
            Ok(text) => text,
            Err(err) => {
                warn!(%err, "could not serialize izarravm.conf");
                return;
            }
        };
        if let Err(err) = std::fs::write(path, text) {
            warn!(%err, path = %path.display(), "could not write izarravm.conf");
        }
    }
}

/// Which [`RETIRED_KEYS`] a parsed prefs document still carries.
///
/// The struct deserializer drops unknown fields silently -- that is what lets an
/// older file keep loading -- so the document has to be inspected before it is
/// deserialized or a retired key leaves no trace at all.
fn retired_keys_present(value: &toml::Value) -> Vec<&'static str> {
    let Some(table) = value.as_table() else {
        return Vec::new();
    };
    RETIRED_KEYS
        .iter()
        .filter(|key| table.contains_key(**key))
        .copied()
        .collect()
}

#[cfg(test)]
#[path = "prefs_test.rs"]
mod tests;
