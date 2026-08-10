// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use clap::Parser;
use izarravm_core::{ConfigError, GswMode, MidiBackend, MidiConfig, MidiPortId, VideoCard};
use std::cell::RefCell;
use std::ops::Deref;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

struct StartupScratch(PathBuf);

impl StartupScratch {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "izarravm-startup-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Deref for StartupScratch {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<Path> for StartupScratch {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for StartupScratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn startup_test_dir(label: &str) -> StartupScratch {
    StartupScratch::new(label)
}

fn resolve_midi_fixture(
    root: &Path,
    locations: &StartupLocations,
    label: &str,
    extra_cli: &[&str],
    config_text: &str,
    prefs_text: &str,
) -> ResolvedStartup {
    let config_path = root.join(format!("{label}.toml"));
    let c_drive = root.join(label).join("c_drive");
    let prefs_path = prefs::prefs_path(&c_drive);
    let mut argv = vec![
        "izarravm".to_owned(),
        "--config".to_owned(),
        config_path.to_string_lossy().into_owned(),
        "--c-drive".to_owned(),
        c_drive.to_string_lossy().into_owned(),
    ];
    argv.extend(extra_cli.iter().map(|value| (*value).to_owned()));
    let cli = Cli::try_parse_from(argv).unwrap();

    resolve_with(&cli, locations, |path| {
        if path == config_path {
            Ok(config_text.to_owned())
        } else if path == prefs_path {
            Ok(prefs_text.to_owned())
        } else {
            unreachable!("unexpected startup read: {}", path.display())
        }
    })
    .unwrap()
}

#[test]
fn cli_c_drive_beats_dosroot() {
    let root = startup_test_dir("cli-c-drive");
    let cli = Cli::try_parse_from([
        "izarravm",
        "--c-drive",
        "cli-drive",
        "--dosroot",
        "environment-drive",
    ])
    .unwrap();
    let locations = StartupLocations {
        state_dir: root.join("state"),
        executable_dir: root.join("portable"),
    };

    let resolved = resolve_with(&cli, &locations, |_| {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "fixture has no preferences",
        ))
    })
    .unwrap();

    assert_eq!(resolved.config.dos.c_drive, Path::new("cli-drive"));
}

