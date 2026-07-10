// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Host audio output at 44.1 kHz stereo.
//!
//! The emulation thread writes PCM into a bounded lock-free queue. The cpal
//! callback drains it without taking a mutex and resamples to the host rate.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use crossbeam_queue::ArrayQueue;
use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};

type StereoFrame = (i16, i16);

/// The rate used by the emulator mixer.
const SOURCE_HZ: u32 = 44_100;
const TARGET_FRAMES: usize = SOURCE_HZ as usize * 30 / 1_000;
const LOW_FRAMES: usize = (SOURCE_HZ as usize * 15).div_ceil(1_000);
const HIGH_FRAMES: usize = SOURCE_HZ as usize * 60 / 1_000;
const CAPACITY_FRAMES: usize = SOURCE_HZ as usize * 100 / 1_000;
const RAMP_FRAMES: u16 = 64;
const CALLBACK_LATE_TOLERANCE_NS: u128 = 1_000_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct AudioDebugSnapshot {
    frames_produced: u64,
    frames_consumed: u64,
    queue_min_depth: usize,
    queue_max_depth: usize,
    low_water_writes: u64,
    underruns_after_prefill: u64,
    overruns: u64,
    late_callbacks: u64,
    callback_lateness_us: u64,
    max_callback_lateness_us: u64,
}

#[derive(Debug)]
struct AudioDebugCounters {
    frames_produced: AtomicU64,
    frames_consumed: AtomicU64,
    queue_min_depth: AtomicUsize,
    queue_max_depth: AtomicUsize,
    low_water_writes: AtomicU64,
    underruns_after_prefill: AtomicU64,
    overruns: AtomicU64,
    late_callbacks: AtomicU64,
    callback_lateness_us: AtomicU64,
    max_callback_lateness_us: AtomicU64,
}

impl AudioDebugCounters {
    fn new(queue_depth: usize) -> Self {
        Self {
            frames_produced: AtomicU64::new(0),
            frames_consumed: AtomicU64::new(0),
            queue_min_depth: AtomicUsize::new(queue_depth),
            queue_max_depth: AtomicUsize::new(queue_depth),
            low_water_writes: AtomicU64::new(0),
            underruns_after_prefill: AtomicU64::new(0),
            overruns: AtomicU64::new(0),
            late_callbacks: AtomicU64::new(0),
            callback_lateness_us: AtomicU64::new(0),
            max_callback_lateness_us: AtomicU64::new(0),
        }
    }

    fn observe_queue_depth(&self, depth: usize) {
        self.queue_min_depth.fetch_min(depth, Ordering::Relaxed);
        self.queue_max_depth.fetch_max(depth, Ordering::Relaxed);
    }

    fn record_callback_lateness(&self, lateness_ns: u128) {
        if lateness_ns <= CALLBACK_LATE_TOLERANCE_NS {
            return;
        }
        let lateness_us = u64::try_from(lateness_ns / 1_000).unwrap_or(u64::MAX);
        self.late_callbacks.fetch_add(1, Ordering::Relaxed);
        self.callback_lateness_us
            .fetch_add(lateness_us, Ordering::Relaxed);
        self.max_callback_lateness_us
            .fetch_max(lateness_us, Ordering::Relaxed);
    }

    fn snapshot(&self) -> AudioDebugSnapshot {
        AudioDebugSnapshot {
            frames_produced: self.frames_produced.load(Ordering::Relaxed),
            frames_consumed: self.frames_consumed.load(Ordering::Relaxed),
            queue_min_depth: self.queue_min_depth.load(Ordering::Relaxed),
            queue_max_depth: self.queue_max_depth.load(Ordering::Relaxed),
            low_water_writes: self.low_water_writes.load(Ordering::Relaxed),
            underruns_after_prefill: self.underruns_after_prefill.load(Ordering::Relaxed),
            overruns: self.overruns.load(Ordering::Relaxed),
            late_callbacks: self.late_callbacks.load(Ordering::Relaxed),
            callback_lateness_us: self.callback_lateness_us.load(Ordering::Relaxed),
            max_callback_lateness_us: self.max_callback_lateness_us.load(Ordering::Relaxed),
        }
    }
}

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
    debug: Option<Arc<AudioDebugCounters>>,
}

