use izarravm_core::{AudioConfig, MidiBackend};

mod opl;

pub use opl::OplChip;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioDeviceKind {
    PcSpeaker,
    SoundBlaster,
    Opl3,
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
        if config.sound_blaster {
            devices.push(AudioDeviceKind::SoundBlaster);
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
        let subsystem = AudioSubsystem::from_config(&config);
        assert_eq!(
            subsystem.devices,
            vec![AudioDeviceKind::PcSpeaker, AudioDeviceKind::SoundBlaster]
        );

        config.sound_blaster = false;
        let subsystem = AudioSubsystem::from_config(&config);
        assert_eq!(subsystem.devices, vec![AudioDeviceKind::PcSpeaker]);
    }
}
