// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize};
use winit::keyboard::KeyCode;

const GUEST_KEY_CHOICES: &[(u8, bool, &str)] = &[
    (0x01, false, "Esc"),
    (0x02, false, "1"),
    (0x03, false, "2"),
    (0x04, false, "3"),
    (0x05, false, "4"),
    (0x06, false, "5"),
    (0x07, false, "6"),
    (0x08, false, "7"),
    (0x09, false, "8"),
    (0x0a, false, "9"),
    (0x0b, false, "0"),
    (0x0c, false, "-"),
    (0x0d, false, "="),
    (0x0e, false, "Backspace"),
    (0x0f, false, "Tab"),
    (0x10, false, "Q"),
    (0x11, false, "W"),
    (0x12, false, "E"),
    (0x13, false, "R"),
    (0x14, false, "T"),
    (0x15, false, "Y"),
    (0x16, false, "U"),
    (0x17, false, "I"),
    (0x18, false, "O"),
    (0x19, false, "P"),
    (0x1a, false, "["),
    (0x1b, false, "]"),
    (0x1c, false, "Enter"),
    (0x1d, false, "Left Ctrl"),
    (0x1e, false, "A"),
    (0x1f, false, "S"),
    (0x20, false, "D"),
    (0x21, false, "F"),
    (0x22, false, "G"),
    (0x23, false, "H"),
    (0x24, false, "J"),
    (0x25, false, "K"),
    (0x26, false, "L"),
    (0x27, false, ";"),
    (0x28, false, "'"),
    (0x29, false, "`"),
    (0x2a, false, "Left Shift"),
    (0x2b, false, "\\"),
    (0x2c, false, "Z"),
    (0x2d, false, "X"),
    (0x2e, false, "C"),
    (0x2f, false, "V"),
    (0x30, false, "B"),
    (0x31, false, "N"),
    (0x32, false, "M"),
    (0x33, false, ","),
    (0x34, false, "."),
    (0x35, false, "/"),
    (0x36, false, "Right Shift"),
    (0x37, false, "Keypad *"),
    (0x38, false, "Left Alt"),
    (0x39, false, "Space"),
    (0x3a, false, "Caps Lock"),
    (0x3b, false, "F1"),
    (0x3c, false, "F2"),
    (0x3d, false, "F3"),
    (0x3e, false, "F4"),
    (0x3f, false, "F5"),
    (0x40, false, "F6"),
    (0x41, false, "F7"),
    (0x42, false, "F8"),
    (0x43, false, "F9"),
    (0x44, false, "F10"),
    (0x45, false, "Num Lock"),
    (0x46, false, "Scroll Lock"),
    (0x47, false, "Keypad 7"),
    (0x48, false, "Keypad 8"),
    (0x49, false, "Keypad 9"),
    (0x4a, false, "Keypad -"),
    (0x4b, false, "Keypad 4"),
    (0x4c, false, "Keypad 5"),
    (0x4d, false, "Keypad 6"),
    (0x4e, false, "Keypad +"),
    (0x4f, false, "Keypad 1"),
    (0x50, false, "Keypad 2"),
    (0x51, false, "Keypad 3"),
    (0x52, false, "Keypad 0"),
    (0x53, false, "Keypad ."),
    (0x56, false, "ISO \\"),
    (0x57, false, "F11"),
    (0x58, false, "F12"),
    (0x1c, true, "Keypad Enter"),
    (0x1d, true, "Right Ctrl"),
    (0x35, true, "Keypad /"),
    (0x38, true, "Right Alt"),
    (0x47, true, "Home"),
    (0x48, true, "Up"),
    (0x49, true, "Page Up"),
    (0x4b, true, "Left"),
    (0x4d, true, "Right"),
    (0x4f, true, "End"),
    (0x50, true, "Down"),
    (0x51, true, "Page Down"),
    (0x52, true, "Insert"),
    (0x53, true, "Delete"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GuestKey {
    pub make: u8,
    #[serde(default)]
    pub extended: bool,
}

impl GuestKey {
    pub fn from_key_code(code: KeyCode) -> Option<Self> {
        keycode_to_set1(code).map(|(make, extended)| Self { make, extended })
    }

    pub fn scancodes(self, pressed: bool) -> Vec<u8> {
        let mut out = Vec::with_capacity(2);
        if self.extended {
            out.push(0xe0);
        }
        out.push(if pressed { self.make } else { self.make | 0x80 });
        out
    }

    pub fn choices() -> impl ExactSizeIterator<Item = Self> {
        GUEST_KEY_CHOICES.iter().map(|(make, extended, _)| Self {
            make: *make,
            extended: *extended,
        })
    }

    pub fn display(self) -> String {
        GUEST_KEY_CHOICES
            .iter()
            .find(|(make, extended, _)| *make == self.make && *extended == self.extended)
            .map(|(_, _, name)| (*name).to_owned())
            .unwrap_or_else(|| {
                format!(
                    "Set 1 {:02X}{}",
                    self.make,
                    if self.extended { " E0" } else { "" }
                )
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
#[serde(transparent)]
pub struct GuestKeyChord(Vec<GuestKey>);

impl GuestKeyChord {
    pub fn new(keys: impl IntoIterator<Item = GuestKey>) -> Self {
        let mut unique = Vec::new();
        for key in keys {
            if !unique.contains(&key) {
                unique.push(key);
            }
        }
        Self(unique)
    }

    pub fn keys(&self) -> &[GuestKey] {
        &self.0
    }

    pub fn display(&self) -> String {
        if self.0.is_empty() {
            return "Not assigned".to_owned();
        }
        self.0
            .iter()
            .map(|key| key.display())
            .collect::<Vec<_>>()
            .join("+")
    }
}

impl From<GuestKey> for GuestKeyChord {
    fn from(key: GuestKey) -> Self {
        Self(vec![key])
    }
}

impl<'de> Deserialize<'de> for GuestKeyChord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Single(GuestKey),
            Chord(Vec<GuestKey>),
        }

        Ok(match Wire::deserialize(deserializer)? {
            Wire::Single(key) => key.into(),
            Wire::Chord(keys) => Self::new(keys),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestKeyTransition {
    pub key: GuestKey,
    pub pressed: bool,
    pub repeat: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GuestKeySource {
    Physical,
    System,
    Controller(u16),
}

#[derive(Debug, Default)]
pub struct GuestKeyRouter {
    owners: BTreeMap<GuestKey, BTreeSet<GuestKeySource>>,
}

impl GuestKeyRouter {
    pub fn apply(&mut self, source: GuestKeySource, transition: GuestKeyTransition) -> Vec<u8> {
        if transition.pressed {
            let owners = self.owners.entry(transition.key).or_default();
            let was_released = owners.is_empty();
            let fresh_source = owners.insert(source);
            if was_released || (transition.repeat && source == GuestKeySource::Physical) {
                return transition.key.scancodes(true);
            }
            if !fresh_source {
                return Vec::new();
            }
        } else if let Some(owners) = self.owners.get_mut(&transition.key) {
            owners.remove(&source);
            if owners.is_empty() {
                self.owners.remove(&transition.key);
                return transition.key.scancodes(false);
            }
        }
        Vec::new()
    }

    pub fn release_source(&mut self, source: GuestKeySource) -> Vec<u8> {
        let keys = self.owners.keys().copied().collect::<Vec<_>>();
        let mut out = Vec::new();
        for key in keys {
            let transition = GuestKeyTransition {
                key,
                pressed: false,
                repeat: false,
            };
            out.extend(self.apply(source, transition));
        }
        out
    }
}

/// Physical key -> (Set 1 make code, is-extended). Extended keys are prefixed
/// with 0xE0 on both make and break. Returns None for keys outside the DOS set.
fn keycode_to_set1(code: KeyCode) -> Option<(u8, bool)> {
    use KeyCode::*;
    let plain = |c| Some((c, false));
    let ext = |c| Some((c, true));
    match code {
        Escape => plain(0x01),
        Digit1 => plain(0x02),
        Digit2 => plain(0x03),
        Digit3 => plain(0x04),
        Digit4 => plain(0x05),
        Digit5 => plain(0x06),
        Digit6 => plain(0x07),
        Digit7 => plain(0x08),
        Digit8 => plain(0x09),
        Digit9 => plain(0x0a),
        Digit0 => plain(0x0b),
        Minus => plain(0x0c),
        Equal => plain(0x0d),
        Backspace => plain(0x0e),
        Tab => plain(0x0f),
        KeyQ => plain(0x10),
        KeyW => plain(0x11),
        KeyE => plain(0x12),
        KeyR => plain(0x13),
        KeyT => plain(0x14),
        KeyY => plain(0x15),
        KeyU => plain(0x16),
        KeyI => plain(0x17),
        KeyO => plain(0x18),
        KeyP => plain(0x19),
        BracketLeft => plain(0x1a),
        BracketRight => plain(0x1b),
        Enter => plain(0x1c),
        ControlLeft => plain(0x1d),
        KeyA => plain(0x1e),
        KeyS => plain(0x1f),
        KeyD => plain(0x20),
        KeyF => plain(0x21),
        KeyG => plain(0x22),
        KeyH => plain(0x23),
        KeyJ => plain(0x24),
        KeyK => plain(0x25),
        KeyL => plain(0x26),
        Semicolon => plain(0x27),
        Quote => plain(0x28),
        Backquote => plain(0x29),
        ShiftLeft => plain(0x2a),
        Backslash => plain(0x2b),
        KeyZ => plain(0x2c),
        KeyX => plain(0x2d),
        KeyC => plain(0x2e),
        KeyV => plain(0x2f),
        KeyB => plain(0x30),
        KeyN => plain(0x31),
        KeyM => plain(0x32),
        Comma => plain(0x33),
        Period => plain(0x34),
        Slash => plain(0x35),
        ShiftRight => plain(0x36),
        NumpadMultiply => plain(0x37),
        AltLeft => plain(0x38),
        Space => plain(0x39),
        CapsLock => plain(0x3a),
        F1 => plain(0x3b),
        F2 => plain(0x3c),
        F3 => plain(0x3d),
        F4 => plain(0x3e),
        F5 => plain(0x3f),
        F6 => plain(0x40),
        F7 => plain(0x41),
        F8 => plain(0x42),
        F9 => plain(0x43),
        F10 => plain(0x44),
        NumLock => plain(0x45),
        ScrollLock => plain(0x46),
        Numpad7 => plain(0x47),
        Numpad8 => plain(0x48),
        Numpad9 => plain(0x49),
        NumpadSubtract => plain(0x4a),
        Numpad4 => plain(0x4b),
        Numpad5 => plain(0x4c),
        Numpad6 => plain(0x4d),
        NumpadAdd => plain(0x4e),
        Numpad1 => plain(0x4f),
        Numpad2 => plain(0x50),
        Numpad3 => plain(0x51),
        Numpad0 => plain(0x52),
        NumpadDecimal => plain(0x53),
        IntlBackslash => plain(0x56),
        F11 => plain(0x57),
        F12 => plain(0x58),
        ControlRight => ext(0x1d),
        AltRight => ext(0x38),
        NumpadDivide => ext(0x35),
        NumpadEnter => ext(0x1c),
        Insert => ext(0x52),
        Delete => ext(0x53),
        Home => ext(0x47),
        End => ext(0x4f),
        PageUp => ext(0x49),
        PageDown => ext(0x51),
        ArrowUp => ext(0x48),
        ArrowLeft => ext(0x4b),
        ArrowRight => ext(0x4d),
        ArrowDown => ext(0x50),
        _ => None,
    }
}

/// Stable per-key id for the held set: the make code plus the extended flag.
/// Keying on the Set 1 bytes (not KeyCode directly) avoids needing Ord/Hash on
/// KeyCode and keeps the held set tied to the wire format.
fn code_id(make: u8, extended: bool) -> u16 {
    u16::from(make) | (u16::from(extended) << 8)
}

fn repeats(code: KeyCode) -> bool {
    !matches!(
        code,
        KeyCode::ControlLeft
            | KeyCode::ControlRight
            | KeyCode::ShiftLeft
            | KeyCode::ShiftRight
            | KeyCode::AltLeft
            | KeyCode::AltRight
    )
}

/// Translates winit physical key events into Set 1 scancode bytes and remembers
/// which keys are held, so everything can be released at once on focus loss or
/// capture release. Pure: no windowing, no OS calls.
#[derive(Debug, Default)]
pub struct HostKeyboard {
    held: BTreeSet<u16>, // KeyCode encoded as u16 via code_id
}

impl HostKeyboard {
    pub fn transition(
        &mut self,
        code: KeyCode,
        pressed: bool,
        repeat: bool,
    ) -> Option<GuestKeyTransition> {
        let key = GuestKey::from_key_code(code)?;
        let id = code_id(key.make, key.extended);
        if pressed {
            let fresh_press = self.held.insert(id);
            let allowed_repeat = repeat && repeats(code);
            if !fresh_press && !allowed_repeat {
                return None;
            }
        } else if !self.held.remove(&id) {
            return None;
        }
        Some(GuestKeyTransition {
            key,
            pressed,
            repeat: pressed && repeat && repeats(code),
        })
    }

    pub fn release_all_transitions(&mut self) -> Vec<GuestKeyTransition> {
        std::mem::take(&mut self.held)
            .into_iter()
            .map(|id| GuestKeyTransition {
                key: GuestKey {
                    make: (id & 0xff) as u8,
                    extended: id & 0x100 != 0,
                },
                pressed: false,
                repeat: false,
            })
            .collect()
    }

    /// Make on press, make|0x80 on release, each 0xE0-prefixed for extended
    /// keys. Empty for keys outside the DOS set, and for a press of a key already
    /// held.
    ///
    /// A press of an already-held key is dropped unless the caller marks it as a
    /// typematic repeat. Modifiers do not repeat.
    pub fn key(&mut self, code: KeyCode, pressed: bool) -> Vec<u8> {
        self.key_with_repeat(code, pressed, false)
    }

    pub fn key_with_repeat(&mut self, code: KeyCode, pressed: bool, repeat: bool) -> Vec<u8> {
        self.transition(code, pressed, repeat)
            .map(|transition| transition.key.scancodes(transition.pressed))
            .unwrap_or_default()
    }

    pub fn is_held(&self, code: KeyCode) -> bool {
        keycode_to_set1(code)
            .map(|(make, extended)| self.held.contains(&code_id(make, extended)))
            .unwrap_or(false)
    }

    /// Break codes for every held key, then forget them all.
    pub fn release_all(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        for transition in self.release_all_transitions() {
            out.extend(transition.key.scancodes(false));
        }
        out
    }
}

#[cfg(test)]
#[path = "keyboard_test.rs"]
mod tests;
