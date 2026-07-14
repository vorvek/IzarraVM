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
fn cli_accepts_explicit_interpreter_backend() {
    let cli = Cli::try_parse_from(["izarravm", "--interpreter"]).unwrap();
    assert!(cli.interpreter);
}

#[test]
fn execution_backend_requires_avx2_only_for_native_builds() {
    assert_eq!(
        requested_execution_backend(false, true, true),
        Ok(ExecutionBackend::Automatic)
    );
    assert!(
        requested_execution_backend(false, true, false)
            .unwrap_err()
            .contains("AVX2")
    );
    assert_eq!(
        requested_execution_backend(true, true, false),
        Ok(ExecutionBackend::Interpreter)
    );
    assert_eq!(
        requested_execution_backend(false, false, false),
        Ok(ExecutionBackend::Interpreter)
    );
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
#[test]
#[allow(clippy::assertions_on_constants)]
fn windows_build_keeps_avx2_runtime_detectable() {
    assert!(
        !cfg!(target_feature = "avx2"),
        "the portable host binary must not enable AVX2 at compile time"
    );
}

#[test]
fn cli_accepts_hdd_folder_profile_json_output() {
    let cli = Cli::try_parse_from([
        "izarravm",
        "--hdd-folder",
        "game",
        "--profile-json",
        "results/run.json",
        "--expect-test-exit",
    ])
    .unwrap();

    assert_eq!(cli.hdd_folder.as_deref(), Some(Path::new("game")));
    assert_eq!(
        cli.profile_json.as_deref(),
        Some(Path::new("results/run.json"))
    );
    assert!(cli.expect_test_exit);
}

#[test]
fn machine_profile_environment_gate_is_explicit() {
    for value in [None, Some(""), Some("0")] {
        assert!(!machine_profile_requested(value));
    }
    for value in [Some("1"), Some("true"), Some("enabled")] {
        assert!(machine_profile_requested(value));
    }
}

#[test]
fn cli_rejects_multiple_run_modes() {
    for arguments in [
        vec![
            "izarravm",
            "--hdd-folder",
            "game",
            "--headless-profile-exe",
            "probe.exe",
        ],
        vec!["izarravm", "--hdd-folder", "game", "--headless-bench"],
    ] {
        assert!(Cli::try_parse_from(arguments).is_err());
    }
}

#[test]
fn hdd_profile_json_reports_fixed_time_and_native_metrics() {
    let dir = munt_test_dir("hdd-profile");
    let path = dir.join("run.json");
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = GswMode::Gsw486;
    let mut machine = Machine::new_raw_program(profile, &[0xb8, 0x00, 0x4c, 0xcd, 0x21])
        .expect("build raw machine");
    machine.enable_machine_profiling();
    let stop = machine
        .run_until_halt_or_cycles(100_000)
        .expect("run raw program");

    write_hdd_profile_json(
        &path,
        Path::new("fixture"),
        GswMode::Gsw486,
        100_000,
        std::time::Duration::from_secs(1),
        &stop,
        Some((35, 70)),
        &machine,
    )
    .expect("write profile JSON");

    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(report["schema"], "izarravm-hdd-profile-v1");
    assert_eq!(report["mode"], "486");
    assert_eq!(report["stop"]["kind"], "dos_exit");
    assert_eq!(report["stop"]["code"], 0);
    assert_eq!(report["timedemo"]["gametics"], 35);
    assert_eq!(report["machine_phase_timing_enabled"], true);
    assert!(report["guest_seconds"].as_f64().unwrap() > 0.0);
    assert!(report["direct_native_coverage"].as_f64().is_some());
    assert!(
        report["direct_slow_exits_per_100_instructions"]
            .as_f64()
            .is_some()
    );
    assert!(report["perf"]["instructions"].as_u64().unwrap() > 0);
    assert!(report["raw_bus_clocks"].as_u64().unwrap() > 0);
    assert!(report["perf"]["jit_direct_entries"].as_u64().is_some());
    assert!(report["perf"]["jit_direct_insns"].as_u64().is_some());
    assert!(report["perf"]["jit_direct_side_exits"].as_u64().is_some());
    for field in [
        "jit_direct_exit_cross_page_or_alignment",
        "jit_direct_exit_unavailable_or_kind",
        "jit_direct_exit_permission",
        "jit_direct_exit_code_watch",
        "jit_direct_exit_other",
        "jit_direct_unresolved_static_unbound",
        "jit_direct_unresolved_static_hidden",
        "jit_direct_unresolved_dynamic_miss_or_unbound",
        "jit_direct_unresolved_dynamic_hidden",
        "jit_direct_decode_dependencies_scanned",
        "jit_direct_portals_hidden",
        "jit_direct_arena_compactions",
        "jit_direct_arena_compaction_live_blocks",
        "jit_direct_arena_compaction_bytes",
        "jit_direct_arena_compaction_failures",
        "jit_direct_reject_observer",
        "jit_direct_reject_interrupt_shadow",
        "jit_direct_reject_aggregate_accounting",
        "jit_direct_reject_mode_key",
        "jit_direct_reject_x87_top",
        "jit_direct_reject_cs_layout",
        "jit_direct_reject_cpl",
        "jit_direct_reject_data_segment",
        "jit_direct_reject_alignment",
        "jit_direct_reject_fetch_limit",
        "jit_direct_reject_zero_budget",
    ] {
        assert!(report["perf"][field].as_u64().is_some());
    }
    assert!(report["classified_wall_ns"].as_u64().is_some());
    assert!(report["unattributed_wall_ns"].as_u64().is_some());
    assert!(
        report["classified_wall_ns"].as_u64().unwrap()
            <= std::time::Duration::from_secs(1).as_nanos() as u64
    );
    assert!(
        report["machine_phases"]
            .as_array()
            .unwrap()
            .iter()
            .any(|phase| phase["name"] == "cpu_batch" && phase["count"].as_u64().unwrap() > 0)
    );
    for name in ["video_conversion", "audio_render"] {
        assert!(
            report["machine_phases"]
                .as_array()
                .unwrap()
                .iter()
                .any(|phase| phase["name"] == name)
        );
    }

    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn hdd_profile_json_does_not_enable_machine_phase_timing() {
    let dir = munt_test_dir("hdd-profile-uninstrumented");
    let path = dir.join("run.json");
    let mut machine = Machine::new_raw_program(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        &[0xb8, 0x00, 0x4c, 0xcd, 0x21],
    )
    .expect("build raw machine");
    let stop = machine
        .run_until_halt_or_cycles(100_000)
        .expect("run raw program");

    write_hdd_profile_json(
        &path,
        Path::new("fixture"),
        GswMode::Gsw386,
        100_000,
        std::time::Duration::from_secs(1),
        &stop,
        None,
        &machine,
    )
    .expect("write profile JSON");

    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(report["machine_phase_timing_enabled"], false);
    assert_eq!(report["classified_wall_ns"], 0);
    assert!(
        report["machine_phases"]
            .as_array()
            .unwrap()
            .iter()
            .all(|phase| phase["wall_ns"] == 0 && phase["count"] == 0)
    );

    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn stop_reason_json_preserves_each_outcome() {
    let cases = [
        (StopReason::Halted, json!({ "kind": "halted" })),
        (
            StopReason::CycleLimit { requested: 123 },
            json!({ "kind": "cycle_limit", "requested": 123 }),
        ),
        (
            StopReason::CpuError("bad opcode".into()),
            json!({ "kind": "cpu_error", "message": "bad opcode" }),
        ),
        (
            StopReason::DosExit { code: 7 },
            json!({ "kind": "dos_exit", "code": 7 }),
        ),
        (
            StopReason::TestExit { code: 9 },
            json!({ "kind": "test_exit", "code": 9 }),
        ),
    ];

    for (stop, expected) in cases {
        assert_eq!(stop_reason_json(&stop), expected);
    }
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

#[test]
fn state_glide_ovl_discovery_is_case_insensitive() {
    let dir = std::env::temp_dir().join(format!(
        "izarravm-glide-ovl-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    assert_eq!(load_state_glide_ovl(&dir), None);
    std::fs::write(dir.join("gLiDe2x.oVl"), b"state fallback").unwrap();
    assert_eq!(load_state_glide_ovl(&dir), Some(b"state fallback".to_vec()));
    std::fs::remove_dir_all(dir).ok();
}

#[cfg(feature = "jit")]
#[test]
fn unit_sim_report_lines_format_headline_and_histogram() {
    let report = izarravm_cpu::SimReport {
        entries: 1000,
        retired_in_units: 3500,
        linked_transfers: 42,
        unresolved_exits: 7,
        side_exits_io: 3,
        side_exits_async: 2,
        sim_invalidations: 5,
        units_built: 120,
        units_rebuilt: 4,
    };
    // Ten units: member counts 1..=10 spread across code/UMA/BIOS entry pages. p50 (nearest rank of
    // 10 items) lands on the 5th smallest (5), p90 on the 9th (9), max 10. Two units sit in the
    // excluded window (page 0xF0 and page 0xA0); one code unit exceeds 64 members.
    let histogram: Vec<(usize, u32)> = vec![
        (1, 0x0010),
        (2, 0x0010),
        (3, 0x0010),
        (4, 0x0011),
        (5, 0x0011),
        (6, 0x0012),
        (7, 0x00f0),
        (8, 0x00a0),
        (9, 0x0012),
        (65, 0x0013),
    ];
    let lines = unit_sim_report_lines(&report, &histogram);
    assert_eq!(
        lines,
        vec![
            "unit_sim entries=1000 retired_in_units=3500 linked_transfers=42 unresolved_exits=7 \
side_exits_io=3 side_exits_async=2 sim_invalidations=5 units_built=120 units_rebuilt=4 \
insns_per_entry=3.500000"
                .to_string(),
            "unit_sim_hist units=10 members_p50=5 members_p90=9 members_max=65 units_over_64=1 \
units_over_128=0 units_over_256=0 excl_units=2"
                .to_string(),
        ]
    );
}

#[cfg(feature = "jit")]
#[test]
fn unit_sim_report_lines_handle_empty_run() {
    let report = izarravm_cpu::SimReport::default();
    let lines = unit_sim_report_lines(&report, &[]);
    assert_eq!(
        lines,
        vec![
            "unit_sim entries=0 retired_in_units=0 linked_transfers=0 unresolved_exits=0 \
side_exits_io=0 side_exits_async=0 sim_invalidations=0 units_built=0 units_rebuilt=0 \
insns_per_entry=0.000000"
                .to_string(),
            "unit_sim_hist units=0 members_p50=0 members_p90=0 members_max=0 units_over_64=0 \
units_over_128=0 units_over_256=0 excl_units=0"
                .to_string(),
        ]
    );
}
