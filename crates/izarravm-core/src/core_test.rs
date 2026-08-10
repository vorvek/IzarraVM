// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn izarra_3000_defaults_to_its_586_persona() {
    assert_eq!(MachineConfig::default().cpu, GswMode::Gsw586);
}

#[test]
fn device_lines_are_returned_in_order_with_high_flag() {
    let lines = parse_device_lines(
        "DEVICE=C:\\DOS\\HIMEM.SYS /TESTMEM:OFF\r\n\
         DEVICEHIGH=C:\\MOUSE.SYS 2\r\n\
         DEVICE=\"C:\\my dir\\ANSI.SYS\"\r\n\
         FILES=40\r\n",
    );
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].path, "C:\\DOS\\HIMEM.SYS");
    assert_eq!(lines[0].args, "/TESTMEM:OFF");
    assert!(!lines[0].high);
    assert_eq!(lines[1].path, "C:\\MOUSE.SYS");
    assert_eq!(lines[1].args, "2");
    assert!(lines[1].high);
    assert_eq!(lines[2].path, "C:\\my dir\\ANSI.SYS"); // quoted path preserved
    assert_eq!(dos_basename(&lines[1].path), "MOUSE.SYS");
}

#[test]
fn emm386_conf_key_parses_and_is_ignored() {
    // Older izarravm.conf files carried `emm386 = "..."`; the key is
    // accepted and ignored so those files still parse while `MachineConfig`
    // uses `deny_unknown_fields`.
    let cfg: AppConfig = toml::from_str("[machine]\nemm386 = \"noems\"\n").unwrap();
    assert_eq!(cfg.machine.emm386, Some("noems".to_string()));

    // Everything else in the parsed config is untouched.
    assert_eq!(cfg.machine.cpu, MachineConfig::default().cpu);
    assert_eq!(cfg.machine.memory_mib, MachineConfig::default().memory_mib);
    assert_eq!(cfg.machine.video, MachineConfig::default().video);

    // The retired key is never written back out.
    let serialized = toml::to_string(&AppConfig::default()).unwrap();
    assert!(!serialized.contains("emm386"));
}

#[test]
fn retired_host_facade_keys_preserve_hardware_and_public_input() {
    let control: AppConfig = toml::from_str(
        r#"
        [input]
        keyboard = false
        mouse = true
        joystick = false
        "#,
    )
    .unwrap();
    let retired: AppConfig = toml::from_str(
        r#"
        [audio]
        pc_speaker = false
        opl3 = false

        [input]
        keyboard = false
        mouse = true
        joystick = false
        steam_input = "optional_backend"
        "#,
    )
    .unwrap();

    assert_eq!(
        HardwareProfile::from_config(&retired).unwrap(),
        HardwareProfile::from_config(&control).unwrap()
    );
    assert_eq!(retired.input.keyboard, control.input.keyboard);
    assert_eq!(retired.input.mouse, control.input.mouse);
    assert_eq!(retired.input.joystick, control.input.joystick);
}

#[test]
fn retired_host_facade_keys_accept_their_previous_values() {
    for text in [
        "[audio]\npc_speaker = true\nopl3 = true\n[input]\nsteam_input = \"off\"\n",
        "[audio]\npc_speaker = false\nopl3 = false\n[input]\nsteam_input = \"optional_backend\"\n",
    ] {
        toml::from_str::<AppConfig>(text).unwrap();
    }
}

#[test]
fn retired_host_facade_keys_remain_strict() {
    for text in [
        "[audio]\npc_speaker = \"yes\"\n",
        "[audio]\nopl3 = 1\n",
        "[input]\nsteam_input = \"required\"\n",
    ] {
        assert!(toml::from_str::<AppConfig>(text).is_err(), "{text}");
    }
}

#[test]
fn retired_host_facade_keys_are_not_serialized() {
    let config: AppConfig = toml::from_str(
        r#"
        [audio]
        pc_speaker = false
        opl3 = false

        [input]
        steam_input = "optional_backend"
        "#,
    )
    .unwrap();
    let serialized = toml::to_string(&config).unwrap();

    assert!(!serialized.contains("pc_speaker"));
    assert!(!serialized.contains("opl3"));
    assert!(!serialized.contains("steam_input"));
}

