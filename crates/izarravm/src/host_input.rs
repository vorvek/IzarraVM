// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use izarravm_core::InputConfig;
use izarravm_input::{GuestKeyTransition, HostKeyboard};
use winit::keyboard::KeyCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HostInputPolicy {
    keyboard: bool,
    mouse: bool,
    joystick: bool,
}

impl HostInputPolicy {
    pub(crate) const fn new(keyboard: bool, mouse: bool, joystick: bool) -> Self {
        Self {
            keyboard,
            mouse,
            joystick,
        }
    }

    pub(crate) const fn from_config(config: &InputConfig) -> Self {
        Self::new(config.keyboard, config.mouse, config.joystick)
    }

    pub(crate) fn key_transition(
        self,
        keyboard: &mut HostKeyboard,
        code: KeyCode,
        pressed: bool,
        repeat: bool,
    ) -> Option<GuestKeyTransition> {
        if self.keyboard {
            keyboard.transition(code, pressed, repeat)
        } else {
            keyboard.release_all_transitions();
            None
        }
    }

    pub(crate) fn release_key_transitions(
        self,
        keyboard: &mut HostKeyboard,
    ) -> Vec<GuestKeyTransition> {
        let transitions = keyboard.release_all_transitions();
        if self.keyboard {
            transitions
        } else {
            Vec::new()
        }
    }

    pub(crate) const fn keyboard_enabled(self) -> bool {
        self.keyboard
    }

    pub(crate) const fn mouse_enabled(self) -> bool {
        self.mouse
    }

    pub(crate) const fn mouse_capture_requested(self, clicked: bool, captured: bool) -> bool {
        self.mouse && clicked && !captured
    }

    pub(crate) const fn mouse_active(self, captured: bool) -> bool {
        self.mouse && captured
    }

    pub(crate) const fn joystick_enabled(self) -> bool {
        self.joystick
    }
}

#[cfg(test)]
#[path = "host_input_test.rs"]
mod tests;
