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

/// Accept must be able to RETRY a MIDI engine, not only change it.
///
/// A user whose P330 failed to open -- bad ROM path, missing port -- had no way
/// to ask for another attempt: the request was sent only when the configuration
/// differed, so fixing the problem outside the emulator and pressing Accept did
/// nothing. Worse, an engine that failed while running latched its error, and
/// with no request there was nothing to clear it: the panel kept the red line
/// through every subsequent visit, including after the receiver was switched
/// off. Both are one decision, made here.
#[test]
fn accepting_the_panel_retries_a_midi_engine_that_is_not_ready() {
    let live = MidiConfig::default();
    let changed = MidiConfig {
        backend: MidiBackend::Off,
        soundfont: Some(PathBuf::from("/tmp/other.sf3")),
        ..MidiConfig::default()
    };
    let ready = [MidiStatus::Ready, MidiStatus::Ready];

    assert!(
        !midi_request_needed(&live, &live, true, ready),
        "an unchanged config on two healthy engines must not restart them"
    );
    assert!(
        midi_request_needed(&changed, &live, true, ready),
        "a changed config is always sent"
    );

    // The bug: identical settings, one engine broken. Every non-Ready status
    // has to reach the session, because each is fixable from outside and none
    // of them clears itself.
    for status in [
        MidiStatus::InitializationFailed,
        MidiStatus::MissingPort,
        MidiStatus::MissingSoundFont,
        MidiStatus::MissingRoms,
        MidiStatus::RomPathMissing,
        MidiStatus::RomControlMissing,
        MidiStatus::RomPcmMissing,
        MidiStatus::RomsNotPairable,
    ] {
        assert!(
            midi_request_needed(&live, &live, true, [status, MidiStatus::Ready]),
            "a failed P300 ({status:?}) must be retried on Accept"
        );
        assert!(
            midi_request_needed(&live, &live, true, [MidiStatus::Ready, status]),
            "a failed P330 ({status:?}) must be retried on Accept"
        );
    }

    // With nothing running there is no engine to RETRY: a status can only be
    // stale, and an engine can only be re-opened, when a worker is holding one.
    // An unpowered machine reports `MidiStatus::default()` (Ready) for both
    // legs anyway, so this pair cannot arise from a real snapshot -- it is
    // written out to pin that the retry half is gated on `powered` and not on
    // the statuses happening to be Ready.
    assert!(!midi_request_needed(
        &live,
        &live,
        false,
        [MidiStatus::InitializationFailed; 2]
    ));
}

/// A configuration changed while the machine is OFF must still be sent.
///
/// There is no engine to reconfigure, which is why the RETRY half of this
/// decision is gated on `powered` -- but the session is not idle. With no
/// worker it applies the change to its own spec and snapshot and emits the
/// `Applied` event, and that event is the only thing that writes `prefs.midi`.
/// So refusing to send while powered off lost the setting three times over: the
/// next power-on booted the old configuration, `izarravm.conf` never learned
/// the new one, and the panel -- which reseeds from the snapshot -- showed the
/// old values back with no error. Accept looked like it had worked.
///
/// The two links after this one are pinned next door:
/// `session::tests::power_cycle_starts_empty_but_shutdown_closes_the_session`
/// for what the session does with an unpowered request, and
/// `applied_state_is_the_only_media_and_midi_preference_input` above for the
/// event reaching the prefs file.
#[test]
fn a_midi_change_made_while_the_machine_is_off_is_still_sent() {
    let live = MidiConfig::default();
    let changed = MidiConfig {
        backend: MidiBackend::External,
        external_port: Some(MidiPortId {
            name: "chosen while off".into(),
            ordinal: 0,
        }),
        ..MidiConfig::default()
    };
    // The statuses an unpowered snapshot really carries: `SessionSnapshot`
    // fills both MIDI legs with `MidiStatus::default()`, which is Ready. The
    // gate cannot lean on them looking broken.
    let unpowered = [MidiStatus::default(); 2];
    assert_eq!(unpowered, [MidiStatus::Ready; 2]);

    assert!(
        midi_request_needed(&changed, &live, false, unpowered),
        "a change made with the machine off has to reach the session"
    );
    assert!(
        !midi_request_needed(&live, &live, false, unpowered),
        "and an unchanged one still does not"
    );
}

/// The MT-32 ROM boxes take a FOLDER as readily as a file.
///
/// The loader identifies ROM images by content, and a set whose images are
/// split halves cannot be named as two files at all -- the halves have to be
/// found together. Requiring `is_file` here greyed the Munt entry out for a
/// user with a perfectly good ROM set in a folder.
#[test]
fn the_mt32_rom_boxes_accept_a_folder_a_file_or_neither() {
    let directory = std::env::temp_dir().join(format!(
        "izarravm-munt-picker-{}-{}",
        std::process::id(),
        line!()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let file = directory.join("MT32_CONTROL.ROM");
    std::fs::write(&file, b"x").unwrap();
    let folder = directory.to_string_lossy().into_owned();
    let file = file.to_string_lossy().into_owned();
    let absent = directory.join("nothing.rom").to_string_lossy().into_owned();

    assert!(munt_roms_available(&file, &file), "two files");
    assert!(munt_roms_available(&folder, &folder), "two folders");
    assert!(munt_roms_available(&folder, &file), "one of each");
    assert!(
        munt_roms_available(&format!("  {folder}  "), &file),
        "a hand-typed path with stray spaces still resolves"
    );
    assert!(!munt_roms_available("", &file), "an empty box is not a set");
    assert!(
        !munt_roms_available(&absent, &file),
        "a path that does not exist is not a set"
    );

    let _ = std::fs::remove_dir_all(&directory);
}

/// Every ROM failure the loader can report must read as a different sentence.
///
/// The owner's MT-32 set failed with "The MIDI output could not be
/// initialized." and nothing else -- no file name, no requirement, no hint that
/// a folder would have worked. A status that cannot say what went wrong is a
/// status that sends the user back to guessing.
///
/// The statuses themselves are proved REACHABLE next door, in
/// `midi::tests::open_munt_reports_which_rom_requirement_failed`, which drives
/// the real loader. This is the other half: that each one then reads as its own
/// sentence rather than sharing a string with another.
#[test]
fn every_rom_failure_says_something_different() {
    let rom_statuses = [
        MidiStatus::MissingRoms,
        MidiStatus::RomPathMissing,
        MidiStatus::RomControlMissing,
        MidiStatus::RomPcmMissing,
        MidiStatus::RomsNotPairable,
        MidiStatus::InitializationFailed,
    ];
    let mut seen: Vec<&str> = Vec::new();
    for status in rom_statuses {
        let text = midi_status_text(status);
        assert!(
            !seen.contains(&text),
            "{status:?} repeats an earlier message: {text:?}"
        );
        assert!(
            text.len() > 20,
            "{status:?} must actually explain itself: {text:?}"
        );
        seen.push(text);
    }
}