#[test]
fn rejects_memory_outside_supported_range() {
    let mut config = AppConfig::default();
    config.machine.memory_mib = 1;
    assert!(matches!(
        config.validate(),
        Err(ConfigError::InvalidMemory(1))
    ));

    config.machine.memory_mib = 65;
    assert!(matches!(
        config.validate(),
        Err(ConfigError::InvalidMemory(65))
    ));
}

#[test]
fn applies_cli_style_overrides() {
    let mut config = AppConfig::default();
    config.apply_overrides(ConfigOverrides {
        cpu: Some(GswMode::Gsw386),
        memory_mib: Some(32),
        video: Some(VideoCard::Vega),
        c_drive: Some(PathBuf::from("games")),
        soundfont: Some(PathBuf::from("gm.sf2")),
        midi_backend: Some(MidiBackend::Munt),
        external_midi_port: Some(MidiPortId {
            name: "USB MIDI".to_string(),
            ordinal: 1,
        }),
        mt32_control_rom: Some(PathBuf::from("MT32_CONTROL.ROM")),
        mt32_pcm_rom: Some(PathBuf::from("MT32_PCM.ROM")),
        sb_irq: Some(SbIrq::I7),
        sb_dma: Some(SbDma8::D3),
        sb_high_dma: Some(SbDma16::D6),
    });

    assert_eq!(config.machine.cpu, GswMode::Gsw386);
    assert_eq!(config.machine.memory_mib, 32);
    assert_eq!(config.machine.video, VideoCard::Vega);
    assert_eq!(config.dos.c_drive, PathBuf::from("games"));
    assert_eq!(config.audio.midi.soundfont, Some(PathBuf::from("gm.sf2")));
    assert_eq!(config.audio.midi.backend, MidiBackend::Munt);
    assert_eq!(
        config.audio.midi.external_port,
        Some(MidiPortId {
            name: "USB MIDI".to_string(),
            ordinal: 1,
        })
    );
    assert_eq!(
        config.audio.midi.mt32_control_rom,
        Some(PathBuf::from("MT32_CONTROL.ROM"))
    );
    assert_eq!(
        config.audio.midi.mt32_pcm_rom,
        Some(PathBuf::from("MT32_PCM.ROM"))
    );
    assert_eq!(config.audio.sound_blaster.irq, SbIrq::I7);
    assert_eq!(config.audio.sound_blaster.dma, SbDma8::D3);
    assert_eq!(config.audio.sound_blaster.high_dma, SbDma16::D6);
}

#[test]
fn midi_config_defaults_p330_off_and_round_trips_receiver_identity() {
    let default = MidiConfig::default();
    assert_eq!(default.backend, MidiBackend::Off);
    assert_eq!(default.external_port, None);
    assert_eq!(default.soundfont, None);
    assert_eq!(default.mt32_control_rom, None);
    assert_eq!(default.mt32_pcm_rom, None);

    let parsed: AppConfig = toml::from_str(
        r#"
        [audio.midi]
        backend = "munt"
        soundfont = "custom.sf3"
        mt32_control_rom = "control.rom"
        mt32_pcm_rom = "pcm.rom"

        [audio.midi.external_port]
        name = "USB MIDI"
        ordinal = 2
        "#,
    )
    .unwrap();
    assert_eq!(parsed.audio.midi.backend, MidiBackend::Munt);
    assert_eq!(
        parsed.audio.midi.external_port,
        Some(MidiPortId {
            name: "USB MIDI".to_string(),
            ordinal: 2,
        })
    );
    let encoded = toml::to_string(&parsed).unwrap();
    let round_trip: AppConfig = toml::from_str(&encoded).unwrap();
    assert_eq!(round_trip.audio.midi, parsed.audio.midi);

    let legacy: AppConfig = toml::from_str("[audio.midi]\nbackend = \"fluid_synth\"\n").unwrap();
    assert_eq!(legacy.audio.midi.backend, MidiBackend::Off);
    assert!(
        toml::to_string(&legacy)
            .unwrap()
            .contains("backend = \"off\"")
    );
}

