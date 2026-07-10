// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn munt_test_dir(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "izarravm-munt-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn cli_parses_munt_roms_and_stable_external_port_identity() {
    let cli = Cli::try_parse_from([
        "izarravm",
        "--midi-backend",
        "munt",
        "--midi-port",
        "USB MIDI",
        "--midi-port-ordinal",
        "2",
        "--mt32-control-rom",
        "control.rom",
        "--mt32-pcm-rom",
        "pcm.rom",
    ])
    .unwrap();

    assert_eq!(cli.midi_backend, Some(MidiBackend::Munt));
    assert_eq!(cli.midi_port.as_deref(), Some("USB MIDI"));
    assert_eq!(cli.midi_port_ordinal, Some(2));
    assert_eq!(
        cli.mt32_control_rom.as_deref(),
        Some(std::path::Path::new("control.rom"))
    );
    assert_eq!(
        cli.mt32_pcm_rom.as_deref(),
        Some(std::path::Path::new("pcm.rom"))
    );
}

#[test]
fn saved_midi_preferences_fill_only_keys_absent_from_cli_and_toml() {
    let mut config = MidiConfig::default();
    let saved = MidiConfig {
        backend: MidiBackend::Munt,
        external_port: Some(MidiPortId {
            name: "USB MIDI".into(),
            ordinal: 2,
        }),
        soundfont: Some(PathBuf::from("saved.sf3")),
        mt32_control_rom: Some(PathBuf::from("saved-control.rom")),
        mt32_pcm_rom: Some(PathBuf::from("saved-pcm.rom")),
    };
    merge_saved_midi(
        &mut config,
        &saved,
        MidiConfigPresence {
            backend: true,
            soundfont: true,
            ..MidiConfigPresence::default()
        },
    );

    assert_eq!(config.backend, MidiBackend::Off);
    assert_eq!(config.soundfont, None);
    assert_eq!(config.external_port, saved.external_port);
    assert_eq!(config.mt32_control_rom, saved.mt32_control_rom);
    assert_eq!(config.mt32_pcm_rom, saved.mt32_pcm_rom);
}

