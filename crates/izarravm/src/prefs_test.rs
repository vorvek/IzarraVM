// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_core::{MidiBackend, MidiPortId};
use izarravm_input::{
    ControllerDeviceMatcher, GuestControllerProfile, GuestKey, GuestKeyChord, JoystickAxis,
    JoystickAxisBinding, JoystickBinding, JoystickButton, JoystickPolarity,
};
use winit::keyboard::KeyCode;

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
        input_release: KeyBinding::new(true, true, false, true, "F4"),
        fullscreen: KeyBinding::new(false, false, true, false, "Enter"),
        controller: Some(ControllerConfig::from_legacy(joystick_binding())),
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
        KeyBinding::new(false, false, false, true, "F2")
    );
    assert_eq!(
        parsed.fullscreen,
        KeyBinding::new(false, false, false, true, "F4")
    );
    assert_eq!(parsed.controller, None);
    assert!(parsed.panel_open, "panel defaults to open for older files");
    assert_eq!(parsed.midi, MidiConfig::default());
}

#[test]
fn legacy_joystick_binding_migrates_and_is_not_written_back() {
    let text = r#"
[joystick_binding]
controller_uuid = "00010203-0405-0607-0809-0a0b0c0d0e0f"
controller_name = "Test Controller"
button_1 = "south"
button_2 = "east"

[joystick_binding.x]
control = "left_stick_x"
polarity = "positive"

[joystick_binding.y]
control = "left_stick_y"
polarity = "negative"
"#;
    let prefs = toml::from_str::<GuiPrefs>(text).unwrap();
    let controller = prefs.controller.as_ref().expect("legacy mapping migrated");
    assert_eq!(controller.profile, GuestControllerProfile::Standard);
    assert!(!controller.axes[0].transform.inverted);
    assert!(controller.axes[1].transform.inverted);
    let saved = toml::to_string_pretty(&prefs).unwrap();
    assert!(saved.contains("[controller]"));
    assert!(!saved.contains("joystick_binding"));
    assert_eq!(toml::from_str::<GuiPrefs>(&saved).unwrap(), prefs);
}

#[test]
fn controller_key_chords_round_trip_and_accept_the_old_single_key_shape() {
    let device = ControllerDeviceMatcher {
        backend: "gilrs-wgi".into(),
        platform: "windows".into(),
        guid: "controller-guid".into(),
        vendor_id: Some(0x1234),
        product_id: Some(0x5678),
        name: "Test Controller".into(),
        occurrence: 0,
    };
    let mut controller = ControllerConfig::default_keyboard(device);
    controller.keys[0].host.host.raw_code = Some(0);
    let shift = GuestKey::from_key_code(KeyCode::ShiftLeft).unwrap();
    let letter = GuestKey::from_key_code(KeyCode::KeyA).unwrap();
    controller.keys[12].guest = GuestKeyChord::new([shift, letter]);
    let prefs = GuiPrefs {
        controller: Some(controller),
        ..GuiPrefs::default()
    };

    let text = toml::to_string_pretty(&prefs).unwrap();
    assert!(text.contains("raw_code = 0"));
    let parsed = toml::from_str::<GuiPrefs>(&text).unwrap();
    assert_eq!(parsed, prefs);

    let mut legacy = toml::Value::try_from(&prefs).unwrap();
    let keys = legacy
        .get_mut("controller")
        .and_then(toml::Value::as_table_mut)
        .and_then(|controller| controller.get_mut("keys"))
        .and_then(toml::Value::as_array_mut)
        .expect("serialized controller keys");
    let old_single_key = keys[0]
        .get("guest")
        .and_then(toml::Value::as_array)
        .and_then(|chord| chord.first())
        .cloned()
        .expect("default chord key");
    keys[0]
        .as_table_mut()
        .expect("key binding table")
        .insert("guest".into(), old_single_key);
    let migrated = legacy.try_into::<GuiPrefs>().unwrap();
    assert_eq!(migrated.controller.unwrap().keys[0].guest.keys().len(), 1);
}

#[test]
fn key_binding_display_strips_winit_prefixes() {
    assert_eq!(
        KeyBinding::new(false, false, false, true, "F2").display(),
        format!("{SUPER_KEY_NAME}+F2")
    );
    assert_eq!(
        KeyBinding::new(true, true, true, true, "KeyA").display(),
        format!("Ctrl+Shift+Alt+{SUPER_KEY_NAME}+A")
    );
    assert_eq!(
        KeyBinding::new(false, false, false, false, "Digit5").display(),
        "5"
    );
}

#[test]
fn the_super_key_takes_the_name_the_host_gives_it() {
    // The label follows the key cap. The stored field does not: a file written
    // on one host reads the same on the other.
    #[cfg(windows)]
    assert_eq!(SUPER_KEY_NAME, "Win");
    #[cfg(not(windows))]
    assert_eq!(SUPER_KEY_NAME, "Super");
    let text = toml::to_string_pretty(&GuiPrefs::default()).expect("serialize");
    assert!(text.contains("super = true"), "{text}");
}

#[test]
fn a_binding_without_the_super_field_still_loads() {
    // A file written before Super was a modifier has no `super` key in the
    // table. It must parse, or one missing field would reset every preference.
    let text = "[fullscreen]\nctrl = false\nshift = true\nalt = false\nkey = \"F9\"\n";
    let parsed: GuiPrefs = toml::from_str(text).expect("deserialize");
    assert_eq!(
        parsed.fullscreen,
        KeyBinding::new(false, true, false, false, "F9")
    );
}

#[test]
fn retired_hotkey_defaults_move_to_the_current_ones() {
    let text = "[input_release]\nctrl = true\nshift = false\nalt = false\nkey = \"F2\"\n\
                [fullscreen]\nctrl = true\nshift = false\nalt = false\nkey = \"F11\"\n";
    let path = PathBuf::from("izarravm.conf");
    let moved = GuiPrefs::load_with(&path, |_| Ok(text.to_string()));
    assert_eq!(moved.input_release, GuiPrefs::default().input_release);
    assert_eq!(moved.fullscreen, GuiPrefs::default().fullscreen);

    // A combination the user chose is left alone, even on the same keys.
    let chosen = "[input_release]\nctrl = true\nshift = true\nalt = false\nkey = \"F2\"\n";
    let kept = GuiPrefs::load_with(&path, |_| Ok(chosen.to_string()));
    assert_eq!(
        kept.input_release,
        KeyBinding::new(true, true, false, false, "F2")
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
/// the guest's: the ReSonique II's output stage and the PC speaker's leg are
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