#[test]
fn sound_blaster_overrides_and_aliases_parse() {
    assert_eq!("7".parse::<SbIrq>().unwrap(), SbIrq::I7);
    assert_eq!("irq10".parse::<SbIrq>().unwrap(), SbIrq::I10);
    assert_eq!("3".parse::<SbDma8>().unwrap(), SbDma8::D3);
    assert_eq!("dma6".parse::<SbDma16>().unwrap(), SbDma16::D6);
    assert_eq!(SbIrq::I10.line(), 10);
    assert_eq!(SbDma8::D3.channel(), 3);
    assert_eq!(SbDma16::D7.channel(), 7);
}

#[test]
fn midi_receivers_and_fixed_guest_ports_are_unambiguous() {
    assert_eq!("off".parse::<MidiBackend>().unwrap(), MidiBackend::Off);
    assert_eq!(
        "external".parse::<MidiBackend>().unwrap(),
        MidiBackend::External
    );
    assert_eq!(
        "fluidsynth".parse::<MidiBackend>().unwrap(),
        MidiBackend::Off
    );
    assert_eq!("mt-32".parse::<MidiBackend>().unwrap(), MidiBackend::Munt);
    assert_eq!(WAVETABLE_MPU_BASE, 0x300);
    assert_eq!(MIDI_MPU_BASE, 0x330);
}

#[test]
fn sound_blaster_config_defaults_when_absent_or_partial() {
    // No [audio.sound_blaster] table: the shipped default (IRQ7/DMA1/DMA5).
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("izarravm.toml");
    fs::write(
        &path,
        r#"
            [machine]
            cpu = "386"
            memory_mib = 16
            video = "et4000_ax"
        "#,
    )
    .unwrap();
    let config = AppConfig::from_toml_path(path).unwrap();
    assert_eq!(
        config.audio.sound_blaster,
        SoundBlasterConfig {
            enabled: true,
            irq: SbIrq::I7,
            dma: SbDma8::D1,
            high_dma: SbDma16::D5
        }
    );

    // `enabled` is still the file to set -- whether the card is fitted is a
    // property of the machine, not something the guest can change. The routing
    // beside it is not: CMOS owns that now, so the key parses and is dropped.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("izarravm.toml");
    fs::write(
        &path,
        r#"
            [audio.sound_blaster]
            enabled = true
            irq = "5"
        "#,
    )
    .unwrap();
    let config = AppConfig::from_toml_path(path).unwrap();
    assert!(config.audio.sound_blaster.enabled);
    assert_eq!(
        config.audio.sound_blaster.irq,
        SbIrq::I7,
        "audio.sound_blaster.irq is retired: the file cannot move the card"
    );
    assert_eq!(config.audio.sound_blaster.dma, SbDma8::D1);
    assert_eq!(config.audio.sound_blaster.high_dma, SbDma16::D5);
}

#[test]
fn wss_config_defaults_when_absent_or_partial() {
    // No [audio.wss] table: the codec is always present (enabled), at the
    // WSS standard base 0x530 with IRQ11/DMA0 -- IRQ11 rather than the WSS
    // standard IRQ7, which is yielded to the Sound Blaster, and DMA0 to dodge
    // the SB16 DMA1.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("izarravm.toml");
    fs::write(
        &path,
        r#"
            [machine]
            cpu = "386"
            memory_mib = 16
            video = "et4000_ax"
        "#,
    )
    .unwrap();
    let config = AppConfig::from_toml_path(path).unwrap();
    assert_eq!(
        config.audio.wss,
        WssConfig {
            enabled: true,
            base: 0x530,
            irq: WssIrq::I11,
            dma: SbDma8::D0,
        }
    );

    // As with the Sound Blaster, `enabled` and `base` remain in the file; the
    // codec IRQ is retired to CMOS and the key is dropped.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("izarravm.toml");
    fs::write(
        &path,
        r#"
            [audio.wss]
            enabled = true
            irq = "10"
        "#,
    )
    .unwrap();
    let config = AppConfig::from_toml_path(path).unwrap();
    assert!(config.audio.wss.enabled);
    assert_eq!(config.audio.wss.base, 0x530);
    assert_eq!(
        config.audio.wss.irq,
        WssIrq::I11,
        "audio.wss.irq is retired: the file cannot move the codec"
    );
    assert_eq!(
        config.audio.wss.dma,
        SbDma8::D0,
        "the retired DMA key must land on the CODEC default (0), not \
         SbDma8::default() (1), which is the channel the SB16 holds"
    );
}