#[test]
fn c_drive_precedence_is_exact() {
    let root = startup_test_dir("c-drive");
    let locations = StartupLocations {
        state_dir: root.join("state"),
        executable_dir: root.join("portable"),
    };
    let config_path = root.join("settings.toml");
    let config_arg = config_path.to_string_lossy().into_owned();
    let cases = [
        (
            vec![
                "--config",
                &config_arg,
                "--c-drive",
                "cli-drive",
                "--dosroot",
                "dosroot-drive",
            ],
            "toml-drive",
            PathBuf::from("cli-drive"),
        ),
        (
            vec!["--config", &config_arg, "--dosroot", "dosroot-drive"],
            "toml-drive",
            PathBuf::from("dosroot-drive"),
        ),
        (
            vec!["--config", &config_arg],
            "toml-drive",
            PathBuf::from("toml-drive"),
        ),
        (vec!["--config", &config_arg], ".", PathBuf::from(".")),
        (Vec::new(), "unused", locations.state_dir.join("c_drive")),
        (
            vec!["--portable"],
            "unused",
            locations.executable_dir.join("c_drive"),
        ),
    ];

    for (arguments, toml_c_drive, expected) in cases {
        let mut argv = vec!["izarravm"];
        argv.extend(arguments);
        let cli = Cli::try_parse_from(argv).unwrap();
        let config_text = format!("[dos]\nc_drive = {toml_c_drive:?}\n");
        let resolved = resolve_with(&cli, &locations, |path| {
            if path == config_path {
                Ok(config_text.clone())
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "fixture has no preferences",
                ))
            }
        })
        .unwrap();
        assert_eq!(resolved.config.dos.c_drive, expected);
    }

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn midi_sources_resolve_in_precedence_order() {
    let root = startup_test_dir("midi-precedence");
    let state_dir = root.join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let control_rom = state_dir.join("MT32_CONTROL.ROM");
    let pcm_rom = state_dir.join("MT32_PCM.ROM");
    std::fs::write(&control_rom, b"control").unwrap();
    std::fs::write(&pcm_rom, b"pcm").unwrap();
    let locations = StartupLocations {
        state_dir,
        executable_dir: root.join("portable"),
    };
    let config_path = root.join("settings.toml");
    let c_drive = root.join("c_drive");
    let cli = Cli::try_parse_from([
        "izarravm",
        "--config",
        config_path.to_str().unwrap(),
        "--c-drive",
        c_drive.to_str().unwrap(),
        "--midi-backend",
        "off",
    ])
    .unwrap();
    let config_text = r#"
        [audio.midi.external_port]
        name = "TOML port"
        ordinal = 3
    "#;
    let prefs_path = prefs::prefs_path(&c_drive);
    let prefs_text = r#"
        [midi]
        backend = "munt"
        soundfont = "saved.sf3"
    "#;

    let resolved = resolve_with(&cli, &locations, |path| {
        if path == config_path {
            Ok(config_text.to_owned())
        } else if path == prefs_path {
            Ok(prefs_text.to_owned())
        } else {
            unreachable!("unexpected startup read: {}", path.display())
        }
    })
    .unwrap();
    let midi = &resolved.config.audio.midi;

    assert_eq!(midi.backend, MidiBackend::Off);
    assert_eq!(
        midi.external_port,
        Some(MidiPortId {
            name: "TOML port".into(),
            ordinal: 3,
        })
    );
    assert_eq!(midi.soundfont.as_deref(), Some(Path::new("saved.sf3")));
    assert_eq!(
        midi.mt32_control_rom.as_deref(),
        Some(control_rom.as_path())
    );
    assert_eq!(midi.mt32_pcm_rom.as_deref(), Some(pcm_rom.as_path()));

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn midi_precedence_matrix_covers_every_field() {
    let root = startup_test_dir("midi-matrix");
    let state_dir = root.join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::write(state_dir.join("MT32_CONTROL.ROM"), b"control").unwrap();
    std::fs::write(state_dir.join("MT32_PCM.ROM"), b"pcm").unwrap();
    let locations = StartupLocations {
        state_dir,
        executable_dir: root.join("portable"),
    };
    let toml = r#"
        [audio.midi]
        backend = "munt"
        soundfont = "toml.sf3"
        mt32_control_rom = "toml-control.rom"
        mt32_pcm_rom = "toml-pcm.rom"

        [audio.midi.external_port]
        name = "TOML port"
        ordinal = 2
    "#;
    let saved = r#"
        [midi]
        backend = "off"
        soundfont = "saved.sf3"
        mt32_control_rom = "saved-control.rom"
        mt32_pcm_rom = "saved-pcm.rom"

        [midi.external_port]
        name = "Saved port"
        ordinal = 1
    "#;

    let cli = resolve_midi_fixture(
        &root,
        &locations,
        "cli",
        &[
            "--midi-backend",
            "external",
            "--midi-port",
            "CLI port",
            "--midi-port-ordinal",
            "4",
            "--soundfont",
            "cli.sf3",
            "--mt32-control-rom",
            "cli-control.rom",
            "--mt32-pcm-rom",
            "cli-pcm.rom",
        ],
        toml,
        saved,
    );
    assert_eq!(cli.config.audio.midi.backend, MidiBackend::External);
    assert_eq!(
        cli.config.audio.midi.external_port,
        Some(MidiPortId {
            name: "CLI port".into(),
            ordinal: 4,
        })
    );
    assert_eq!(
        cli.config.audio.midi.soundfont.as_deref(),
        Some(Path::new("cli.sf3"))
    );
    assert_eq!(
        cli.config.audio.midi.mt32_control_rom.as_deref(),
        Some(Path::new("cli-control.rom"))
    );
    assert_eq!(
        cli.config.audio.midi.mt32_pcm_rom.as_deref(),
        Some(Path::new("cli-pcm.rom"))
    );

    let toml_resolved = resolve_midi_fixture(&root, &locations, "toml", &[], toml, saved);
    assert_eq!(toml_resolved.config.audio.midi.backend, MidiBackend::Munt);
    assert_eq!(
        toml_resolved.config.audio.midi.external_port,
        Some(MidiPortId {
            name: "TOML port".into(),
            ordinal: 2,
        })
    );
    assert_eq!(
        toml_resolved.config.audio.midi.soundfont.as_deref(),
        Some(Path::new("toml.sf3"))
    );
    assert_eq!(
        toml_resolved.config.audio.midi.mt32_control_rom.as_deref(),
        Some(Path::new("toml-control.rom"))
    );
    assert_eq!(
        toml_resolved.config.audio.midi.mt32_pcm_rom.as_deref(),
        Some(Path::new("toml-pcm.rom"))
    );

    let saved_resolved = resolve_midi_fixture(
        &root,
        &locations,
        "saved",
        &[],
        "[machine]\nmemory_mib = 24\n",
        saved,
    );
    assert_eq!(saved_resolved.config.audio.midi.backend, MidiBackend::Off);
    assert_eq!(
        saved_resolved.config.audio.midi.external_port,
        Some(MidiPortId {
            name: "Saved port".into(),
            ordinal: 1,
        })
    );
    assert_eq!(
        saved_resolved.config.audio.midi.soundfont.as_deref(),
        Some(Path::new("saved.sf3"))
    );
    assert_eq!(
        saved_resolved.config.audio.midi.mt32_control_rom.as_deref(),
        Some(Path::new("saved-control.rom"))
    );
    assert_eq!(
        saved_resolved.config.audio.midi.mt32_pcm_rom.as_deref(),
        Some(Path::new("saved-pcm.rom"))
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn each_toml_midi_key_blocks_only_its_saved_value() {
    let root = startup_test_dir("midi-presence-bits");
    let state_dir = root.join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let discovered_control = state_dir.join("MT32_CONTROL.ROM");
    let discovered_pcm = state_dir.join("MT32_PCM.ROM");
    std::fs::write(&discovered_control, b"control").unwrap();
    std::fs::write(&discovered_pcm, b"pcm").unwrap();
    let locations = StartupLocations {
        state_dir,
        executable_dir: root.join("portable"),
    };
    let saved_text = r#"
        [midi]
        backend = "munt"
        soundfont = "saved.sf3"
        mt32_control_rom = "saved-control.rom"
        mt32_pcm_rom = "saved-pcm.rom"

        [midi.external_port]
        name = "Saved port"
        ordinal = 1
    "#;
    let saved = MidiConfig {
        backend: MidiBackend::Munt,
        external_port: Some(MidiPortId {
            name: "Saved port".into(),
            ordinal: 1,
        }),
        soundfont: Some(PathBuf::from("saved.sf3")),
        mt32_control_rom: Some(PathBuf::from("saved-control.rom")),
        mt32_pcm_rom: Some(PathBuf::from("saved-pcm.rom")),
    };
    let mut backend = saved.clone();
    backend.backend = MidiBackend::Off;
    let mut external_port = saved.clone();
    external_port.external_port = Some(MidiPortId {
        name: "TOML port".into(),
        ordinal: 7,
    });
    let mut soundfont = saved.clone();
    soundfont.soundfont = Some(PathBuf::from("toml.sf3"));
    let mut control_rom = saved.clone();
    control_rom.mt32_control_rom = Some(PathBuf::from("toml-control.rom"));
    let mut pcm_rom = saved.clone();
    pcm_rom.mt32_pcm_rom = Some(PathBuf::from("toml-pcm.rom"));
    let cases = [
        ("backend", "[audio.midi]\nbackend = \"off\"\n", backend),
        (
            "external-port",
            "[audio.midi.external_port]\nname = \"TOML port\"\nordinal = 7\n",
            external_port,
        ),
        (
            "soundfont",
            "[audio.midi]\nsoundfont = \"toml.sf3\"\n",
            soundfont,
        ),
        (
            "control-rom",
            "[audio.midi]\nmt32_control_rom = \"toml-control.rom\"\n",
            control_rom,
        ),
        (
            "pcm-rom",
            "[audio.midi]\nmt32_pcm_rom = \"toml-pcm.rom\"\n",
            pcm_rom,
        ),
    ];

    for (label, config_text, expected) in cases {
        let resolved = resolve_midi_fixture(&root, &locations, label, &[], config_text, saved_text);
        assert_eq!(resolved.config.audio.midi, expected, "{label}");
    }

    let discovery = resolve_midi_fixture(
        &root,
        &locations,
        "discovery",
        &[],
        "[machine]\nmemory_mib = 24\n",
        "",
    );
    assert_eq!(discovery.config.audio.midi.backend, MidiBackend::Off);
    assert_eq!(discovery.config.audio.midi.external_port, None);
    assert_eq!(discovery.config.audio.midi.soundfont, None);
    assert_eq!(
        discovery.config.audio.midi.mt32_control_rom,
        Some(discovered_control)
    );
    assert_eq!(
        discovery.config.audio.midi.mt32_pcm_rom,
        Some(discovered_pcm)
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn startup_reads_documents_once_and_hands_the_captured_prefs_to_gui() {
    let root = startup_test_dir("single-read");
    let config_path = root.join("settings.toml");
    let c_drive = root.join("c_drive");
    let prefs_path = prefs::prefs_path(&c_drive);
    std::fs::write(&config_path, "[machine]\nmemory_mib = 24\n").unwrap();
    std::fs::write(
        &prefs_path,
        "master_volume = 0.25\n[midi]\nbackend = \"munt\"\n",
    )
    .unwrap();
    let locations = StartupLocations {
        state_dir: root.join("state"),
        executable_dir: root.join("portable"),
    };
    let cli = Cli::try_parse_from([
        "izarravm",
        "--config",
        config_path.to_str().unwrap(),
        "--c-drive",
        c_drive.to_str().unwrap(),
        "--midi-backend",
        "external",
    ])
    .unwrap();
    let reads = Rc::new(RefCell::new(Vec::new()));
    let observed_reads = reads.clone();

    let resolved = resolve_with(&cli, &locations, move |path| {
        observed_reads.borrow_mut().push(path.to_owned());
        std::fs::read_to_string(path)
    })
    .unwrap();
    assert_eq!(
        reads.borrow().as_slice(),
        [config_path.clone(), prefs_path.clone()]
    );

    std::fs::write(
        &prefs_path,
        "master_volume = 0.90\n[midi]\nbackend = \"off\"\n",
    )
    .unwrap();
    let launch = resolved.into_gui(vec![0x5a], false);

    assert_eq!(
        reads.borrow().as_slice(),
        [config_path.clone(), prefs_path.clone()]
    );
    assert_eq!(launch.prefs.master_volume, 0.25);
    assert_eq!(launch.prefs.midi.backend, MidiBackend::Munt);
    assert_eq!(launch.midi_config.backend, MidiBackend::External);
    assert_eq!(launch.prefs_path, prefs_path);

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn global_glide_discovery_is_case_insensitive() {
    let dir = startup_test_dir("glide-ovl");
    assert_eq!(load_state_glide_ovl(&dir), None);
    std::fs::write(dir.join("gLiDe2x.oVl"), b"state fallback").unwrap();
    assert_eq!(load_state_glide_ovl(&dir), Some(b"state fallback".to_vec()));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn munt_discovery_is_case_insensitive_and_prefers_mt32() {
    let dir = startup_test_dir("prefer-mt32");
    let mt_control = dir.join("mt32_control.rom");
    let mt_pcm = dir.join("Mt32_Pcm.Rom");
    for path in [
        &mt_control,
        &mt_pcm,
        &dir.join("CM32L_CONTROL.ROM"),
        &dir.join("CM32L_PCM.ROM"),
    ] {
        std::fs::write(path, b"rom").unwrap();
    }
    let mut midi = izarravm_core::MidiConfig::default();

    discover_munt_roms(&mut midi, &dir);

    assert_eq!(midi.backend, MidiBackend::Off);
    assert_eq!(midi.mt32_control_rom, Some(mt_control));
    assert_eq!(midi.mt32_pcm_rom, Some(mt_pcm));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn munt_discovery_uses_only_complete_pairs() {
    let dir = startup_test_dir("complete-pairs");
    std::fs::write(dir.join("MT32_CONTROL.ROM"), b"rom").unwrap();
    let cm_control = dir.join("cm32l_control.rom");
    let cm_pcm = dir.join("cm32l_pcm.rom");
    std::fs::write(&cm_control, b"rom").unwrap();
    std::fs::write(&cm_pcm, b"rom").unwrap();
    let mut midi = izarravm_core::MidiConfig::default();

    discover_munt_roms(&mut midi, &dir);

    assert_eq!(midi.mt32_control_rom, Some(cm_control));
    assert_eq!(midi.mt32_pcm_rom, Some(cm_pcm.clone()));
    std::fs::remove_file(cm_pcm).unwrap();
    let mut incomplete = izarravm_core::MidiConfig::default();
    discover_munt_roms(&mut incomplete, &dir);
    assert_eq!(incomplete.mt32_control_rom, None);
    assert_eq!(incomplete.mt32_pcm_rom, None);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn malformed_config_stops_before_fallback_or_preferences() {
    let root = startup_test_dir("invalid-config");
    let config_path = root.join("broken.toml");
    let locations = StartupLocations {
        state_dir: root.join("state"),
        executable_dir: root.join("portable"),
    };
    let cli = Cli::try_parse_from(["izarravm", "--config", config_path.to_str().unwrap()]).unwrap();
    let reads = Rc::new(RefCell::new(Vec::new()));
    let observed_reads = reads.clone();

    let error = resolve_with(&cli, &locations, move |path| {
        observed_reads.borrow_mut().push(path.to_owned());
        Ok("[machine\nmemory_mib = 24".to_owned())
    })
    .unwrap_err();

    assert!(matches!(
        error,
        StartupError(ConfigError::Parse { ref path, .. }) if path == &config_path
    ));
    assert_eq!(reads.borrow().as_slice(), [config_path]);
    assert!(!locations.state_dir.join("c_drive").exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn hardware_validation_follows_fallback_and_preferences() {
    let root = startup_test_dir("invalid-hardware");
    let locations = StartupLocations {
        state_dir: root.join("state"),
        executable_dir: root.join("portable"),
    };
    let fallback = locations.state_dir.join("c_drive");
    let expected_prefs = locations.state_dir.join("izarravm.conf");
    let cli = Cli::try_parse_from(["izarravm", "--memory-mib", "1"]).unwrap();
    let reads = Rc::new(RefCell::new(Vec::new()));
    let observed_reads = reads.clone();
    let observed_fallback = fallback.clone();

    let error = resolve_with(&cli, &locations, move |path| {
        assert!(observed_fallback.is_dir());
        observed_reads.borrow_mut().push(path.to_owned());
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "fixture has no preferences",
        ))
    })
    .unwrap_err();

    assert!(matches!(error, StartupError(ConfigError::InvalidMemory(1))));
    assert_eq!(reads.borrow().as_slice(), [expected_prefs]);
    assert!(fallback.is_dir());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn startup_preserves_typed_wss_validation_errors() {
    let root = startup_test_dir("invalid-wss");
    let config_path = root.join("invalid-wss.toml");
    let c_drive = root.join("c_drive");
    let locations = StartupLocations {
        state_dir: root.join("state"),
        executable_dir: root.join("portable"),
    };
    let cli = Cli::try_parse_from([
        "izarravm",
        "--config",
        config_path.to_str().unwrap(),
        "--c-drive",
        c_drive.to_str().unwrap(),
    ])
    .unwrap();

    let error = resolve_with(&cli, &locations, |path| {
        if path == config_path {
            Ok("[audio.wss]\nbase = 1016\n".into())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "fixture has no preferences",
            ))
        }
    })
    .unwrap_err();

    assert!(matches!(
        error,
        StartupError(ConfigError::InvalidWssBase(0x03f8, 0x0400))
    ));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn malformed_preferences_remain_nonfatal() {
    let root = startup_test_dir("invalid-prefs");
    let c_drive = root.join("c_drive");
    let locations = StartupLocations {
        state_dir: root.join("state"),
        executable_dir: root.join("portable"),
    };
    let cli = Cli::try_parse_from(["izarravm", "--c-drive", c_drive.to_str().unwrap()]).unwrap();

    let resolved = resolve_with(&cli, &locations, |_| Ok("[broken".into())).unwrap();

    assert_eq!(resolved.prefs, prefs::GuiPrefs::default());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn portable_and_partial_munt_resolution_is_stable() {
    let root = startup_test_dir("munt-partials");
    let state_dir = root.join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::write(state_dir.join("MT32_CONTROL.ROM"), b"control").unwrap();
    std::fs::write(state_dir.join("MT32_PCM.ROM"), b"pcm").unwrap();
    let locations = StartupLocations {
        state_dir: state_dir.clone(),
        executable_dir: root.join("portable"),
    };
    let missing = || {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "fixture has no preferences",
        ))
    };

    let explicit_cli = Cli::try_parse_from([
        "izarravm",
        "--c-drive",
        root.join("explicit/c_drive").to_str().unwrap(),
        "--mt32-control-rom",
        "missing-explicit-control.rom",
    ])
    .unwrap();
    let explicit = resolve_with(&explicit_cli, &locations, |_| missing()).unwrap();
    assert_eq!(
        explicit.config.audio.midi.mt32_control_rom.as_deref(),
        Some(Path::new("missing-explicit-control.rom"))
    );
    assert_eq!(explicit.config.audio.midi.mt32_pcm_rom, None);

    let saved_c_drive = root.join("saved/c_drive");
    let saved_prefs_path = prefs::prefs_path(&saved_c_drive);
    let saved_cli =
        Cli::try_parse_from(["izarravm", "--c-drive", saved_c_drive.to_str().unwrap()]).unwrap();
    let saved = resolve_with(&saved_cli, &locations, |path| {
        if path == saved_prefs_path {
            Ok("[midi]\nmt32_control_rom = \"missing-saved-control.rom\"\n".into())
        } else {
            unreachable!("unexpected startup read: {}", path.display())
        }
    })
    .unwrap();
    assert_eq!(
        saved.config.audio.midi.mt32_control_rom.as_deref(),
        Some(Path::new("missing-saved-control.rom"))
    );
    assert_eq!(saved.config.audio.midi.mt32_pcm_rom, None);

    let portable_cli = Cli::try_parse_from([
        "izarravm",
        "--portable",
        "--c-drive",
        root.join("portable/c_drive").to_str().unwrap(),
    ])
    .unwrap();
    let portable = resolve_with(&portable_cli, &locations, |_| missing()).unwrap();
    assert_eq!(portable.config.audio.midi.backend, MidiBackend::Off);
    assert_eq!(portable.config.audio.midi.mt32_control_rom, None);
    assert_eq!(portable.config.audio.midi.mt32_pcm_rom, None);

    std::fs::remove_file(state_dir.join("MT32_PCM.ROM")).unwrap();
    let incomplete_cli = Cli::try_parse_from([
        "izarravm",
        "--c-drive",
        root.join("incomplete/c_drive").to_str().unwrap(),
    ])
    .unwrap();
    let incomplete = resolve_with(&incomplete_cli, &locations, |_| missing()).unwrap();
    assert_eq!(incomplete.config.audio.midi.mt32_control_rom, None);
    assert_eq!(incomplete.config.audio.midi.mt32_pcm_rom, None);

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn explicit_paths_stay_relative_and_portable_glide_stays_global() {
    let root = startup_test_dir("relative-paths");
    let state_dir = root.join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::write(state_dir.join("MT32_CONTROL.ROM"), b"control").unwrap();
    std::fs::write(state_dir.join("MT32_PCM.ROM"), b"pcm").unwrap();
    std::fs::write(state_dir.join("GLIDE2X.OVL"), b"global glide").unwrap();
    let locations = StartupLocations {
        state_dir,
        executable_dir: root.join("portable"),
    };
    let config_path = root.join("relative.toml");
    let cli = Cli::try_parse_from([
        "izarravm",
        "--portable",
        "--config",
        config_path.to_str().unwrap(),
    ])
    .unwrap();
    let config_text = r#"
        [dos]
        c_drive = "./relative-c"
        cd_image = "../media/disc.iso"

        [audio.midi]
        soundfont = "./banks/game.sf3"
    "#;
    let resolved = resolve_with(&cli, &locations, |path| {
        if path == config_path {
            Ok(config_text.into())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "fixture has no preferences",
            ))
        }
    })
    .unwrap();
    let launch = resolved.into_gui(vec![0x42], false);

    assert_eq!(launch.c_drive.to_string_lossy(), "./relative-c");
    assert_eq!(
        launch.cd_image.as_deref(),
        Some(Path::new("../media/disc.iso"))
    );
    assert_eq!(
        launch.midi_config.soundfont.as_deref(),
        Some(Path::new("./banks/game.sf3"))
    );
    assert_eq!(launch.midi_config.mt32_control_rom, None);
    assert_eq!(launch.midi_config.mt32_pcm_rom, None);
    assert_eq!(launch.glide_ovl, Some(b"global glide".to_vec()));

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn gui_launch_carries_the_resolved_host_input_policy() {
    let root = startup_test_dir("host-input-policy");
    let config_path = root.join("input.toml");
    let c_drive = root.join("c_drive");
    let locations = StartupLocations {
        state_dir: root.join("state"),
        executable_dir: root.join("portable"),
    };
    let cli = Cli::try_parse_from([
        "izarravm",
        "--config",
        config_path.to_str().unwrap(),
        "--c-drive",
        c_drive.to_str().unwrap(),
    ])
    .unwrap();
    let resolved = resolve_with(&cli, &locations, |path| {
        if path == config_path {
            Ok("[input]\nkeyboard = false\nmouse = true\njoystick = false\n".into())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "fixture has no preferences",
            ))
        }
    })
    .unwrap();

    let policy = resolved.into_gui(vec![0x42], false).host_input;
    assert!(!policy.keyboard_enabled());
    assert!(policy.mouse_enabled());
    assert!(!policy.joystick_enabled());

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn headless_startup_resolves_toml_then_cli_hardware() {
    let root = startup_test_dir("headless-resolution");
    let config_path = root.join("headless.toml");
    let c_drive = root.join("c_drive");
    let locations = StartupLocations {
        state_dir: root.join("state"),
        executable_dir: root.join("portable"),
    };
    let cli = Cli::try_parse_from([
        "izarravm",
        "--headless-test-rom",
        "--config",
        config_path.to_str().unwrap(),
        "--cpu",
        "386",
        "--memory-mib",
        "16",
        "--c-drive",
        c_drive.to_str().unwrap(),
    ])
    .unwrap();
    let resolved = resolve_with(&cli, &locations, |path| {
        if path == config_path {
            Ok("[machine]\ncpu = \"486\"\nmemory_mib = 32\nvideo = \"vega\"\n".into())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "fixture has no preferences",
            ))
        }
    })
    .unwrap();

    assert_eq!(resolved.hardware().cpu, GswMode::Gsw386);
    assert_eq!(resolved.hardware().memory_mib, 16);
    assert_eq!(resolved.hardware().video, VideoCard::Vega);

    std::fs::remove_dir_all(root).unwrap();
}

/// The hardware flags have to survive the trip from the command line into the
/// RTC setup, or the "your saved CMOS overrode this" warning has nothing to
/// warn about. The values themselves reach the machine through the config; this
/// second copy exists only to record that the user typed them, which is what
/// separates "overridden" from "left at the default".
#[test]
fn typed_hardware_flags_reach_the_rtc_setup() {
    let root = startup_test_dir("requested-flags");
    let cli = Cli::try_parse_from([
        "izarravm",
        "--cpu",
        "386-slow",
        "--sb-irq",
        "5",
        "--sb-high-dma",
        "7",
    ])
    .unwrap();
    let locations = StartupLocations {
        state_dir: root.join("state"),
        executable_dir: root.join("portable"),
    };

    let resolved = resolve_with(&cli, &locations, |_| {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "fixture has no preferences",
        ))
    })
    .unwrap();
    let launch = resolved.into_gui(Vec::new(), false);

    assert_eq!(
        launch.rtc_setup.requested,
        crate::cmos::RequestedHardware {
            cpu: Some(izarravm_core::GswMode::Gsw386Slow),
            sb_irq: Some(izarravm_core::SbIrq::I5),
            sb_dma: None,
            sb_high_dma: Some(izarravm_core::SbDma16::D7),
        },
        "a flag left off the command line must stay None, or an untouched \
         setting would be reported as overridden"
    );
}

/// Pointing --config at the GUI preferences file is refused by name. The two
/// have both been called izarravm.conf, so this is an easy mistake to make and
/// a confusing one to diagnose from a bare unknown-field error.
#[test]
fn the_guis_preferences_file_is_refused_as_a_machine_config() {
    let root = startup_test_dir("gui-prefs-as-config");
    let config_path = root.join("izarravm.conf");
    let config_arg = config_path.to_string_lossy().into_owned();
    let cli = Cli::try_parse_from(["izarravm", "--config", &config_arg]).unwrap();
    let locations = StartupLocations {
        state_dir: root.join("state"),
        executable_dir: root.join("portable"),
    };

    let error = resolve_with(&cli, &locations, |path| {
        if path == config_path {
            Ok("master_volume = 0.8\ncrt_style = \"subtle\"\n".to_string())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "fixture has no preferences",
            ))
        }
    })
    .unwrap_err();

    let message = error.to_string();
    assert!(
        message.contains("master_volume") && message.contains("machine config"),
        "the error must name the file and the key that gave it away: {message}"
    );
}

/// A ROM set dropped in as a FOLDER is discovered too.
///
/// The loose-pair rule only recognises a set whose two files carry the
/// canonical names, and most distributions do not: versioned names, Munt's own
/// `ctrl_mt32_1_07.rom` shape, and half-image pairs that cannot be named as two
/// files at all. Handing the folder to the loader lets it identify the images by
/// content instead, which is the whole point of it accepting a folder.
#[test]
fn a_rom_set_folder_in_the_state_directory_is_discovered() {
    let root = startup_test_dir("munt-folder-discovery");
    let state_dir = root.join("state");
    let roms = state_dir.join("MT32");
    std::fs::create_dir_all(&roms).unwrap();
    // Names the loose-pair rule would not match, in a folder name it does.
    std::fs::write(roms.join("ctrl_mt32_1_07.rom"), b"control").unwrap();
    std::fs::write(roms.join("pcm_mt32.rom"), b"pcm").unwrap();

    let mut config = MidiConfig::default();
    discover_munt_roms(&mut config, &state_dir);
    assert_eq!(config.mt32_control_rom.as_deref(), Some(roms.as_path()));
    assert_eq!(config.mt32_pcm_rom.as_deref(), Some(roms.as_path()));

    // A loose canonical pair still wins: it is the more specific answer, and
    // pointing at two files is what the user gets to see in the panel.
    let control = state_dir.join("MT32_CONTROL.ROM");
    let pcm = state_dir.join("MT32_PCM.ROM");
    std::fs::write(&control, b"control").unwrap();
    std::fs::write(&pcm, b"pcm").unwrap();
    let mut config = MidiConfig::default();
    discover_munt_roms(&mut config, &state_dir);
    assert_eq!(config.mt32_control_rom.as_deref(), Some(control.as_path()));
    assert_eq!(config.mt32_pcm_rom.as_deref(), Some(pcm.as_path()));

    // A folder that is not a ROM-set folder is left alone.
    let bare = root.join("bare");
    std::fs::create_dir_all(bare.join("screenshots")).unwrap();
    let mut config = MidiConfig::default();
    discover_munt_roms(&mut config, &bare);
    assert_eq!(config.mt32_control_rom, None);
    assert_eq!(config.mt32_pcm_rom, None);

    let _ = std::fs::remove_dir_all(&root);
}