#[test]
fn munt_discovery_is_case_insensitive_and_prefers_mt32() {
    let dir = munt_test_dir("prefer-mt32");
    let mt_control = dir.join("mt32_control.rom");
    let mt_pcm = dir.join("Mt32_Pcm.Rom");
    for name in [
        &mt_control,
        &mt_pcm,
        &dir.join("CM32L_CONTROL.ROM"),
        &dir.join("CM32L_PCM.ROM"),
    ] {
        std::fs::write(name, b"rom").unwrap();
    }

    let mut config = MidiConfig::default();
    discover_munt_roms(&mut config, &dir);

    assert_eq!(config.mt32_control_rom, Some(mt_control));
    assert_eq!(config.mt32_pcm_rom, Some(mt_pcm));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn munt_discovery_uses_only_complete_pairs() {
    let dir = munt_test_dir("complete-pairs");
    std::fs::write(dir.join("MT32_CONTROL.ROM"), b"rom").unwrap();
    let cm_control = dir.join("cm32l_control.rom");
    let cm_pcm = dir.join("cm32l_pcm.rom");
    std::fs::write(&cm_control, b"rom").unwrap();
    std::fs::write(&cm_pcm, b"rom").unwrap();

    let mut config = MidiConfig::default();
    discover_munt_roms(&mut config, &dir);
    assert_eq!(config.mt32_control_rom, Some(cm_control));
    assert_eq!(config.mt32_pcm_rom, Some(cm_pcm.clone()));

    std::fs::remove_file(cm_pcm).unwrap();
    let mut incomplete = MidiConfig::default();
    discover_munt_roms(&mut incomplete, &dir);
    assert_eq!(incomplete.mt32_control_rom, None);
    assert_eq!(incomplete.mt32_pcm_rom, None);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn munt_discovery_does_not_mix_with_configured_paths() {
    let dir = munt_test_dir("configured-paths");
    std::fs::write(dir.join("MT32_CONTROL.ROM"), b"rom").unwrap();
    std::fs::write(dir.join("MT32_PCM.ROM"), b"rom").unwrap();
    let explicit = PathBuf::from("custom-control.rom");
    let mut config = MidiConfig {
        mt32_control_rom: Some(explicit.clone()),
        ..MidiConfig::default()
    };

    discover_munt_roms(&mut config, &dir);

    assert_eq!(config.mt32_control_rom, Some(explicit));
    assert_eq!(config.mt32_pcm_rom, None);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn midi_presence_tracks_each_explicit_toml_and_cli_key() {
    let path = std::env::temp_dir().join(format!(
        "izarravm-midi-presence-{}-{}.toml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(
        &path,
        r#"
            [audio.midi]
            mt32_control_rom = "control.rom"

            [audio.midi.external_port]
            name = "USB MIDI"
            ordinal = 1

        "#,
    )
    .unwrap();
    let cli = Cli::try_parse_from([
        "izarravm",
        "--config",
        path.to_str().unwrap(),
        "--midi-backend",
        "external",
        "--soundfont",
        "cli.sf3",
    ])
    .unwrap();
    let presence = midi_config_presence(&cli).unwrap();
    let _ = std::fs::remove_file(path);

    assert_eq!(
        presence,
        MidiConfigPresence {
            backend: true,
            external_port: true,
            soundfont: true,
            mt32_control_rom: true,
            mt32_pcm_rom: false,
        }
    );
}

#[test]
fn ascii_to_set1_maps_a_letter_to_make_and_break() {
    assert_eq!(ascii_to_set1('h'), vec![0x23, 0xa3]);
    // Uppercase wraps the key in left-Shift make/break.
    assert_eq!(ascii_to_set1('H'), vec![0x2a, 0x23, 0xa3, 0xaa]);
    // Enter is the unshifted return key.
    assert_eq!(ascii_to_set1('\r'), vec![0x1c, 0x9c]);
    // A shifted number-row glyph holds Shift over the digit key.
    assert_eq!(ascii_to_set1('!'), vec![0x2a, 0x02, 0x82, 0xaa]);
    // Characters with no US-layout key produce nothing.
    assert!(ascii_to_set1('\u{00f1}').is_empty());
}

#[test]
fn write_framebuffer_ppm_uses_the_active_distira_scanout() {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine.write_physical_u32(
        izarravm_machine::DISTIRA_MMIO_BASE + izarravm_video::DISTIRA_REG_CLEAR_COLOR as u32,
        0x0010_2030,
    );
    machine.write_physical_u32(
        izarravm_machine::DISTIRA_MMIO_BASE + izarravm_video::DISTIRA_REG_COMMAND as u32,
        izarravm_video::DISTIRA_CMD_CLEAR,
    );
    machine.write_physical_u32(
        izarravm_machine::DISTIRA_MMIO_BASE + izarravm_video::DISTIRA_REG_COMMAND as u32,
        izarravm_video::DISTIRA_CMD_SWAP,
    );
    assert_eq!(machine.active_display(), ActiveDisplay::Distira);

    let dir = std::env::temp_dir().join(format!(
        "izarravm_ppm_test_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join("frame.ppm");

    write_framebuffer_ppm(&mut machine, &path).expect("write ppm");
    let bytes = std::fs::read(&path).expect("read ppm back");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(bytes.starts_with(b"P6\n"));
    let header = String::from_utf8_lossy(&bytes[..32.min(bytes.len())]);
    let mut parts = header.split_whitespace();
    assert_eq!(parts.next(), Some("P6"));
    let width: usize = parts.next().unwrap().parse().unwrap();
    let height: usize = parts.next().unwrap().parse().unwrap();
    assert_eq!((width, height), (640, 480));
}

#[test]
fn langid_maps_to_guest_layout_index() {
    assert_eq!(layout_index_from_langid(0x0409), 0); // en-US
    assert_eq!(layout_index_from_langid(0x0809), 1); // en-GB
    assert_eq!(layout_index_from_langid(0x1009), 0); // en-CA -> US
    assert_eq!(layout_index_from_langid(0x0c0a), 2); // es-ES
    assert_eq!(layout_index_from_langid(0x080a), 16); // es-MX -> Latin America
    assert_eq!(layout_index_from_langid(0x040c), 3); // fr-FR
    assert_eq!(layout_index_from_langid(0x0407), 4); // de-DE
    assert_eq!(layout_index_from_langid(0x0410), 5); // it-IT
    assert_eq!(layout_index_from_langid(0x0411), 0); // ja-JP -> US fallback
}

#[test]
fn langid_maps_new_layouts() {
    assert_eq!(layout_index_from_langid(0x080c), 6); // fr-BE -> BE
    assert_eq!(layout_index_from_langid(0x0c0c), 7); // fr-CA -> CF
    assert_eq!(layout_index_from_langid(0x0406), 8); // da-DK -> DK
    assert_eq!(layout_index_from_langid(0x0413), 9); // nl-NL -> NL
    assert_eq!(layout_index_from_langid(0x0414), 10); // nb-NO -> NO
    assert_eq!(layout_index_from_langid(0x0816), 11); // pt-PT -> PO
    assert_eq!(layout_index_from_langid(0x100c), 12); // fr-CH -> SF
    assert_eq!(layout_index_from_langid(0x0807), 13); // de-CH -> SG
    assert_eq!(layout_index_from_langid(0x040b), 14); // fi-FI -> SU
    assert_eq!(layout_index_from_langid(0x041d), 15); // sv-SE -> SV
}

#[test]
fn codepage_index_for_each_layout() {
    let want = [0u8, 0, 1, 1, 1, 1, 1, 3, 4, 1, 4, 2, 1, 1, 1, 1, 1];
    for (i, w) in want.iter().enumerate() {
        assert_eq!(codepage_index_for_layout(i as u8), *w);
    }
}

#[test]
fn katea_run_prog_name_picks_a_clean_8_3_name() {
    use std::path::Path;
    assert_eq!(katea_run_prog_name(Path::new("/x/FOO.EXE")), "PROG.EXE");
    assert_eq!(katea_run_prog_name(Path::new("bar.com")), "PROG.COM");
    assert_eq!(katea_run_prog_name(Path::new("noext")), "PROG.COM");
    assert_eq!(katea_run_prog_name(Path::new("a.longext")), "PROG.LON");
}

#[test]
fn c_root_path_lives_under_dot_izarravm_when_not_portable() {
    let p = super::c_root_path(false);
    assert!(
        p.ends_with(std::path::Path::new(".izarravm").join("c_drive")),
        "default C: root should end with .izarravm/c_drive, got {p:?}"
    );
    assert_eq!(p, state_dir_path().join("c_drive"));
}

#[test]
fn c_root_path_is_a_bare_c_drive_when_portable() {
    // Portable mode keys off the executable's own directory, so the path is
    // just <exe_dir>/c_drive — no ~/.izarravm prefix.
    let p = super::c_root_path(true);
    assert_eq!(
        p.file_name().and_then(|n| n.to_str()),
        Some("c_drive"),
        "portable C: root should be a c_drive folder, got {p:?}"
    );
    assert!(
        !p.to_string_lossy().contains(".izarravm"),
        "portable C: root must not use the ~/.izarravm prefix, got {p:?}"
    );
}

#[test]
fn boot_suite_failure_summary_lists_every_failed_record() {
    let mut results = SuiteResults {
        version: 1,
        declared_record_count: 3,
        payload_len: 0,
        checksum: 0,
        records: vec![
            izarravm_firmware::SuiteRecord {
                status: SuiteRecordStatus::Begin,
                name: "suite.boot".to_string(),
                value: None,
            },
            izarravm_firmware::SuiteRecord {
                status: SuiteRecordStatus::Fail,
                name: "sound.opl3".to_string(),
                value: None,
            },
            izarravm_firmware::SuiteRecord {
                status: SuiteRecordStatus::Fail,
                name: "timer.irq0".to_string(),
                value: None,
            },
        ],
    };

    assert_eq!(
        boot_suite_failure_summary(&results).as_deref(),
        Some("boot suite reported FAIL: sound.opl3, timer.irq0")
    );
    results
        .records
        .retain(|record| record.status != SuiteRecordStatus::Fail);
    assert_eq!(boot_suite_failure_summary(&results), None);
}