#[test]
fn wss_config_parses_overrides_when_present() {
    // The two fields the file still owns: whether the codec is fitted and where
    // its config region decodes. The IRQ and DMA keys beside them are retired,
    // so a file carrying all four still loads -- and moves only the first two.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("izarravm.toml");
    fs::write(
        &path,
        r#"
            [audio.wss]
            enabled = false
            base = 0x604
            irq = "11"
            dma = "3"
        "#,
    )
    .unwrap();
    let config = AppConfig::from_toml_path(path).unwrap();
    assert_eq!(
        config.audio.wss,
        WssConfig {
            enabled: false,
            base: 0x604,
            irq: WssIrq::I11,
            dma: SbDma8::D0,
        }
    );
}

#[test]
fn wss_irq_parses_documented_lines_and_rejects_others() {
    // The documented WSS lines are 7/9/10/11; anything else (e.g. the SB16's
    // IRQ5, which `SbIrq` carried but the codec cannot) must be rejected.
    assert_eq!("7".parse::<WssIrq>().unwrap(), WssIrq::I7);
    assert_eq!("irq9".parse::<WssIrq>().unwrap(), WssIrq::I9);
    assert_eq!("10".parse::<WssIrq>().unwrap(), WssIrq::I10);
    assert_eq!("11".parse::<WssIrq>().unwrap(), WssIrq::I11);
    assert_eq!(WssIrq::I9.line(), 9);
    assert_eq!(WssIrq::I11.line(), 11);
    assert!("5".parse::<WssIrq>().is_err(), "IRQ5 is not a WSS line");
}

#[test]
fn rejects_wss_base_that_shadows_fixed_ports() {
    // The documented WSS bases all pass validation.
    for base in [0x530u16, 0x604, 0xE80, 0xF40] {
        let mut config = AppConfig::default();
        config.audio.wss.base = base;
        assert!(
            config.validate().is_ok(),
            "documented WSS base {base:#06x} must validate"
        );
    }
    // A base whose window shadows the 8237 DMA controller (0x000-0x00F) is
    // rejected so it cannot silently steal those ports at the WSS decode.
    let mut config = AppConfig::default();
    config.audio.wss.base = 0x0004;
    assert!(matches!(
        config.validate(),
        Err(ConfigError::InvalidWssBase(0x0004, 0x000C))
    ));
    // base 0x000 (full overlap with DMA ch1) is likewise rejected.
    config.audio.wss.base = 0x0000;
    assert!(matches!(
        config.validate(),
        Err(ConfigError::InvalidWssBase(0x0000, 0x0008))
    ));
    // A window straddling the SB16 base (0x21C..0x224 overlaps 0x220) is caught.
    config.audio.wss.base = 0x021C;
    assert!(matches!(
        config.validate(),
        Err(ConfigError::InvalidWssBase(0x021C, 0x0224))
    ));
    // A disabled codec is not validated, so even a dangerous base is allowed.
    config.audio.wss.enabled = false;
    config.audio.wss.base = 0x0000;
    assert!(
        config.validate().is_ok(),
        "disabled WSS skips base validation"
    );
}

