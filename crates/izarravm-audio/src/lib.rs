// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

#![forbid(unsafe_code)]

mod dsp;
mod midi;
mod mixer;
mod mpu401;
mod opl;
mod output;
mod pcm;
mod resample;
mod soundfont;
mod wss;

pub use dsp::SbDsp;
pub use midi::MidiEngine;
pub use mixer::SbMixer;
pub use mpu401::{Mpu401, TimedMidiMessage};
pub use opl::OplChip;
pub use output::{AudioDebugSnapshot, AudioPlayer, AudioSink};
pub use resample::Resampler;
pub use soundfont::{EMBEDDED_SOUNDFONT_SHA256, embedded_soundfont_path};
pub use wss::{Ad1848, Ad1848Config, BoardIrqStrobe, trace_enabled as wss_trace_enabled};
