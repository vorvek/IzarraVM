#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostInputKind {
    Keyboard,
    Mouse,
    Joystick,
    SteamInputFuture,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostInputEvent {
    Key {
        code: String,
        pressed: bool,
    },
    MouseButton {
        button: u8,
        pressed: bool,
    },
    JoystickButton {
        gamepad_id: u32,
        button: String,
        pressed: bool,
    },
    JoystickAxis {
        gamepad_id: u32,
        axis: String,
        value: f32,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct InputState {
    pub keyboard_enabled: bool,
    pub mouse_enabled: bool,
    pub joystick_enabled: bool,
}

impl Default for InputState {
    fn default() -> Self {
        Self {
            keyboard_enabled: true,
            mouse_enabled: true,
            joystick_enabled: true,
        }
    }
}

pub fn winit_keyboard_marker() -> &'static str {
    std::any::type_name::<winit::keyboard::KeyCode>()
}

pub fn gilrs_gamepad_marker() -> &'static str {
    std::any::type_name::<gilrs::Button>()
}