#[test]
fn rejects_wss_base_over_serial_or_parallel_ports() {
    // read_io decodes the COM/LPT UARTs before the WSS window, so a base over
    // them would be silently shadowed. validate_base must reject those too.
    // COM2 (0x2F8): window 0x2F8..0x300 overlaps the 0x2F8-0x2FF UART.
    let mut config = AppConfig::default();
    config.audio.wss.base = 0x02F8;
    assert!(
        matches!(
            config.validate(),
            Err(ConfigError::InvalidWssBase(0x02F8, _))
        ),
        "a WSS base over COM2 must be rejected"
    );
    // LPT1 (0x378): window 0x378..0x380 overlaps the 0x378-0x37F parallel port.
    config.audio.wss.base = 0x0378;
    assert!(
        matches!(
            config.validate(),
            Err(ConfigError::InvalidWssBase(0x0378, _))
        ),
        "a WSS base over LPT1 must be rejected"
    );
    // COM1 (0x3F8): window overlaps the 0x3F8-0x3FF UART.
    config.audio.wss.base = 0x03F8;
    assert!(
        matches!(
            config.validate(),
            Err(ConfigError::InvalidWssBase(0x03F8, _))
        ),
        "a WSS base over COM1 must be rejected"
    );
    // LPT2 (0x278): window overlaps the 0x278-0x27F parallel port.
    config.audio.wss.base = 0x0278;
    assert!(
        matches!(
            config.validate(),
            Err(ConfigError::InvalidWssBase(0x0278, _))
        ),
        "a WSS base over LPT2 must be rejected"
    );
}

#[test]
fn rejects_wss_sb16_irq_or_dma_collision() {
    // On a real combo card the AD1848 and SB16 are jumpered to distinct IRQ/DMA
    // resources. The defaults are disjoint (WSS IRQ7/DMA0 vs SB16 IRQ5/DMA1), so
    // a default config validates.
    let config = AppConfig::default();
    assert!(config.validate().is_ok(), "disjoint defaults validate");

    // Pointing the WSS at the SB16's DMA channel (DMA1) is rejected.
    let mut config = AppConfig::default();
    config.audio.wss.dma = SbDma8::D1; // == SB16 default DMA1
    assert!(matches!(
        config.validate(),
        Err(ConfigError::WssSbDmaCollision(1))
    ));

    // Pointing the WSS at the SB16's IRQ line (both IRQ7) is rejected.
    let mut config = AppConfig::default();
    config.audio.wss.irq = WssIrq::I7;
    config.audio.sound_blaster.irq = SbIrq::I7;
    assert!(matches!(
        config.validate(),
        Err(ConfigError::WssSbIrqCollision(7))
    ));

    // With the SB16 disabled there is no contention, so a "colliding" config is
    // allowed (the SB16 is not present to fight over the resource).
    let mut config = AppConfig::default();
    config.audio.sound_blaster.enabled = false;
    config.audio.wss.dma = SbDma8::D1;
    config.audio.wss.irq = WssIrq::I7;
    config.audio.sound_blaster.irq = SbIrq::I7;
    assert!(
        config.validate().is_ok(),
        "a disabled SB16 cannot collide with the WSS"
    );
}

#[test]
fn loads_toml_config() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("izarravm.toml");
    fs::write(
        &path,
        r#"
            [machine]
            cpu = "386"
            memory_mib = 16
            video = "et4000_ax"
        "#,
    )
    .unwrap();

    let config = AppConfig::from_toml_path(path).unwrap();
    assert_eq!(
        config.machine.cpu,
        GswMode::Gsw586,
        "machine.cpu is retired to CMOS: the file parses but does not set it"
    );
    assert_eq!(config.machine.video, VideoCard::Vega);
    assert_eq!(config.dos.c_drive, PathBuf::from("."));
}

