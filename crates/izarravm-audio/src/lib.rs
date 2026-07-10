#![forbid(unsafe_code)]

use izarravm_core::{AudioConfig, MidiBackend};

mod dsp;
mod mixer;
mod opl;
mod output;
mod pcm;
mod resample;
mod soundfont;
mod wss;

pub use dsp::SbDsp;
pub use mixer::SbMixer;
pub use opl::OplChip;
pub use output::{AudioPlayer, AudioSink};
pub use resample::Resampler;
pub use soundfont::{EMBEDDED_SOUNDFONT_SHA256, embedded_soundfont_path};
pub use wss::{Ad1848, Ad1848Config};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioDeviceKind {
    PcSpeaker,
    SoundBlaster,
    Wss,
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
#[path = "audio_test.rs"]
mod tests;
