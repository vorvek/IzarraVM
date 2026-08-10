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

/// The three retired audio keys are DROPPED, not clamped and carried over.
///
/// Each named a level inside the machine's own audio chain, and that chain is
/// the guest's: the ReSonique 2's output stage and the PC speaker's leg are
/// CT1745 registers that SNDMIXER.COM sets and any program can read back. A
/// host-side second copy could not be seen by the guest and had to be kept in
/// step with a mixer entitled to disagree with it.
///
/// Dropping beats carrying over. `amp_gain` was retired once already, and its
/// persisted values were calibrated against a chain with a 12.0x compensator in
/// it -- the loader only ever CLAMPED, so every file written before that fix
/// kept running the output stage at +21.6 dB into the clamp. `output_gain`
/// replaced it and shipped for one branch. There is no factor that is right for
/// both a default nobody chose and a level a user picked by ear, so the file
/// simply loses them and the machine's own mixer answers from here.
#[test]
fn the_retired_audio_keys_are_ignored_and_never_written_back() {
    let path = Path::new("izarravm.conf");
    let load = |text: &str| {
        let text = text.to_string();
        GuiPrefs::load_with(path, move |_| Ok(text.clone()))
    };

    // Each retired key on its own, at values the old loaders accepted: the
    // shipped legacy default, a hand-raised one, the newer key, and the
    // speaker percent.
    for retired in [
        "amp_gain = 120
",
        "amp_gain = 300
",
        "output_gain = 25
",
        "output_gain = 500
",
        "pc_speaker_volume = 40
",
    ] {
        assert_eq!(
            load(retired),
            GuiPrefs::default(),
            "{retired:?} must load as if it were not there at all"
        );
    }

    // All three at once, alongside a key that IS live: the live one survives
    // and the retired ones leave no trace. Without the master_volume half this
    // would also pass if the loader threw the whole file away.
    let mixed = load(
        "amp_gain = 120
output_gain = 25
pc_speaker_volume = 40
master_volume = 0.25
",
    );
    assert_eq!(
        mixed,
        GuiPrefs {
            master_volume: 0.25,
            ..GuiPrefs::default()
        }
    );

    // Never written back, so the file heals on the next save.
    let text = toml::to_string_pretty(&GuiPrefs::default()).unwrap();
    for retired in ["amp_gain", "output_gain", "pc_speaker_volume"] {
        assert!(!text.contains(retired), "{retired} in {text}");
    }
}

/// The knob's persisted range reaches past unity, and widening it retires
/// nothing.
///
/// This is the whole compatibility story for `master_volume` going above 1.0:
/// the accepted interval only grew, so every value an older build could have
/// written is still inside it and still means the level it always meant. That is
/// why there is no new retired key and no rescale -- unlike `amp_gain` above,
/// which changed what its numbers MEANT and had to be dropped. Assert the
/// legacy values load untouched, that an above-unity value survives the round
/// trip, and that the ceiling still catches a hand-edited file.
#[test]
fn master_volume_persists_above_unity_and_keeps_legacy_values_untouched() {
    let path = Path::new("izarravm.conf");
    let load = |text: &str| {
        let text = text.to_string();
        GuiPrefs::load_with(path, move |_| Ok(text.clone()))
    };

    // Legacy: every value an older build could write, loaded with no change.
    for legacy in [0.0f32, 0.25, 0.65, 0.8, 1.0] {
        let loaded = load(&format!("master_volume = {legacy}\n"));
        assert_eq!(
            loaded.master_volume, legacy,
            "a saved {legacy} must still mean {legacy}"
        );
    }

    // Above unity: accepted, and not flattened back to 1.0 the way the old
    // clamp did.
    for boosted in [1.5f32, 2.0, 3.75, MAX_VOLUME] {
        let loaded = load(&format!("master_volume = {boosted}\n"));
        assert_eq!(loaded.master_volume, boosted);
    }

    // Round trip through the file the GUI actually writes.
    let prefs = GuiPrefs {
        master_volume: 3.75,
        ..GuiPrefs::default()
    };
    let text = toml::to_string_pretty(&prefs).expect("serialize");
    let parsed = load(&text);
    assert_eq!(parsed, prefs, "the boosted level survives a save and load");

    // Still bounded on both ends: a hand-edited file cannot ask for more than
    // the knob can, or for a negative level.
    assert_eq!(load("master_volume = 50.0\n").master_volume, MAX_VOLUME);
    assert_eq!(load("master_volume = -2.0\n").master_volume, 0.0);
}

/// `--config` pointed at a prefs file must still be RECOGNISED as one.
///
/// The retired keys stay in the marker list for exactly this: a file old enough
/// to carry `output_gain` and nothing else newer is still unmistakably the
/// GUI's own conf, and pointing the machine config flag at it should say so
/// rather than fail on an unknown field naming one key and explaining nothing.
#[test]
fn retired_keys_still_identify_a_prefs_file_to_the_config_flag() {
    for key in [
        "amp_gain = 120",
        "output_gain = 25",
        "pc_speaker_volume = 40",
    ] {
        let value = toml::from_str::<toml::Value>(key).unwrap();
        assert!(
            izarravm_core::gui_prefs_marker(&value).is_some(),
            "{key} must still mark the file as GUI prefs"
        );
    }
}

#[test]
fn prefs_path_sits_beside_c_root() {
    let c_root = PathBuf::from("/home/user/.izarravm/c_drive");
    let path = prefs_path(&c_root);
    assert_eq!(path, PathBuf::from("/home/user/.izarravm/izarravm.conf"));
}
