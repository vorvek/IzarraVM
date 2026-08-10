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
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

type StereoFrame = (i16, i16);

/// The rate used by the emulator mixer.
const SOURCE_HZ: u32 = 44_100;
const TARGET_FRAMES: usize = SOURCE_HZ as usize * 30 / 1_000;
const LOW_FRAMES: usize = (SOURCE_HZ as usize * 15).div_ceil(1_000);
const HIGH_FRAMES: usize = SOURCE_HZ as usize * 60 / 1_000;
const CAPACITY_FRAMES: usize = SOURCE_HZ as usize * 100 / 1_000;
const RAMP_FRAMES: u16 = 64;
const CALLBACK_LATE_TOLERANCE_NS: u128 = 1_000_000;
/// How long to wait between attempts to reopen a failed output stream. A device
/// that has just gone is not coming back this frame.
const STREAM_RETRY_INTERVAL: Duration = Duration::from_millis(750);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AudioDebugSnapshot {
    pub frames_produced: u64,
    pub frames_consumed: u64,
    pub queue_min_depth: usize,
    pub queue_max_depth: usize,
    pub low_water_writes: u64,
    pub underruns_after_prefill: u64,
    pub overruns: u64,
    pub late_callbacks: u64,
    pub callback_lateness_us: u64,
    pub max_callback_lateness_us: u64,
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
        self.observe_queue_range(depth, depth);
    }

    fn observe_queue_range(&self, min_depth: usize, max_depth: usize) {
        self.queue_min_depth.fetch_min(min_depth, Ordering::Relaxed);
        self.queue_max_depth.fetch_max(max_depth, Ordering::Relaxed);
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
    /// A sink with no output stream behind it, for tests and for any caller
    /// that wants to drive the audio path without a sound device.
    ///
    /// The queue behaves exactly as a live one does -- same capacity, same
    /// high-water recovery -- it is simply never drained by a callback. That is
    /// the point: it makes what the emulation thread QUEUES observable, which is
    /// otherwise only visible by listening.
    pub fn detached() -> Self {
        Self {
            ring: new_ring(),
            debug: None,
        }
    }

    /// Take every audio frame currently queued, discarding the padding a fresh
    /// queue is primed with. Pairs with [`detached`](Self::detached).
    pub fn take_queued_frames(&self) -> Vec<StereoFrame> {
        let mut frames = Vec::new();
        while let Some(queued) = self.ring.pop() {
            if let QueuedFrame::Audio(frame) = queued {
                frames.push(frame);
            }
        }
        frames
    }

    /// Return the optional diagnostic counters without exposing the mutable
    /// atomics shared with the audio callback.
    pub fn debug_snapshot(&self) -> Option<AudioDebugSnapshot> {
        self.debug.as_ref().map(|debug| debug.snapshot())
    }

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
    stream: cpal::Stream,
    sink: AudioSink,
    audio_debug: bool,
    /// Raised by the cpal error callback, from whatever thread cpal runs it on.
    /// A stream that has errored never calls back again, so this is the only
    /// evidence there is that the machine has gone silent.
    failed: Arc<AtomicBool>,
    /// When the next rebuild may be attempted. A device that is gone stays gone
    /// for a while, and retrying it every frame would spend the UI thread
    /// enumerating audio endpoints.
    retry_after: Option<Instant>,
}

impl AudioPlayer {
    /// Open the default output device at its preferred format.
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let ring = new_ring();
        let audio_debug = std::env::var_os("IZARRAVM_AUDIO_DEBUG").is_some();
        let runtime_profile = std::env::var("IZARRAVM_RUNTIME_PROFILE").as_deref() == Ok("1");
        let debug =
            (audio_debug || runtime_profile).then(|| Arc::new(AudioDebugCounters::new(ring.len())));
        let failed = Arc::new(AtomicBool::new(false));
        let stream = open_stream(Arc::clone(&ring), debug.clone(), audio_debug, &failed)?;

