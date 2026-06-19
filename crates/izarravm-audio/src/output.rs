//! Host audio output: plays queued 44100 Hz stereo PCM through the default
//! device. The emulator pushes resampled OPL frames with [`AudioSink::queue`]
//! and the cpal callback drains them. This is device glue, it can only be
//! exercised by actually running on a machine with an output device, so it is
//! kept small and free of synthesis logic.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

const DAC_HZ: u32 = 44_100;

/// A `Send` handle to the queue feeding the output stream. Clone it onto the
/// emulation thread to push PCM while the stream itself stays put.
#[derive(Clone)]
pub struct AudioSink {
    ring: Arc<Mutex<VecDeque<(i16, i16)>>>,
    capacity: usize,
}

impl AudioSink {
    /// Queue resampled frames for playback, dropping the oldest if the backlog
    /// exceeds ~0.5 s so a faster-than-real-time emulator cannot grow the buffer
    /// without bound. Mutex ring for now; move to lock-free if it ever glitches.
    pub fn queue(&self, frames: &[(i16, i16)]) {
        let mut ring = self.ring.lock().expect("audio ring poisoned");
        ring.extend(frames.iter().copied());
        while ring.len() > self.capacity {
            ring.pop_front();
        }
    }
}

/// A handle to the running output stream and the queue feeding it. Dropping it
/// stops playback. The `cpal::Stream` is `!Send`, so keep this on the thread
/// that created it and hand the emulation thread a [`AudioSink`] via [`sink`].
///
/// [`sink`]: AudioPlayer::sink
pub struct AudioPlayer {
    _stream: cpal::Stream,
    sink: AudioSink,
}

impl AudioPlayer {
    /// Open the default output device at the 44100 Hz DAC rate (f32 stereo).
    /// Returns an error if there is no device or the format is unsupported, so
    /// the caller can keep running silently.
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("no default audio output device")?;
        let config = cpal::StreamConfig {
            channels: 2,
            sample_rate: cpal::SampleRate(DAC_HZ),
            buffer_size: cpal::BufferSize::Default,
        };

        let ring: Arc<Mutex<VecDeque<(i16, i16)>>> = Arc::new(Mutex::new(VecDeque::new()));
        let callback_ring = Arc::clone(&ring);
        let stream = device.build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let mut ring = callback_ring.lock().expect("audio ring poisoned");
                for frame in data.chunks_mut(2) {
                    let (left, right) = ring.pop_front().unwrap_or((0, 0)); // silence on underrun
                    frame[0] = f32::from(left) / 32768.0;
                    if let Some(slot) = frame.get_mut(1) {
                        *slot = f32::from(right) / 32768.0;
                    }
                }
            },
            |error| eprintln!("izarravm audio: output stream error: {error}"),
            None,
        )?;
        stream.play()?;

        Ok(Self {
            _stream: stream,
            sink: AudioSink {
                ring,
                capacity: DAC_HZ as usize / 2, // ~0.5 s of backlog
            },
        })
    }

    /// A `Send` handle to the playback queue for the emulation thread.
    pub fn sink(&self) -> AudioSink {
        self.sink.clone()
    }
}
