// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn controller_config(name: &str) -> ControllerConfig {
    ControllerConfig::default_keyboard(ControllerDeviceMatcher {
        backend: "test".into(),
        platform: "test".into(),
        guid: format!("guid-{name}"),
        vendor_id: Some(1),
        product_id: Some(2),
        name: name.into(),
        occurrence: 0,
    })
}

#[test]
fn midi_rom_selection_is_visible_only_for_munt() {
    assert!(!midi_rom_selection_visible(MidiBackend::Off));
    assert!(!midi_rom_selection_visible(MidiBackend::External));
    assert!(midi_rom_selection_visible(MidiBackend::Munt));
}

#[test]
fn controller_setup_saves_only_complete_or_cleared_selections() {
    assert!(controller_setup_can_save(Some("Quake"), true));
    assert!(controller_setup_can_save(None, false));
    assert!(!controller_setup_can_save(Some("Missing"), false));
    assert!(!controller_setup_can_save(None, true));
}

#[test]
fn inline_controller_mapping_migrates_to_a_selected_profile() {
    let state_dir = std::env::temp_dir().join(format!(
        "izarravm-controller-migration-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = ControllerProfileStore::new(&state_dir);
    let config = controller_config("Legacy controller");
    let mut prefs = GuiPrefs {
        controller: Some(config.clone()),
        ..GuiPrefs::default()
    };

    let (selected, restored, changed) = restore_controller_profile(&store, &mut prefs);

    assert!(changed);
    assert_eq!(selected.as_deref(), Some("New Profile"));
    assert_eq!(restored, Some(config.clone()));
    assert_eq!(prefs.controller, None);
    assert_eq!(prefs.controller_profile, selected);
    assert_eq!(store.load("New Profile").unwrap(), config);
    let saved = toml::to_string_pretty(&prefs).unwrap();
    assert!(!saved.contains("[controller]"));
    assert!(saved.contains("controller_profile = \"New Profile\""));

    std::fs::remove_dir_all(state_dir).unwrap();
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
fn volume_gain_is_cubic_below_unity_and_clamped() {
    // Endpoints are exact: silence at 0, unity at the 100% detent.
    assert_eq!(volume_gain(0.0), 0.0);
    assert_eq!(volume_gain(1.0), 1.0);
    // Halfway to the detent is 0.5^3 = 0.125 of linear gain.
    assert!((volume_gain(0.5) - 0.125).abs() < 1e-6);
    // 0.8 (the default) cubes to 0.512.
    assert!((volume_gain(0.8) - 0.512).abs() < 1e-6);
    // Negative input is clamped away.
    assert_eq!(volume_gain(-1.0), 0.0);
}

/// The knob amplifies past unity, and the numbers it puts on the mix are the
/// numbers the panel prints.
///
/// The taper below the detent is perceptual and the travel above it is not: a
/// cubic carried past 1.0 would reach 125x at the top of the slider, which is
/// not a speaker knob, it is a fuse. Above unity the reading is literal, so 300%
/// is three times and the label means what it says. Pin both the literal
/// readings and the ceiling.
#[test]
fn volume_gain_amplifies_above_unity_up_to_the_ceiling() {
    assert!(
        volume_gain(2.0) > 1.0,
        "the knob must have travel past line level at all"
    );
    // Literal above the detent: the percent on the panel IS the multiplier.
    assert!((volume_gain(2.0) - 2.0).abs() < 1e-6);
    assert!((volume_gain(3.0) - 3.0).abs() < 1e-6);
    assert!((volume_gain(MAX_VOLUME) - 5.0).abs() < 1e-6);
    // 5x is +14 dB, the worst well-behaved case the ceiling was chosen to
    // recover. A curve that stopped short would leave that case quiet.
    let db = 20.0 * volume_gain(MAX_VOLUME).log10();
    assert!(
        (db - 14.0).abs() < 0.1,
        "top of travel is {db} dB, want +14"
    );
    // Continuous across the detent: no jump in level as the knob passes 100%.
    assert!((volume_gain(0.999) - 1.0).abs() < 0.01);
    // And clamped at the ceiling, so a hand-edited conf cannot ask for more.
    assert_eq!(volume_gain(50.0), MAX_VOLUME);
}

/// The slider's value box is editable, and it has to read back in the units it
/// prints.
///
/// egui's stock parser is a plain float parse. Against a box that displays
/// "80%" it fails twice: it rejects the string the box seeded itself with, so
/// Enter on unedited text does nothing, and it takes a typed `100` at face
/// value. That second one used to be harmless -- 100 clamped to the old ceiling
/// of 1.0, which is what the user meant anyway -- and stopped being harmless the
/// moment the ceiling moved to 5.0, because now typing the neutral setting jumps
/// the knob to five times it. Pin the divide, the suffix, and the rejection.
#[test]
fn the_volume_box_parses_the_percent_it_prints() {
    // The case the wider range broke: 100 means unity, not the number 100.
    assert_eq!(volume_percent_to_fraction("100"), Some(1.0));
    assert_ne!(
        volume_percent_to_fraction("100"),
        Some(100.0),
        "a face-value parse would clamp this to the top of the travel"
    );
    // The string the box seeds itself with must survive a round trip.
    assert_eq!(volume_percent_to_fraction("80%"), Some(0.8));
    // The whole travel, including above unity.
    assert_eq!(volume_percent_to_fraction("0"), Some(0.0));
    assert_eq!(volume_percent_to_fraction("500"), Some(5.0));
    assert_eq!(
        volume_percent_to_fraction("500%"),
        Some(f64::from(MAX_VOLUME))
    );
    // Whitespace either side of the number or the suffix.
    assert_eq!(volume_percent_to_fraction("  250 % "), Some(2.5));
    // Not a number: rejected, so egui keeps the value the box already held.
    for garbage in ["", "%", "loud", "1.2.3", "--5"] {
        assert_eq!(
            volume_percent_to_fraction(garbage),
            None,
            "{garbage:?} must leave the knob where it was"
        );
    }
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
fn cd_transport_buttons_follow_live_playback_state() {
    let audio_disc = CdAudioState {
        media_present: true,
        audio_capable: true,
        ..CdAudioState::default()
    };
    let data_disc = CdAudioState {
        audio_capable: false,
        ..audio_disc
    };
    let playing = CdAudioState {
        playing: true,
        has_next_track: true,
        ..audio_disc
    };
    let last_track = CdAudioState {
        has_next_track: false,
        ..playing
    };
    let paused = CdAudioState {
        playing: false,
        paused: true,
        ..playing
    };

    // Play/pause stays live while playing, because the button then pauses.
    assert!(cd_transport_enabled(true, audio_disc));
    assert!(cd_transport_enabled(true, playing));
    assert!(!cd_transport_enabled(true, data_disc));
    assert!(!cd_transport_enabled(false, playing));

    // Skip needs a track after the play head, so a stopped drive cannot skip.
    assert!(cd_skip_enabled(true, playing));
    assert!(cd_skip_enabled(true, paused));
    assert!(!cd_skip_enabled(true, last_track));
    assert!(!cd_skip_enabled(true, audio_disc));

    assert!(cd_stop_enabled(true, playing));
    assert!(cd_stop_enabled(true, paused));
    assert!(!cd_stop_enabled(true, audio_disc));
    assert!(!cd_stop_enabled(false, playing));
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

fn test_session_frame(update: u64, changed_rows: Vec<std::ops::Range<usize>>) -> SessionFrame {
    SessionFrame {
        words: std::sync::Arc::new(vec![0u32; 4 * 8]),
        changed_rows,
        width: 4,
        height: 8,
        seq: update,
        update_from: update,
        update_to: update,
        generation: 1,
    }
}

/// Two publications, one paint: the damage of the frame that was never painted
/// has to survive into the one that replaced it, or its rows stay stale on the
/// texture with no later frame reporting them again.
#[test]
fn an_unpainted_frame_folds_its_damage_into_the_next_one() {
    let unpainted = test_session_frame(4, [1..2, 6..7].into_iter().collect());
    let newer = test_session_frame(5, std::iter::once(2..3).collect());

    let merged = merge_session_frames(unpainted, newer);

    assert_eq!(
        merged.changed_rows,
        [1..3, 6..7].into_iter().collect::<Vec<_>>()
    );
    assert_eq!((merged.update_from, merged.update_to), (4, 5));
    assert!(
        !crate::crt::upload_is_full(merged.update_from, 3, false),
        "the folded frame continues the chain, so it still uploads by runs"
    );
}

/// A merge is only sound while the two frames describe the same screen. A
/// worker generation change republishes from a fresh machine, so the older
/// frame's rows mean nothing against it -- and the resulting publication gap is
/// what makes the consumer take the new frame whole.
#[test]
fn frames_from_different_generations_do_not_fold() {
    let unpainted = test_session_frame(4, std::iter::once(1..2).collect());
    let mut newer = test_session_frame(5, std::iter::once(2..3).collect());
    newer.generation = 2;

    let merged = merge_session_frames(unpainted, newer);

    assert_eq!(
        merged.changed_rows,
        std::iter::once(2..3).collect::<Vec<_>>()
    );
    assert_eq!(merged.update_from, 5);
    assert!(crate::crt::upload_is_full(merged.update_from, 3, false));
}
