// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_input::{JoystickAxis, JoystickAxisBinding, JoystickButton, JoystickPolarity};

fn test_joystick_binding(name: &str) -> JoystickBinding {
    JoystickBinding {
        controller_uuid: format!("uuid-{name}"),
        controller_name: name.into(),
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
fn cpu_mode_label_preserves_fractional_clock_rates() {
    assert_eq!(
        cpu_mode_label(GswMode::Gsw386Slow),
        "GSW-586 - 386-slow mode - 7.33 MHz"
    );
    assert_eq!(
        cpu_mode_label(GswMode::Gsw586),
        "GSW-586 - 586 mode - 166 MHz"
    );
}

#[test]
fn parent_accept_activates_only_the_staged_joystick_binding() {
    let original = test_joystick_binding("Original");
    let replacement = test_joystick_binding("Replacement");
    let mut live = Some(original);
    let mut prefs = GuiPrefs::default();
    let mut last_sent = Some(Some(JoystickSample {
        x: 128,
        y: 128,
        buttons: 0,
    }));

    apply_joystick_binding(
        &mut live,
        &mut prefs,
        &Some(replacement.clone()),
        &mut last_sent,
    );

    assert_eq!(live, Some(replacement.clone()));
    assert_eq!(prefs.joystick_binding, Some(replacement));
    assert_eq!(last_sent, None, "new binding must be injected immediately");
}

#[test]
fn wizard_and_parent_cancellation_leave_the_live_binding_unchanged() {
    let original = test_joystick_binding("Original");
    let live = Some(original.clone());
    let partial_wizard = JoystickWizard::default();
    drop(partial_wizard);
    let staged = Some(test_joystick_binding("Replacement"));
    drop(staged);

    assert_eq!(live, Some(original));
}

#[test]
fn munt_selection_requires_two_existing_rom_files() {
    let dir = std::env::temp_dir().join(format!(
        "izarravm-munt-ui-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let control = dir.join("control.rom");
    let pcm = dir.join("pcm.rom");
    let control_text = control.to_string_lossy();
    let pcm_text = pcm.to_string_lossy();

    assert!(!munt_roms_available("", ""));
    assert!(!munt_roms_available(&control_text, &pcm_text));
    std::fs::write(&control, b"control").unwrap();
    assert!(!munt_roms_available(&control_text, &pcm_text));
    std::fs::write(&pcm, b"pcm").unwrap();
    assert!(munt_roms_available(&control_text, &pcm_text));

    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn volume_gain_is_cubic_and_clamped() {
    // Endpoints are exact: silence at 0, unity at full.
    assert_eq!(volume_gain(0.0), 0.0);
    assert_eq!(volume_gain(1.0), 1.0);
    // Halfway on the slider is 0.5^3 = 0.125 of linear gain.
    assert!((volume_gain(0.5) - 0.125).abs() < 1e-6);
    // 0.8 (the default) cubes to 0.512.
    assert!((volume_gain(0.8) - 0.512).abs() < 1e-6);
    // Out-of-range input is clamped before cubing.
    assert_eq!(volume_gain(-1.0), 0.0);
    assert_eq!(volume_gain(2.0), 1.0);
}

#[test]
fn cd_volume_mapping_links_live_stereo_levels() {
    assert_eq!(cd_level_percent(0, 0), 0);
    assert_eq!(cd_level_percent(31, 31), 100);
    assert_eq!(cd_level_percent(31, 0), 50);
    assert_eq!(cd_percent_level(0), 0);
    assert_eq!(cd_percent_level(100), 31);
    assert_eq!(cd_percent_level(50), 16);
}

#[test]
fn cd_eject_uses_live_media_state_instead_of_a_host_label() {
    let loaded = CdAudioState {
        media_present: true,
        ..CdAudioState::default()
    };
    assert!(cd_eject_enabled(true, loaded));
    assert!(!cd_eject_enabled(false, loaded));
    assert!(!cd_eject_enabled(true, CdAudioState::default()));
}

#[test]
fn initial_cd_source_preserves_explicit_image_precedence_without_fallback() {
    let dir =
        std::env::temp_dir().join(format!("izarravm-initial-cd-source-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let saved_image = dir.join("saved.iso");
    let saved_folder = dir.join("saved-folder");
    let explicit = dir.join("missing-explicit.iso");
    std::fs::write(&saved_image, []).unwrap();
    std::fs::create_dir_all(&saved_folder).unwrap();
    let mut prefs = GuiPrefs {
        last_cd_image: Some(saved_image.clone()),
        last_cd_folder: Some(saved_folder.clone()),
        ..GuiPrefs::default()
    };

    assert_eq!(
        initial_cd_source(Some(explicit.clone()), &prefs),
        Some(CdSource::Image(explicit))
    );
    assert_eq!(
        initial_cd_source(None, &prefs),
        Some(CdSource::Image(saved_image.clone()))
    );
    std::fs::remove_file(saved_image).unwrap();
    assert_eq!(
        initial_cd_source(None, &prefs),
        Some(CdSource::Folder(saved_folder))
    );
    prefs.last_cd_folder = None;
    assert_eq!(initial_cd_source(None, &prefs), None);

    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn applied_state_is_the_only_media_and_midi_preference_input() {
    let floppy = PathBuf::from("disk.img");
    let cd = PathBuf::from("disc.iso");
    let mut prefs = GuiPrefs::default();

    assert!(apply_session_preference(
        &mut prefs,
        AppliedState::Floppy(Some(FloppySource(floppy.clone())))
    ));
    assert_eq!(prefs.last_floppy_image, Some(floppy));
    assert!(apply_session_preference(
        &mut prefs,
        AppliedState::Cd(Some(CdSource::Image(cd.clone())))
    ));
    assert_eq!(prefs.last_cd_image, Some(cd));
    assert_eq!(prefs.last_cd_folder, None);

    let midi = MidiConfig {
        backend: MidiBackend::External,
        ..MidiConfig::default()
    };
    assert!(apply_session_preference(
        &mut prefs,
        AppliedState::Midi(midi.clone())
    ));
    assert_eq!(prefs.midi, midi);
    assert!(!apply_session_preference(&mut prefs, AppliedState::Other));

    assert!(apply_session_preference(
        &mut prefs,
        AppliedState::Floppy(None)
    ));
    assert!(apply_session_preference(&mut prefs, AppliedState::Cd(None)));
    assert_eq!(prefs.last_floppy_image, None);
    assert_eq!(prefs.last_cd_image, None);
}

#[test]
fn logo_recolor_maps_background_to_beige_and_keeps_ink() {
    // One pure-background pixel and one pure-black-ink pixel, both opaque.
    let raw = [236u8, 230, 223, 255, 0, 0, 0, 255];
    let out = recolor_logo(&raw, PANEL_FACE_F32);
    // Background becomes the exact panel beige.
    assert_eq!(&out[0..4], &[205u8, 195, 164, 255]);
    // Ink is untouched (background coverage is zero).
    assert_eq!(&out[4..8], &[0u8, 0, 0, 255]);
}

#[test]
fn framebuffer_words_pack_into_rgba() {
    let words = [0, 0x00AB_CDEF, 0, 0x00AB_CDEF];
    let rgba = words_to_rgba(&words, 2, 2);
    assert_eq!(rgba.len(), 16);
    // Pixel 1 is 0x00ABCDEF -> R=AB, G=CD, B=EF, A=FF.
    assert_eq!(
        (rgba[4], rgba[5], rgba[6], rgba[7]),
        (0xAB, 0xCD, 0xEF, 0xFF)
    );
}

#[test]
fn star_icon_is_red_in_the_centre_and_clear_in_the_corner() {
    let size = 64u32;
    let rgba = render_star_icon(size, [0xC7, 0x44, 0x46]);
    assert_eq!(rgba.len(), (size * size * 4) as usize);
    let center = ((size / 2 * size + size / 2) * 4) as usize;
    assert_eq!(&rgba[center..center + 4], &[0xC7u8, 0x44, 0x46, 0xFF]);
    // Top-left corner is outside the star, fully transparent.
    assert_eq!(&rgba[0..4], &[0u8, 0, 0, 0]);
}
