// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_core::{MidiBackend, MidiPortId};
use izarravm_input::{
    JoystickAxis, JoystickAxisBinding, JoystickBinding, JoystickButton, JoystickPolarity,
};

fn joystick_binding() -> JoystickBinding {
    JoystickBinding {
        controller_uuid: "00010203-0405-0607-0809-0a0b0c0d0e0f".into(),
        controller_name: "Test Controller".into(),
        x: JoystickAxisBinding {
            control: JoystickAxis::LeftStickX,
            polarity: JoystickPolarity::Positive,
        },
        y: JoystickAxisBinding {
            control: JoystickAxis::LeftStickY,
            polarity: JoystickPolarity::Negative,
        },
        button_1: JoystickButton::South,
        button_2: JoystickButton::East,
    }
}

#[test]
fn round_trips_through_toml() {
    let prefs = GuiPrefs {
        master_volume: 0.65,
        amp_gain: 55,
        pc_speaker_volume: 40,
        crt_style: CrtStyle::YeOlde,
        input_release: KeyBinding::new(true, true, false, "F4"),
        fullscreen: KeyBinding::new(false, false, true, "Enter"),
        joystick_binding: Some(joystick_binding()),
        last_floppy_image: Some(PathBuf::from("/tmp/disk.img")),
        last_cd_image: Some(PathBuf::from("/tmp/game.iso")),
        last_cd_folder: None,
        panel_open: false,
        midi: MidiConfig {
            backend: MidiBackend::External,
            external_port: Some(MidiPortId {
                name: "LoopMIDI".into(),
                ordinal: 1,
            }),
            soundfont: Some(PathBuf::from("/tmp/external.sf3")),
            mt32_control_rom: None,
            mt32_pcm_rom: None,
        },
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
    assert_eq!(parsed.amp_gain, DEFAULT_AMP_GAIN);
    assert_eq!(parsed.pc_speaker_volume, DEFAULT_PC_SPEAKER_VOLUME);
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
    assert_eq!(parsed.joystick_binding, None);
    assert!(parsed.panel_open, "panel defaults to open for older files");
    assert_eq!(parsed.midi, MidiConfig::default());
}

#[test]
fn joystick_binding_round_trips_with_named_controls_and_polarity() {
    let prefs = GuiPrefs {
        joystick_binding: Some(joystick_binding()),
        ..GuiPrefs::default()
    };
    let text = toml::to_string_pretty(&prefs).unwrap();
    assert!(text.contains("[joystick_binding.x]"));
    assert!(text.contains("control = \"left_stick_x\""));
    assert!(text.contains("polarity = \"positive\""));
    assert_eq!(toml::from_str::<GuiPrefs>(&text).unwrap(), prefs);
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
fn retired_glide_render_threads_key_is_ignored_and_not_written() {
    let prefs: GuiPrefs = toml::from_str("glide_render_threads = 4\n").unwrap();
    assert_eq!(prefs, GuiPrefs::default());
    assert!(
        !toml::to_string(&prefs)
            .unwrap()
            .contains("glide_render_threads")
    );
}

#[test]
fn prefs_path_sits_beside_c_root() {
    let c_root = PathBuf::from("/home/user/.izarravm/c_drive");
    let path = prefs_path(&c_root);
    assert_eq!(path, PathBuf::from("/home/user/.izarravm/izarravm.conf"));
}
