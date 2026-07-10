// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Host audio output at 44.1 kHz stereo.
//!
//! The emulation thread writes PCM into a bounded lock-free queue. The cpal
//! callback drains it without taking a mutex and resamples to the host rate.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use crossbeam_queue::ArrayQueue;
use std::sync::Arc;

type StereoFrame = (i16, i16);

/// The rate used by the emulator mixer.
const SOURCE_HZ: u32 = 44_100;
const TARGET_FRAMES: usize = SOURCE_HZ as usize * 30 / 1_000;
const LOW_FRAMES: usize = (SOURCE_HZ as usize * 15).div_ceil(1_000);
const HIGH_FRAMES: usize = SOURCE_HZ as usize * 60 / 1_000;
const CAPACITY_FRAMES: usize = SOURCE_HZ as usize * 100 / 1_000;
const RAMP_FRAMES: u16 = 64;

#[derive(Debug, Clone, Copy)]
enum QueuedFrame {
    Audio(StereoFrame),
    Padding,
}

fn new_ring() -> Arc<ArrayQueue<QueuedFrame>> {
    let ring = Arc::new(ArrayQueue::new(CAPACITY_FRAMES));
    push_padding(&ring, TARGET_FRAMES);
    ring
}

fn push_padding(ring: &ArrayQueue<QueuedFrame>, count: usize) {
    for _ in 0..count {
        if ring.push(QueuedFrame::Padding).is_err() {
            break;
        }
    }
}

fn recover_to_target(ring: &ArrayQueue<QueuedFrame>, frames: &[StereoFrame]) {
    while ring.pop().is_some() {}

    let audio_count = frames.len().min(TARGET_FRAMES - usize::from(RAMP_FRAMES));
    push_padding(ring, TARGET_FRAMES - audio_count);
    for &frame in &frames[frames.len() - audio_count..] {
        if ring.push(QueuedFrame::Audio(frame)).is_err() {
            break;
        }
    }
}

/// A sendable handle to the queue feeding the output stream.
#[derive(Clone)]
pub struct AudioSink {
    ring: Arc<ArrayQueue<QueuedFrame>>,
}

impl AudioSink {
    /// Queue mixer frames while holding the buffer near its 30 ms target.
    ///
    /// Falling below 15 ms schedules silence before the new audio. Rising above
    /// 60 ms discards old latency and inserts a short fade boundary. The queue
    /// itself is capped at 100 ms, so a stalled callback cannot grow memory or
    /// leave sound far behind the guest.
    pub fn queue(&self, frames: &[StereoFrame]) {
        if frames.is_empty() {
            return;
        }

        let queued = self.ring.len();
        let planned_padding = if queued < LOW_FRAMES {
            TARGET_FRAMES.saturating_sub(queued)
        } else {
            0
        };
        let projected = queued
            .saturating_add(planned_padding)
            .saturating_add(frames.len());
        if projected > HIGH_FRAMES {
            recover_to_target(&self.ring, frames);
            return;
        }

        push_padding(&self.ring, planned_padding);
        for &frame in frames {
            if self.ring.push(QueuedFrame::Audio(frame)).is_err() {
                break;
            }
        }
    }
}

/// A running output stream and the queue that feeds it.
///
/// The cpal stream is not sendable, so callers keep this value on its creation
/// thread and pass an AudioSink to the emulation thread.
pub struct AudioPlayer {
    _stream: cpal::Stream,
    sink: AudioSink,
}

impl AudioPlayer {
    /// Open the default output device at its preferred format.
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("no default audio output device")?;
        let supported = device.default_output_config()?;
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let ring = new_ring();

        let stream = match sample_format {
            cpal::SampleFormat::F32 => build_stream::<f32>(&device, &config, Arc::clone(&ring)),
            cpal::SampleFormat::I16 => build_stream::<i16>(&device, &config, Arc::clone(&ring)),
            cpal::SampleFormat::U16 => build_stream::<u16>(&device, &config, Arc::clone(&ring)),
            other => return Err(format!("unsupported audio sample format: {other:?}").into()),
        }?;
        stream.play()?;

        Ok(Self {
            _stream: stream,
            sink: AudioSink { ring },
        })
    }

    /// Return a handle that can feed this stream from another thread.
    pub fn sink(&self) -> AudioSink {
        self.sink.clone()
    }
}

struct CallbackSource {
    ring: Arc<ArrayQueue<QueuedFrame>>,
    last: StereoFrame,
    gain: u16,
    underruns: u64,
}

impl CallbackSource {
    fn new(ring: Arc<ArrayQueue<QueuedFrame>>) -> Self {
        Self {
            ring,
            last: (0, 0),
            gain: 0,
            underruns: 0,
        }
    }

    fn next(&mut self) -> StereoFrame {
        match self.ring.pop() {
            Some(QueuedFrame::Audio(frame)) => {
                self.last = frame;
                self.gain = self.gain.saturating_add(1).min(RAMP_FRAMES);
            }
            Some(QueuedFrame::Padding) => {
                self.gain = self.gain.saturating_sub(1);
            }
            None => {
                self.gain = self.gain.saturating_sub(1);
                self.underruns = self.underruns.saturating_add(1);
            }
        }
        scale_frame(self.last, self.gain)
    }
}

fn scale_frame(frame: StereoFrame, gain: u16) -> StereoFrame {
    let scale = |sample: i16| {
        (i32::from(sample) * i32::from(gain) / i32::from(RAMP_FRAMES))
            .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
    };
    (scale(frame.0), scale(frame.1))
}

/// Build a stream that linearly resamples the mixer output to the host rate.
fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    ring: Arc<ArrayQueue<QueuedFrame>>,
) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let out_hz = config.sample_rate.0 as i64;
    let debug = std::env::var_os("IZARRAVM_AUDIO_DEBUG").is_some();
    let mut source = CallbackSource::new(ring);
    let mut cur = (0i16, 0i16);
    let mut next = (0i16, 0i16);
    let mut phase = 0i64;
    let mut out_frames = 0u64;
    let mut reported_underruns = 0u64;

    device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            for frame in data.chunks_mut(channels) {
                let fraction = phase as f32 / out_hz as f32;
                let left = f32::from(cur.0) + (f32::from(next.0) - f32::from(cur.0)) * fraction;
                let right = f32::from(cur.1) + (f32::from(next.1) - f32::from(cur.1)) * fraction;
                if let Some(sample) = frame.get_mut(0) {
                    *sample = T::from_sample(left / 32768.0);
                }
                if let Some(sample) = frame.get_mut(1) {
                    *sample = T::from_sample(right / 32768.0);
                }
                for sample in frame.iter_mut().skip(2) {
                    *sample = T::from_sample(0.0);
                }

                phase += i64::from(SOURCE_HZ);
                while phase >= out_hz {
                    phase -= out_hz;
                    cur = next;
                    next = source.next();
                }
            }

            if debug {
                out_frames += (data.len() / channels.max(1)) as u64;
                if out_frames >= out_hz as u64 {
                    let underruns = source.underruns.saturating_sub(reported_underruns);
                    eprintln!(
                        "[AUDIO] underruns/s={underruns} ring_now={} (0 = keeping up)",
                        source.ring.len()
                    );
                    reported_underruns = source.underruns;
                    out_frames = 0;
                }
            }
        },
        |error| eprintln!("izarravm audio: output stream error: {error}"),
        None,
    )
}

#[cfg(test)]
#[path = "output_test.rs"]
mod tests;
