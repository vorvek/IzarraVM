use winit::keyboard::KeyCode;

/// Physical key -> (Set 1 make code, is-extended). Extended keys are prefixed
/// with 0xE0 on both make and break. Returns None for keys outside the DOS set.
#[allow(dead_code)]
pub(crate) fn keycode_to_set1(code: KeyCode) -> Option<(u8, bool)> {
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

/// Stub for the stateful translator that Task 2 will flesh out.
#[derive(Debug, Default)]
pub struct HostKeyboard;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_core_and_extended_keys() {
        assert_eq!(keycode_to_set1(KeyCode::Escape), Some((0x01, false)));
        assert_eq!(keycode_to_set1(KeyCode::KeyA), Some((0x1e, false)));
        assert_eq!(keycode_to_set1(KeyCode::ShiftLeft), Some((0x2a, false)));
        assert_eq!(keycode_to_set1(KeyCode::ShiftRight), Some((0x36, false)));
        assert_eq!(keycode_to_set1(KeyCode::IntlBackslash), Some((0x56, false)));
        assert_eq!(keycode_to_set1(KeyCode::Numpad8), Some((0x48, false)));
        assert_eq!(keycode_to_set1(KeyCode::ArrowUp), Some((0x48, true)));
        assert_eq!(keycode_to_set1(KeyCode::ArrowRight), Some((0x4d, true)));
        assert_eq!(keycode_to_set1(KeyCode::ControlRight), Some((0x1d, true)));
        assert_eq!(keycode_to_set1(KeyCode::AltRight), Some((0x38, true)));
        assert_eq!(keycode_to_set1(KeyCode::NumpadDivide), Some((0x35, true)));
        assert_eq!(keycode_to_set1(KeyCode::NumpadEnter), Some((0x1c, true)));
        assert_eq!(keycode_to_set1(KeyCode::Delete), Some((0x53, true)));
        assert_eq!(keycode_to_set1(KeyCode::F24), None);
    }
}