        Ok(Self {
            stream,
            sink: AudioSink { ring, debug },
            audio_debug,
            failed,
            retry_after: None,
        })
    }

    /// Return a handle that can feed this stream from another thread.
    pub fn sink(&self) -> AudioSink {
        self.sink.clone()
    }

    /// Rebuild the output stream if the running one has failed. Call this from
    /// the thread that owns the player, once in a while (the GUI does it each
    /// frame); returns true when a stream was successfully replaced.
    ///
    /// A cpal stream that reports an error is finished: it stops calling back
    /// and never resumes, so the machine plays to nothing for the rest of the
    /// session. That is what a device change does -- unplugging a headset,
    /// Windows moving the default endpoint, a driver reset -- and it used to be
    /// handled by printing one line to stderr. The default device is re-queried
    /// on every attempt, so the new stream follows the endpoint the host has
    /// moved to rather than reopening the one that vanished.
    ///
    /// The QUEUE survives: the new stream is built on the same ring the
    /// emulation thread is already writing to, so nothing has to be told that
    /// this happened and no audio staged in the meantime is lost.
    pub fn poll_recover(&mut self) -> bool {
        if !self.failed.load(Ordering::Acquire) {
            return false;
        }
        let now = Instant::now();
        if self.retry_after.is_some_and(|at| now < at) {
            return false;
        }
        self.retry_after = Some(now + STREAM_RETRY_INTERVAL);
        match open_stream(
            Arc::clone(&self.sink.ring),
            self.sink.debug.clone(),
            self.audio_debug,
            &self.failed,
        ) {
            Ok(stream) => {
                // Clear BEFORE installing. The new stream shares the flag, and
                // it can fail the moment it starts -- during the assignment
                // below, which also drops the old one. Clearing afterwards would
                // wipe that report and leave the machine playing to a device
                // that had already gone. The other way round costs at worst one
                // extra rebuild, and only when the outgoing stream complains on
                // its way out.
                self.failed.store(false, Ordering::Release);
                self.stream = stream;
                self.retry_after = None;
                eprintln!("izarravm audio: output stream reopened after a device error");
                true
            }
            Err(error) => {
                eprintln!("izarravm audio: could not reopen the output stream: {error}");
                false
            }
        }
    }
}

/// Open a stream on the CURRENT default output device, feeding `ring`.
fn open_stream(
    ring: Arc<ArrayQueue<QueuedFrame>>,
    debug: Option<Arc<AudioDebugCounters>>,
    audio_debug: bool,
    failed: &Arc<AtomicBool>,
) -> Result<cpal::Stream, Box<dyn std::error::Error>> {
    let device = cpal::default_host()
        .default_output_device()
        .ok_or("no default audio output device")?;
    let supported = device.default_output_config()?;
    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let stream = match sample_format {
        cpal::SampleFormat::F32 => {
            build_stream::<f32>(&device, &config, ring, debug, audio_debug, failed)
        }
        cpal::SampleFormat::I16 => {
            build_stream::<i16>(&device, &config, ring, debug, audio_debug, failed)
        }
        cpal::SampleFormat::U16 => {
            build_stream::<u16>(&device, &config, ring, debug, audio_debug, failed)
        }
        other => return Err(format!("unsupported audio sample format: {other:?}").into()),
    }?;
    stream.play()?;
    Ok(stream)
}

struct CallbackSource {
    ring: Arc<ArrayQueue<QueuedFrame>>,
    debug: Option<Arc<AudioDebugCounters>>,
    last: StereoFrame,
    gain: u16,
    underruns: u64,
    prefill_remaining: usize,
    debug_frames_consumed: u64,
    debug_underruns_after_prefill: u64,
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
            debug_frames_consumed: 0,
            debug_underruns_after_prefill: 0,
        }
    }

    fn next(&mut self) -> StereoFrame {
        if self.debug.is_some() {
            self.debug_frames_consumed = self.debug_frames_consumed.saturating_add(1);
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
                if self.prefill_remaining == 0 && self.debug.is_some() {
                    self.debug_underruns_after_prefill =
                        self.debug_underruns_after_prefill.saturating_add(1);
                }
            }
        }
        if self.debug.is_some() {
            self.prefill_remaining = self.prefill_remaining.saturating_sub(1);
        }
        scale_frame(self.last, self.gain)
    }

    fn flush_debug_callback(&mut self, queue_depth_before: Option<usize>) {
        let (Some(debug), Some(queue_depth_before)) = (&self.debug, queue_depth_before) else {
            return;
        };
        let queue_depth_after = self.ring.len();
        if self.debug_frames_consumed != 0 {
            debug
                .frames_consumed
                .fetch_add(self.debug_frames_consumed, Ordering::Relaxed);
        }
        if self.debug_underruns_after_prefill != 0 {
            debug
                .underruns_after_prefill
                .fetch_add(self.debug_underruns_after_prefill, Ordering::Relaxed);
        }
        let min_depth = if self.debug_underruns_after_prefill == 0 {
            queue_depth_before.min(queue_depth_after)
        } else {
            0
        };
        debug.observe_queue_range(min_depth, queue_depth_before.max(queue_depth_after));
        self.debug_frames_consumed = 0;
        self.debug_underruns_after_prefill = 0;
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
    emit_debug_log: bool,
    failed: &Arc<AtomicBool>,
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
            let queue_depth_before = debug.as_ref().map(|_| source.ring.len());
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
            source.flush_debug_callback(queue_depth_before);

            if emit_debug_log && let Some(debug) = &debug {
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
        {
            // A cpal stream never recovers on its own: after an error it stops
            // calling back for good. Raising the flag is what lets the owning
            // thread notice and rebuild -- printing alone left the machine
            // playing to a dead device with nothing on screen to say so.
            let failed = Arc::clone(failed);
            move |error| {
                eprintln!("izarravm audio: output stream error: {error}");
                failed.store(true, Ordering::Release);
            }
        },
        None,
    )
}

#[cfg(test)]
#[path = "output_test.rs"]
mod tests;
