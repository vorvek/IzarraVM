//! GUI preferences, persisted as a small `izarravm.conf` TOML file next to the
//! C: root (in the directory that contains the c_drive folder). This is separate
//! from `AppConfig`: it holds host-side GUI state (master volume, last mounts)
//! that the machine config has no place for.
//!
//! Every load and save is best-effort: an IO or parse error logs a warning and
//! falls back to defaults rather than aborting the run.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::warn;

use izarravm_video::{DISTIRA_DEFAULT_RENDER_THREADS, normalize_distira_render_threads};

/// File name for the GUI prefs, written next to the C: root.
const PREFS_FILE: &str = "izarravm.conf";

/// Default master volume (0..1). 0.8 sits comfortably below clipping for most
/// material while still being plainly audible.
const DEFAULT_VOLUME: f32 = 0.8;

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
    /// Distira Glide renderer worker count. Matches 86Box's choices: 1, 2, or 4.
    pub glide_render_threads: u8,
    /// CRT presentation style: off, subtle (default), or Ye Olde Screene.
    pub crt_style: CrtStyle,
    /// Hotkey that releases captured input. Default Ctrl+F2.
    pub input_release: KeyBinding,
    /// Hotkey that toggles fullscreen. Default Ctrl+F11.
    pub fullscreen: KeyBinding,
    /// Last floppy IMG mounted, re-mounted on startup if it still exists.
    pub last_floppy_image: Option<PathBuf>,
    /// Last folder mounted as drive A:, restored on startup if it still exists.
    pub last_floppy_folder: Option<PathBuf>,
    /// Reserved for a future CD image mount. Persisted so the slot exists.
    pub last_cd_image: Option<PathBuf>,
}

impl Default for GuiPrefs {
    fn default() -> Self {
        Self {
            master_volume: DEFAULT_VOLUME,
            glide_render_threads: DISTIRA_DEFAULT_RENDER_THREADS,
            crt_style: CrtStyle::Subtle,
            input_release: KeyBinding::new(true, false, false, "F2"),
            fullscreen: KeyBinding::new(true, false, false, "F11"),
            last_floppy_image: None,
            last_floppy_folder: None,
            last_cd_image: None,
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
    /// Load the prefs from `path`. A missing file yields the defaults silently;
    /// an unreadable or unparseable file logs a warning and also yields defaults,
    /// so a corrupt file never blocks startup. The volume is clamped to 0..1.
    pub fn load(path: &Path) -> Self {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(err) => {
                warn!(%err, path = %path.display(), "could not read izarravm.conf; using defaults");
                return Self::default();
            }
        };
        match toml::from_str::<Self>(&text) {
            Ok(mut prefs) => {
                prefs.master_volume = prefs.master_volume.clamp(0.0, 1.0);
                prefs.glide_render_threads =
                    normalize_distira_render_threads(prefs.glide_render_threads);
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
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_toml() {
        let prefs = GuiPrefs {
            master_volume: 0.65,
            glide_render_threads: 4,
            crt_style: CrtStyle::YeOlde,
            input_release: KeyBinding::new(true, true, false, "F4"),
            fullscreen: KeyBinding::new(false, false, true, "Enter"),
            last_floppy_image: Some(PathBuf::from("/tmp/disk.img")),
            last_floppy_folder: Some(PathBuf::from("/tmp/games")),
            last_cd_image: None,
        };
        let text = toml::to_string_pretty(&prefs).expect("serialize");
        let parsed: GuiPrefs = toml::from_str(&text).expect("deserialize");
        assert_eq!(parsed, prefs);
    }

    #[test]
    fn missing_keys_fall_back_to_defaults() {
        // An empty file should parse into the full default set, so a partial or
        // older file never fails to load.
        let parsed: GuiPrefs = toml::from_str("").expect("deserialize empty");
        assert_eq!(parsed, GuiPrefs::default());
        assert_eq!(parsed.master_volume, DEFAULT_VOLUME);
        assert_eq!(parsed.glide_render_threads, 2);
        assert_eq!(
            parsed.crt_style,
            CrtStyle::Subtle,
            "CRT defaults to the subtle look for older files"
        );
        assert_eq!(
            parsed.input_release,
            KeyBinding::new(true, false, false, "F2")
        );
        assert_eq!(
            parsed.fullscreen,
            KeyBinding::new(true, false, false, "F11")
        );
    }

    #[test]
    fn key_binding_display_strips_winit_prefixes() {
        assert_eq!(
            KeyBinding::new(true, false, false, "F2").display(),
            "Ctrl+F2"
        );
        assert_eq!(
            KeyBinding::new(true, true, true, "KeyA").display(),
            "Ctrl+Shift+Alt+A"
        );
        assert_eq!(
            KeyBinding::new(false, false, false, "Digit5").display(),
            "5"
        );
    }

    #[test]
    fn crt_style_serialises_lowercase() {
        assert_eq!(
            toml::Value::try_from(CrtStyle::YeOlde).unwrap().as_str(),
            Some("yeolde")
        );
        assert_eq!(CrtStyle::default(), CrtStyle::Subtle);
        assert_eq!(CrtStyle::Off.as_u32(), 0);
        assert_eq!(CrtStyle::YeOlde.as_u32(), 2);
    }

    #[test]
    fn glide_render_threads_are_limited_to_86box_choices() {
        let mut path = std::env::temp_dir();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        path.push(format!(
            "izarravm-prefs-{}-{}.conf",
            std::process::id(),
            nonce
        ));
        std::fs::write(&path, "glide_render_threads = 3\n").expect("write prefs");

        let prefs = GuiPrefs::load(&path);
        let _ = std::fs::remove_file(path);

        assert_eq!(prefs.glide_render_threads, 2);
    }

    #[test]
    fn prefs_path_sits_beside_c_root() {
        let c_root = PathBuf::from("/home/user/.izarravm/c_drive");
        let path = prefs_path(&c_root);
        assert_eq!(path, PathBuf::from("/home/user/.izarravm/izarravm.conf"));
    }
}
