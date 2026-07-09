// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn enabled_audio_devices_follow_config() {
    let mut config = AudioConfig {
        opl3: false,
        ..AudioConfig::default()
    };
    config.wss.enabled = false;
    let subsystem = AudioSubsystem::from_config(&config);
    assert_eq!(
        subsystem.devices,
        vec![AudioDeviceKind::PcSpeaker, AudioDeviceKind::SoundBlaster]
    );

    config.sound_blaster.enabled = false;
    let subsystem = AudioSubsystem::from_config(&config);
    assert_eq!(subsystem.devices, vec![AudioDeviceKind::PcSpeaker]);
}

#[test]
fn wss_device_present_when_enabled_and_absent_when_disabled() {
    // The AD1848 codec is always present on the ReSonique 2 combo card, so the
    // default config enables it: the Wss device sits after SoundBlaster.
    let config = AudioConfig::default();
    assert!(config.wss.enabled, "WSS enabled by default");
    let subsystem = AudioSubsystem::from_config(&config);
    assert!(
        subsystem.devices.contains(&AudioDeviceKind::Wss),
        "Wss device present when enabled"
    );

    // Disabling it drops the Wss device while leaving the rest intact.
    let config = AudioConfig {
        wss: izarravm_core::WssConfig {
            enabled: false,
            ..Default::default()
        },
        ..AudioConfig::default()
    };
    let subsystem = AudioSubsystem::from_config(&config);
    assert!(
        !subsystem.devices.contains(&AudioDeviceKind::Wss),
        "Wss device absent when disabled"
    );
}