impl AudioSink {
    /// Queue mixer frames while holding the buffer near its 30 ms target.
    ///
    /// Falling below 15 ms records producer starvation and appends new audio
    /// immediately. Rising above 60 ms discards old latency and inserts a short
    /// fade boundary. The queue itself is capped at 100 ms, so a stalled callback
    /// cannot grow memory or leave sound far behind the guest.
    pub fn queue(&self, frames: &[StereoFrame]) {
        if frames.is_empty() {
            return;
        }
        if let Some(debug) = &self.debug {
            debug
                .frames_produced
                .fetch_add(frames.len() as u64, Ordering::Relaxed);
        }

        let queued = self.ring.len();
        if queued < LOW_FRAMES
            && let Some(debug) = &self.debug
        {
            debug.low_water_writes.fetch_add(1, Ordering::Relaxed);
        }
        let projected = queued.saturating_add(frames.len());
        if projected > HIGH_FRAMES {
            if let Some(debug) = &self.debug {
                debug.overruns.fetch_add(1, Ordering::Relaxed);
            }
            recover_to_target(&self.ring, frames);
            if let Some(debug) = &self.debug {
                debug.observe_queue_depth(self.ring.len());
            }
            return;
        }

        for &frame in frames {
            if self.ring.push(QueuedFrame::Audio(frame)).is_err() {
                if let Some(debug) = &self.debug {
                    debug.overruns.fetch_add(1, Ordering::Relaxed);
                }
                break;
            }
        }
        if let Some(debug) = &self.debug {
            debug.observe_queue_depth(self.ring.len());
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
        let debug = std::env::var_os("IZARRAVM_AUDIO_DEBUG")
            .is_some()
            .then(|| Arc::new(AudioDebugCounters::new(ring.len())));

        let stream = match sample_format {
            cpal::SampleFormat::F32 => {
                build_stream::<f32>(&device, &config, Arc::clone(&ring), debug.clone())
            }
            cpal::SampleFormat::I16 => {
                build_stream::<i16>(&device, &config, Arc::clone(&ring), debug.clone())
            }
            cpal::SampleFormat::U16 => {
                build_stream::<u16>(&device, &config, Arc::clone(&ring), debug.clone())
            }
            other => return Err(format!("unsupported audio sample format: {other:?}").into()),
        }?;
        stream.play()?;

        Ok(Self {
            _stream: stream,
            sink: AudioSink { ring, debug },
        })
    }

    /// Return a handle that can feed this stream from another thread.
    pub fn sink(&self) -> AudioSink {
        self.sink.clone()
    }
}

struct CallbackSource {
    ring: Arc<ArrayQueue<QueuedFrame>>,
    debug: Option<Arc<AudioDebugCounters>>,
    last: StereoFrame,
    gain: u16,
    underruns: u64,
    prefill_remaining: usize,
}

impl CallbackSource {
    fn with_debug(
        ring: Arc<ArrayQueue<QueuedFrame>>,
        debug: Option<Arc<AudioDebugCounters>>,
    ) -> Self {
        Self {
            ring,
            debug,
            last: (0, 0),
            gain: 0,
            underruns: 0,
            prefill_remaining: TARGET_FRAMES,
        }
    }

    fn next(&mut self) -> StereoFrame {
        if let Some(debug) = &self.debug {
            debug.frames_consumed.fetch_add(1, Ordering::Relaxed);
        }
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
                if self.prefill_remaining == 0
                    && let Some(debug) = &self.debug
                {
                    debug
                        .underruns_after_prefill
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        if let Some(debug) = &self.debug {
            self.prefill_remaining = self.prefill_remaining.saturating_sub(1);
            debug.observe_queue_depth(self.ring.len());
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
    debug: Option<Arc<AudioDebugCounters>>,
) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let out_hz = config.sample_rate.0 as i64;
    let mut source = CallbackSource::with_debug(ring, debug.clone());
    let mut cur = (0i16, 0i16);
    let mut next = (0i16, 0i16);
    let mut phase = 0i64;
    let mut out_frames = 0u64;
    let mut reported = AudioDebugSnapshot::default();
    let mut previous_callback = None;
    let mut previous_callback_frames = 0usize;

    device.build_output_stream(
        config,
        move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
            let callback_frames = data.len() / channels.max(1);
            if let Some(debug) = &debug {
                let callback = info.timestamp().callback;
                if let Some(previous) = previous_callback
                    && let Some(elapsed) = callback.duration_since(&previous)
                {
                    let expected_ns = previous_callback_frames as u128 * 1_000_000_000
                        / out_hz as u128;
                    debug.record_callback_lateness(elapsed.as_nanos().saturating_sub(expected_ns));
                }
                previous_callback = Some(callback);
                previous_callback_frames = callback_frames;
            }

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

            if let Some(debug) = &debug {
                out_frames += callback_frames as u64;
                if out_frames >= out_hz as u64 {
                    let snapshot = debug.snapshot();
                    eprintln!(
                        "[AUDIO] produced/s={} consumed/s={} ring_now={} ring_min={} ring_max={} low_water_writes/s={} underruns_after_prefill/s={} overruns/s={} late_callbacks/s={} callback_lateness_us/s={} max_callback_lateness_us={}",
                        snapshot.frames_produced.saturating_sub(reported.frames_produced),
                        snapshot.frames_consumed.saturating_sub(reported.frames_consumed),
                        source.ring.len(),
                        snapshot.queue_min_depth,
                        snapshot.queue_max_depth,
                        snapshot
                            .low_water_writes
                            .saturating_sub(reported.low_water_writes),
                        snapshot
                            .underruns_after_prefill
                            .saturating_sub(reported.underruns_after_prefill),
                        snapshot.overruns.saturating_sub(reported.overruns),
                        snapshot
                            .late_callbacks
                            .saturating_sub(reported.late_callbacks),
                        snapshot
                            .callback_lateness_us
                            .saturating_sub(reported.callback_lateness_us),
                        snapshot.max_callback_lateness_us,
                    );
                    reported = snapshot;
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