/// Every key CMOS took over parses, is reported, and leaves the built-in
/// power-on default in place. Parsing matters as much as ignoring: `AppConfig`
/// denies unknown fields, so without the stripping pass an existing config file
/// would stop loading altogether rather than quietly losing these keys.
#[test]
fn retired_cmos_keys_are_accepted_reported_and_ignored() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("izarravm.toml");
    fs::write(
        &path,
        r#"
            [machine]
            cpu = "386-slow"
            memory_mib = 16

            [audio.sound_blaster]
            irq = "2"
            dma = "3"
            high_dma = "7"

            [audio.wss]
            irq = "9"
            dma = "1"
        "#,
    )
    .unwrap();

    let config = AppConfig::from_toml_path(&path).unwrap();
    assert_eq!(config.machine.memory_mib, 16, "live keys still apply");
    assert_eq!(config.machine.cpu, GswMode::Gsw586);
    assert_eq!(config.audio.sound_blaster.irq, SbIrq::I7);
    assert_eq!(config.audio.sound_blaster.dma, SbDma8::D1);
    assert_eq!(config.audio.sound_blaster.high_dma, SbDma16::D5);
    assert_eq!(config.audio.wss.irq, WssIrq::I11);
    assert_eq!(config.audio.wss.dma, SbDma8::D0);

    // The caller is told exactly which keys it ignored, so the warning can name
    // them rather than leaving the user to wonder why nothing happened.
    let mut value = toml::from_str::<toml::Value>(&fs::read_to_string(&path).unwrap()).unwrap();
    let mut dropped = crate::strip_retired_keys(&mut value);
    dropped.sort();
    assert_eq!(
        dropped,
        vec![
            "audio.sound_blaster.dma",
            "audio.sound_blaster.high_dma",
            "audio.sound_blaster.irq",
            "audio.wss.dma",
            "audio.wss.irq",
            "machine.cpu",
        ]
    );
    assert!(
        crate::strip_retired_keys(&mut value).is_empty(),
        "a second pass has nothing left to drop"
    );
}

/// Two different files have both been called izarravm.conf: the one the GUI
/// writes beside the C: drive, and the machine description you pass to
/// --config. Pointing --config at the first is an easy mistake, and the bare
/// unknown-field error it used to produce named one key and explained nothing.
#[test]
fn the_guis_own_preferences_file_is_recognised_and_named() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("izarravm.conf");
    fs::write(
        &path,
        r#"
            master_volume = 0.8
            amp_gain = 120
            crt_style = "subtle"
        "#,
    )
    .unwrap();

    let error = AppConfig::from_toml_path(&path).unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("GUI"),
        "the error should say which file this is: {message}"
    );
    assert!(
        message.contains("master_volume"),
        "and point at the key that gave it away: {message}"
    );
    assert!(
        message.contains("examples/machine.toml"),
        "and say what to pass instead: {message}"
    );

    // The gain knob was renamed `amp_gain` -> `output_gain`. BOTH spellings have
    // to be recognised here: the new one because that is what is written now,
    // and the old one because a file that still carries it is still a prefs file
    // and deserves this message rather than an unknown-field error.
    for (name, key) in [
        ("legacy", "amp_gain = 120"),
        ("current", "output_gain = 25"),
    ] {
        let path = directory.path().join(format!("{name}.conf"));
        fs::write(&path, format!("{key}\n")).unwrap();
        let message = AppConfig::from_toml_path(&path).unwrap_err().to_string();
        assert!(
            message.contains("GUI"),
            "{key} alone must identify a prefs file: {message}"
        );
    }
}

/// The detector keys off fields a machine config cannot have, so a real one --
/// even a complete one -- must never be mistaken for GUI preferences.
#[test]
fn a_real_machine_config_is_not_mistaken_for_gui_preferences() {
    let value = toml::from_str::<toml::Value>(
        r#"
            [machine]
            memory_mib = 16
            video = "vega"

            [audio.sound_blaster]
            enabled = true

            [audio.midi]
            backend = "off"

            [input]
            keyboard = true
        "#,
    )
    .unwrap();
    assert_eq!(crate::gui_prefs_marker(&value), None);
}

/// A config file that never mentioned them is untouched, so the pass cannot
/// invent a warning for a user who has done nothing wrong.
#[test]
fn stripping_reports_nothing_for_a_file_without_the_retired_keys() {
    let mut value = toml::from_str::<toml::Value>(
        r#"
            [machine]
            memory_mib = 16
        "#,
    )
    .unwrap();
    assert!(crate::strip_retired_keys(&mut value).is_empty());
}
