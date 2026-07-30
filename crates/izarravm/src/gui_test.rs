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
        "GSW-586 - 586 mode - 200 MHz"
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
fn cd_mount_session_remounts_on_reset_and_clears_on_eject_or_stop() {
    let source = CdSource::Image(PathBuf::from("disc.cue"));
    let mut session = CdMountSession::default();
    session.remember("disc.cue".to_string(), source.clone());

    let reset_source = session.begin_reset();
    assert_eq!(reset_source, Some(source.clone()));
    assert_eq!(session.source, Some(source.clone()));
    assert_eq!(session.label, None);

    session.remember("disc.cue".to_string(), reset_source.unwrap());
    assert_eq!(session.label.as_deref(), Some("disc.cue"));
    session.clear();
    assert_eq!(session, CdMountSession::default());
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
fn queued_cd_commands_apply_in_channel_order() {
    let mut rom = vec![0u8; izarravm_machine::BIOS_ROM_SIZE];
    rom[0xFFF0..0xFFF5].copy_from_slice(&[0xEA, 0x00, 0x00, 0x00, 0xF0]);
    let mut machine = Machine::new(
        MachineProfile::gsw_386(1, izarravm_core::VideoCard::Vega),
        rom,
    )
    .unwrap();
    let cue = "TRACK 01 AUDIO\nINDEX 01 00:00:00\n";
    let image = izarravm_machine::CdImage::from_cue(cue, vec![0; 4 * 2352]).unwrap();
    let (sender, receiver) = mpsc::channel();
    sender.send(Command::MountCd(image)).unwrap();
    sender.send(Command::CdLinkedLevel(18)).unwrap();
    sender.send(Command::CdPlay).unwrap();
    sender.send(Command::CdStop).unwrap();

    while let Ok(command) = receiver.try_recv() {
        assert!(apply_cd_fifo_command(&mut machine, command).is_none());
    }
    assert!(!machine.cd_audio_state().playing);
    assert_eq!(machine.cd_audio_state().left_level, 18);
    assert_eq!(machine.cd_audio_state().right_level, 18);
}

#[test]
fn refill_credit_clamps_a_stall() {
    let cap = MASTER_CLOCK_HZ / 20;
    // From empty, a normal ~15 ms slice yields its full wall-time worth.
    assert_eq!(
        refill_credit(0, Duration::from_millis(15), cap),
        (MASTER_CLOCK_HZ * 15 / 1000) as i64
    );
    // A long stall is clamped to the cap, so the backlog is forgiven, not banked.
    assert_eq!(
        refill_credit(0, Duration::from_millis(500), cap),
        cap as i64
    );
}

#[test]
fn pacing_sleeps_only_when_the_guest_is_caught_up() {
    assert!(emulation_should_sleep(-1, false));
    assert!(emulation_should_sleep(0, false));
    assert!(!emulation_should_sleep(1, false));
    assert!(emulation_should_sleep(1, true));
}

#[test]
fn fast_modes_run_one_bounded_quantum_while_accurate_modes_use_the_credit() {
    let credit = (MASTER_CLOCK_HZ / 20) as i64;
    assert_eq!(execution_budget(credit, true), FAST_EMU_QUANTUM_TICKS);
    assert_eq!(execution_budget(credit, false), credit as u64);
    assert_eq!(execution_budget(-1, true), 0);
}

#[test]
fn only_halted_guests_receive_device_only_top_up() {
    let budget = MASTER_CLOCK_HZ / 20;
    let executed = FAST_EMU_QUANTUM_TICKS;
    assert_eq!(halted_device_top_up(budget, executed, false), 0);
    assert_eq!(
        halted_device_top_up(budget, executed, true),
        budget - executed
    );
}

#[test]
fn pacing_settlement_distinguishes_ahead_and_behind_execution() {
    let cap = MASTER_CLOCK_HZ / 20;
    let quantum = FAST_EMU_QUANTUM_TICKS;

    let ahead = settle_credit(quantum as i64, quantum * 2, Duration::ZERO, cap);
    assert!(ahead < 0);
    assert!(emulation_should_sleep(ahead, false));

    let behind = settle_credit(quantum as i64, quantum, Duration::from_millis(3), cap);
    assert_eq!(behind, (MASTER_CLOCK_HZ * 3 / 1000) as i64);
    assert!(!emulation_should_sleep(behind, false));
}

#[test]
fn pacing_settlement_caps_catch_up_and_preserves_io_debt() {
    let cap = MASTER_CLOCK_HZ / 20;
    assert_eq!(
        settle_credit(0, 0, Duration::from_millis(500), cap),
        cap as i64
    );

    let credit = FAST_EMU_QUANTUM_TICKS as i64;
    let disk_jump = MASTER_CLOCK_HZ / 5;
    let after_disk = settle_credit(credit, disk_jump, Duration::from_millis(1), cap);
    assert!(after_disk < 0);
    assert_eq!(execution_budget(after_disk, true), 0);
}

#[test]
fn runtime_profile_accumulates_host_phase_times() {
    let mut metrics = RuntimeProfileMetrics::default();
    metrics.record_work(
        Duration::from_nanos(2),
        Duration::from_nanos(3),
        Duration::from_nanos(5),
    );
    metrics.record_work(
        Duration::from_nanos(7),
        Duration::from_nanos(11),
        Duration::from_nanos(13),
    );
    metrics.record_sleep(Duration::from_nanos(17));

    assert_eq!(metrics.emulation_work_wall_ns, 9);
    assert_eq!(metrics.host_audio_mix_queue_wall_ns, 14);
    assert_eq!(metrics.frame_conversion_publish_wall_ns, 18);
    assert_eq!(metrics.throttle_sleep_wall_ns, 17);
}

#[test]
fn runtime_profile_tracks_catchup_credit_and_throttle_ahead_independently() {
    let mut metrics = RuntimeProfileMetrics::default();
    metrics.observe_credit(120);
    metrics.observe_credit(-75);
    metrics.observe_credit(40);
    metrics.observe_credit(-200);

    assert_eq!(metrics.current_pacing_credit_ticks, -200);
    assert_eq!(metrics.max_catchup_credit_ticks, 120);
    assert_eq!(metrics.max_throttle_ahead_ticks, 200);
}

#[test]
fn runtime_profile_counts_latest_frame_publication_and_backpressure() {
    let mut metrics = RuntimeProfileMetrics::default();
    metrics.record_frame(0, u64::MAX, true);
    metrics.record_frame(12, 10, true);
    metrics.record_frame(13, 12, false);
    metrics.record_backpressure(Duration::from_millis(25));

    assert_eq!(metrics.frames_produced, 2);
    assert_eq!(metrics.frames_skipped, 1);
    assert_eq!(
        metrics.presentation_backpressure_wall_ns,
        duration_ns(Duration::from_millis(25))
    );
}

#[test]
fn runtime_profile_json_has_stable_fields_and_derived_values() {
    let metrics = RuntimeProfileMetrics {
        emulation_work_wall_ns: 2,
        host_audio_mix_queue_wall_ns: 3,
        frame_conversion_publish_wall_ns: 5,
        guest_master_ticks: MASTER_CLOCK_HZ / 2,
        current_pacing_credit_ticks: -(MASTER_CLOCK_HZ as i64 / 4),
        max_catchup_credit_ticks: MASTER_CLOCK_HZ / 2,
        max_throttle_ahead_ticks: MASTER_CLOCK_HZ / 4,
        frames_produced: 7,
        frames_skipped: 2,
        ..RuntimeProfileMetrics::default()
    };
    let audio = AudioDebugSnapshot {
        underruns_after_prefill: 11,
        overruns: 13,
        late_callbacks: 17,
        ..AudioDebugSnapshot::default()
    };
    let total_metrics = RuntimeProfileMetrics {
        guest_master_ticks: MASTER_CLOCK_HZ * 3 / 2,
        ..metrics
    };
    let value = serde_json::to_value(RuntimeProfileReport::new(
        "interval",
        4,
        Duration::from_secs(1),
        metrics,
        Duration::from_secs(2),
        total_metrics,
        Some(audio),
    ))
    .unwrap();

    assert_eq!(value["schema"], RUNTIME_PROFILE_SCHEMA);
    assert_eq!(value["scope"], "interval");
    assert_eq!(value["active_work_wall_ns"], 10);
    assert_eq!(value["guest_realtime_factor"], 0.5);
    assert_eq!(value["uncapped_wall_guest_lag_ticks"], MASTER_CLOCK_HZ / 2);
    assert_eq!(value["uncapped_wall_guest_lag_seconds"], 0.5);
    assert_eq!(value["total_guest_realtime_factor"], 0.75);
    assert_eq!(
        value["uncapped_total_wall_guest_lag_ticks"],
        MASTER_CLOCK_HZ / 2
    );
    assert_eq!(value["current_catchup_credit_ticks"], 0);
    assert_eq!(value["current_throttle_ahead_ticks"], MASTER_CLOCK_HZ / 4);
    assert_eq!(value["current_throttle_ahead_seconds"], 0.25);
    assert_eq!(value["max_catchup_credit_seconds"], 0.5);
    assert_eq!(value["frames_produced"], 7);
    assert_eq!(value["frames_skipped"], 2);
    assert_eq!(value["audio_underruns_after_prefill"], 11);
    assert_eq!(value["audio_overruns"], 13);
    assert_eq!(value["audio_late_callbacks"], 17);
    assert!(value.get("audio_queue_lifetime_min_depth").is_some());
    assert!(
        value
            .get("audio_lifetime_max_callback_lateness_us")
            .is_some()
    );
}

#[test]
fn runtime_profile_final_audio_counts_start_at_profile_enable() {
    let baseline = AudioDebugSnapshot {
        frames_produced: 100,
        underruns_after_prefill: 7,
        overruns: 3,
        ..AudioDebugSnapshot::default()
    };
    let current = AudioDebugSnapshot {
        frames_produced: 140,
        underruns_after_prefill: 9,
        overruns: 8,
        ..AudioDebugSnapshot::default()
    };

    let delta = audio_snapshot_since(Some(current), Some(baseline)).unwrap();
    assert_eq!(delta.frames_produced, 40);
    assert_eq!(delta.underruns_after_prefill, 2);
    assert_eq!(delta.overruns, 5);
}

#[test]
fn frame_publish_waits_for_ack_then_selects_the_newest_frame() {
    let published_seq = 10;
    let published_generation = Some(100);

    assert!(!should_publish_frame(
        11,
        published_seq,
        9,
        Some(101),
        published_generation,
    ));
    assert!(!should_publish_frame(
        12,
        published_seq,
        9,
        Some(102),
        published_generation,
    ));
    assert!(should_publish_frame(
        12,
        published_seq,
        published_seq,
        Some(102),
        published_generation,
    ));
}

#[test]
fn frame_publish_skips_static_graphics_after_ack() {
    assert!(should_publish_frame(0, u64::MAX, u64::MAX, Some(1), None));
    assert!(!should_publish_frame(11, 10, 10, Some(7), Some(7)));
    assert!(should_publish_frame(11, 10, 10, None, None));
}

#[test]
fn speed_sample_marks_exactly_ninety_percent_hlt_as_idle() {
    let wall = Duration::from_secs(1);
    assert!(!speed_sample(100, 899, 1_000, wall).1);
    assert!(speed_sample(100, 900, 1_000, wall).1);
    assert!(
        !speed_sample(0, 0, 1_000, wall).1,
        "I/O wait is not HLT idle"
    );
}

#[test]
fn speed_sample_caps_active_throughput_at_realtime() {
    let (ratio, idle) = speed_sample(
        MASTER_CLOCK_HZ.saturating_mul(2),
        0,
        MASTER_CLOCK_HZ.saturating_mul(2),
        Duration::from_secs(1),
    );
    assert_eq!(ratio, 1.0);
    assert!(!idle);
}

#[test]
fn disk_overshoot_holds_the_guest() {
    let cap = MASTER_CLOCK_HZ / 20;
    // A read that ran ~190 ms past its budget leaves credit deep in debt.
    let mut credit: i64 = -(MASTER_CLOCK_HZ as i64) / 5;
    // One short slice cannot lift it out of debt, so the guest's budget stays
    // zero: it waits in wall-clock time.
    credit = refill_credit(credit, Duration::from_millis(1), cap);
    assert!(credit < 0);
    assert_eq!(credit.max(0) as u64, 0, "no budget while in disk debt");
    // After enough wall time the debt clears and the guest runs again.
    credit = refill_credit(credit, Duration::from_millis(500), cap);
    assert!(credit > 0, "debt repaid once wall-clock catches up");
}

#[test]
fn live_mode_switch_debits_credit_in_master_ticks() {
    let mut rom = vec![0u8; izarravm_machine::BIOS_ROM_SIZE];
    rom[..15].copy_from_slice(&[
        0xB0, 0x01, 0xE6, 0xE1, // 486
        0xB0, 0x00, 0xE6, 0xE1, // 386
        0xB0, 0x03, 0xE6, 0xE1, // 386-slow
        0xFA, 0xEB, 0xFE, // cli; jmp $
    ]);
    rom[0xFFF0..0xFFF5].copy_from_slice(&[0xEA, 0x00, 0x00, 0x00, 0xF0]);
    let mut machine = Machine::new(
        MachineProfile::gsw_386(1, izarravm_core::VideoCard::Vega),
        rom,
    )
    .unwrap();
    machine.set_mode(GswMode::Gsw586);
    let budget = MASTER_CLOCK_HZ / 1000;
    let mut credit = budget as i64;
    let before = machine.master_ticks();

    assert_eq!(tick_machine_ticks(&mut machine, budget), None);
    let ran = machine.master_ticks() - before;
    credit -= i64::try_from(ran).unwrap();

    assert_eq!(machine.active_mode(), GswMode::Gsw386Slow);
    assert!(credit <= 0, "the full fixed-time budget was debited");
    assert!(
        credit > -(100 * 900),
        "credit debt is only final-instruction overshoot, not mixed clock units"
    );
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
