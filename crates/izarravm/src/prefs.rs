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
use izarravm_input::JoystickBinding;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::warn;

/// File name for the GUI prefs, written next to the C: root.
const PREFS_FILE: &str = "izarravm.conf";

/// Default master volume (0..1). 0.8 sits comfortably below clipping for most
/// material while still being plainly audible.
const DEFAULT_VOLUME: f32 = 0.8;

/// Default ReSonique 2 output amp gain, in tenths of a linear multiplier
/// (10 = 1.0x, unity). Models the card's analog output stage; the default is
/// neutral, so the mix reaches the host at the level the machine staged it.
///
/// This was 12.0x, and that was correct for the card it was measured on: the
/// CT1745 then powered on at master -14 dB AND voice -14 dB, so a title that
/// never programmed the mixer -- the common case -- played its digital voice
/// 28 dB down and needed roughly that much back to be audible at all. The
/// volume-decode fix moved those defaults to 0 dB (matching DOSBox-X and
/// 86Box) and so removed the 28 dB this gain existed to cancel, leaving a
/// bare +21.6 dB on a mix that was already at unity. Everything clipped:
/// symmetric saturation squashes a panned image to the centre and squares off
/// every waveform, which is precisely the "no stereo separation" and "peaked
/// and muffled" pair reported after that fix.
///
/// Headroom below full scale is now reserved once on the machine's summing
/// node (`MIX_HEADROOM`), and raising the level is SNDMIXER.COM's job. The
/// slider still runs to `OUTPUT_GAIN_MAX` for genuinely quiet material.
pub const DEFAULT_OUTPUT_GAIN: u32 = 10;

/// Upper bound for the output gain (tenths); 500 = 50x. Generous headroom above
/// the default so a very quiet game can still be brought up, guarding only
/// against an absurd hand-edited value.
pub const OUTPUT_GAIN_MAX: u32 = 500;

/// The key `output_gain` replaced, and which is now ignored on load.
///
/// Renaming it is a version break, and a deliberate one. `amp_gain` did not
/// change its RANGE, it changed its MEANING: every persisted value was chosen
/// against a chain that put a fixed 12.0x compensator downstream of it, and
/// that compensator no longer exists. The 120 that shipped as the default was
/// itself calibrated to a CT1745 that powered on 28 dB down. Clamping such a
/// file to `OUTPUT_GAIN_MAX` -- all `load_with` used to do -- keeps it running
/// at +21.6 dB into the clamp, which is the bug this branch fixes, still
/// happening to everyone who ever opened the config menu.
///
/// No rescale is attempted. There is no factor that is right for both the
/// default value and a value the user picked by ear, and this is prerelease
/// (see the no-shipped-version posture): a clean break to the fresh default is
/// honest, whereas a heuristic would silently invent a level nobody chose.
const LEGACY_OUTPUT_GAIN_KEY: &str = "amp_gain";

/// Default PC speaker volume, as a percent (100 = full). The speaker is separate
/// from the ReSonique 2 card; this is a straight attenuation so it can be turned
/// down or muted (0) independently of the card's amp gain.
pub const DEFAULT_PC_SPEAKER_VOLUME: u32 = 100;

/// A host hotkey: modifier flags plus a key name. `key` is the winit `KeyCode`
/// debug name (e.g. "F2", "KeyA"), which the GUI compares against the live key
/// and renders prettily. Kept winit-free so prefs stays plain data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyBinding {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub key: String,
}

impl KeyBinding {
    pub fn new(ctrl: bool, shift: bool, alt: bool, key: &str) -> Self {
        Self {
            ctrl,
            shift,
            alt,
            key: key.to_string(),
        }
    }

    /// True when the live key name and modifier state match this binding.
    pub fn matches(&self, key: &str, ctrl: bool, shift: bool, alt: bool) -> bool {
        self.ctrl == ctrl && self.shift == shift && self.alt == alt && self.key == key
    }

    /// Human label like "Ctrl+F2". Strips the winit "Key"/"Digit" prefixes so a
    /// letter or number reads naturally.
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
        let key = self
            .key
            .strip_prefix("Key")
            .or_else(|| self.key.strip_prefix("Digit"))
            .unwrap_or(&self.key);
        s.push_str(key);
        s
    }
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GuiPrefs {
    /// Master output volume, 0.0..1.0. Applied host-side as a perceptual gain.
    pub master_volume: f32,
    /// ReSonique 2 output amp gain, in tenths of a linear multiplier
    /// (10 = 1.0x unity, the default; see `DEFAULT_OUTPUT_GAIN`). Models the
    /// card's analog output stage, which the digital CT1745 model does not
    /// represent, and is the one host-side control over the card's absolute
    /// level.
    ///
    /// It sits at the END of the machine's chain, after the CT1745 legs are
    /// summed and after `MIX_HEADROOM` has reserved 6 dB below full scale, so
    /// raising it spends that reserve and then clips. Unity is the level the
    /// machine staged; the room to go up is for genuinely quiet material, not
    /// for correcting the mix, which is SNDMIXER.COM's job.
    ///
    /// Persisted as `output_gain`. It was `amp_gain` while the chain carried a
    /// 12.0x compensator; see `LEGACY_OUTPUT_GAIN_KEY` for why old values are
    /// dropped rather than carried over. Converted to a multiplier by
    /// `amp_multiplier`.
    pub output_gain: u32,
    /// PC speaker volume, as a percent (100 = full, 0 = muted). A linear
    /// attenuation applied host-side to the speaker only, independent of the card
    /// amp, so the beeps can be turned down or off.
    pub pc_speaker_volume: u32,
    /// CRT presentation style: off, subtle (default), or Ye Olde Screene.
    pub crt_style: CrtStyle,
    /// Hotkey that releases captured input. Default Ctrl+F2.
    pub input_release: KeyBinding,
    /// Hotkey that toggles fullscreen. Default Ctrl+F11.
    pub fullscreen: KeyBinding,
    /// Optional host controller mapping for the Izarra gameport.
    pub joystick_binding: Option<JoystickBinding>,
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
            output_gain: DEFAULT_OUTPUT_GAIN,
            pc_speaker_volume: DEFAULT_PC_SPEAKER_VOLUME,
            crt_style: CrtStyle::Subtle,
            input_release: KeyBinding::new(true, false, false, "F2"),
            fullscreen: KeyBinding::new(true, false, false, "F11"),
            joystick_binding: None,
            last_floppy_image: None,
            last_cd_image: None,
            last_cd_folder: None,
            panel_open: true,
            midi: MidiConfig::default(),
        }
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
        if value
            .as_table()
            .is_some_and(|table| table.contains_key(LEGACY_OUTPUT_GAIN_KEY))
        {
            warn!(
                path = %path.display(),
                "ignoring the retired `{LEGACY_OUTPUT_GAIN_KEY}` in izarravm.conf: the output \
                 stage is unity now, so its old value is miscalibrated; the key is `output_gain` \
                 and it starts fresh at {DEFAULT_OUTPUT_GAIN}"
            );
        }
        match value.try_into::<Self>() {
            Ok(mut prefs) => {
                prefs.master_volume = prefs.master_volume.clamp(0.0, 1.0);
                prefs.output_gain = prefs.output_gain.min(OUTPUT_GAIN_MAX);
                prefs.pc_speaker_volume = prefs.pc_speaker_volume.min(100);
                prefs
            }
            Err(err) => {
                warn!(%err, path = %path.display(), "could not parse izarravm.conf; using defaults");
                Self::default()
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

#[cfg(test)]
#[path = "prefs_test.rs"]
mod tests;
