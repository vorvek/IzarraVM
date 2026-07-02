#![forbid(unsafe_code)]

use izarravm_core::{AudioConfig, MidiBackend};

mod dsp;
mod mixer;
mod opl;
mod output;
mod pcm;
mod resample;
mod wss;
mod yamaha_adpcm;

pub use dsp::SbDsp;
pub use mixer::SbMixer;
pub use opl::OplChip;
pub use output::{AudioPlayer, AudioSink};
pub use resample::Resampler;
pub use wss::{Ad1848, Ad1848Config};
pub use yamaha_adpcm::{
    AdpcmConfig, AdpcmFormat, YamahaAdpcmChip, decode_adpcm_a, decode_adpcm_b, decode_aica,
    decode_ymz280b, encode_adpcm_b,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioDeviceKind {
    PcSpeaker,
    SoundBlaster,
    Wss,
    Opl3,
    YamahaAdpcm,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MixerConfig {
    pub sample_rate_hz: u32,
    pub channels: u16,
}

impl Default for MixerConfig {
    fn default() -> Self {
        // The Resonique 2 (SB16-class) DAC tops out at 44.1 kHz stereo; 48 kHz
        // would be anachronistic for this machine.
        Self {
            sample_rate_hz: 44_100,
            channels: 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioSubsystem {
    pub mixer: MixerConfig,
    pub devices: Vec<AudioDeviceKind>,
    pub midi_backend: MidiBackend,
}

impl AudioSubsystem {
    pub fn from_config(config: &AudioConfig) -> Self {
        let mut devices = Vec::new();
        if config.pc_speaker {
            devices.push(AudioDeviceKind::PcSpeaker);
        }
        if config.sound_blaster.enabled {
            devices.push(AudioDeviceKind::SoundBlaster);
        }
        if config.wss.enabled {
            devices.push(AudioDeviceKind::Wss);
        }
        if config.opl3 {
            devices.push(AudioDeviceKind::Opl3);
        }

        Self {
            mixer: MixerConfig::default(),
            devices,
            midi_backend: config.midi.backend,
        }
    }
}

pub fn cpal_backend_marker() -> &'static str {
    std::any::type_name::<cpal::StreamConfig>()
}

pub fn midir_backend_marker() -> &'static str {
    std::any::type_name::<midir::MidiOutput>()
}

#[cfg(test)]
mod tests {
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
}
