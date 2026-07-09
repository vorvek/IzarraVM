//! Host audio output: plays queued 44100 Hz stereo PCM through the default
//! device. The emulator pushes resampled OPL frames with [`AudioSink::queue`]
//! and the cpal callback drains them, resampling to the device's own rate. This
//! is device glue, it can only be exercised by actually running on a machine
//! with an output device, so it is kept small and free of synthesis logic.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// The rate the emulator renders PCM at. The output stream resamples from this
/// to whatever the host device asks for.
const SOURCE_HZ: u32 = 44_100;

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
    /// Open the default output device at its own preferred config. Returns an
    /// error if there is no device or the format is unsupported, so the caller
    /// can keep running silently.
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("no default audio output device")?;
        // Use the device's own default config rather than a fixed 44.1 kHz/f32
        // request: in WASAPI shared mode only the device mix format is accepted,
        // so the fixed request failed (silently, no sound) on the many Windows
        // devices that mix at 48 kHz. The callback resamples our 44.1 kHz frames
        // to whatever rate and channel count the device reports.
        let supported = device.default_output_config()?;
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();

        let ring: Arc<Mutex<VecDeque<(i16, i16)>>> = Arc::new(Mutex::new(VecDeque::new()));
        let stream = match sample_format {
            cpal::SampleFormat::F32 => build_stream::<f32>(&device, &config, Arc::clone(&ring)),
            cpal::SampleFormat::I16 => build_stream::<i16>(&device, &config, Arc::clone(&ring)),
            cpal::SampleFormat::U16 => build_stream::<u16>(&device, &config, Arc::clone(&ring)),
            other => return Err(format!("unsupported audio sample format: {other:?}").into()),
        }?;
        stream.play()?;

        Ok(Self {
            _stream: stream,
            sink: AudioSink {
                ring,
                capacity: SOURCE_HZ as usize, // ~1 s of backlog (larger ring for HLE drift tolerance)
            },
        })
    }

    /// A `Send` handle to the playback queue for the emulation thread.
    pub fn sink(&self) -> AudioSink {
        self.sink.clone()
    }
}

/// Build an output stream that pulls 44.1 kHz stereo i16 frames from `ring`,
/// sample-and-hold resamples them to the device rate, and writes them as `T`
/// across the device's channels. Sample-and-hold is crude but ample for a
/// beeper and an OPL chime, and keeps the conversion to a few lines.
fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    ring: Arc<Mutex<VecDeque<(i16, i16)>>>,
) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let out_hz = config.sample_rate.0 as i64;
    // Two straddling source frames (`cur`..`nxt`) plus a fractional read position
    // between them. Interpolating linearly between the two is far smoother than
    // the old zero-order hold, whose stair-stepping injected audible high-
    // frequency imaging noise ("scratch") when up-sampling 44.1 kHz to a 48 kHz
    // device. On underrun `nxt` holds its last value, so a starved ring fades to
    // a held level rather than clicking.
    let mut cur = (0i16, 0i16);
    let mut nxt = (0i16, 0i16);
    let mut phase: i64 = 0; // read position between cur and nxt, scaled by out_hz
    // Opt-in underrun diagnostic (IZARRAVM_AUDIO_DEBUG): counts source frames the
    // ring could not supply (the emulation produced audio slower than the device
    // consumed it) and logs a rate once per output second. A high rate is the
    // signature of the crackle when a guest runs below real time. Off = one bool.
    let debug = std::env::var_os("IZARRAVM_AUDIO_DEBUG").is_some();
    let mut underruns: u64 = 0;
    let mut out_frames: u64 = 0;
    device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            let mut ring = ring.lock().expect("audio ring poisoned");
            for frame in data.chunks_mut(channels) {
                // Linear interpolation: `phase/out_hz` is how far the output time
                // sits between `cur` and `nxt`.
                let frac = phase as f32 / out_hz as f32;
                let li = f32::from(cur.0) + (f32::from(nxt.0) - f32::from(cur.0)) * frac;
                let ri = f32::from(cur.1) + (f32::from(nxt.1) - f32::from(cur.1)) * frac;
                let l = T::from_sample(li / 32768.0);
                let r = T::from_sample(ri / 32768.0);
                if let Some(s) = frame.get_mut(0) {
                    *s = l;
                }
                if let Some(s) = frame.get_mut(1) {
                    *s = r;
                }
                for s in frame.iter_mut().skip(2) {
                    *s = T::from_sample(0.0);
                }
                // Advance the read position by SOURCE_HZ/out_hz per output frame,
                // stepping cur<-nxt<-ring each time it crosses a source frame.
                phase += SOURCE_HZ as i64;
                while phase >= out_hz {
                    phase -= out_hz;
                    cur = nxt;
                    match ring.pop_front() {
                        Some(f) => nxt = f,
                        None => underruns += 1, // hold last (nxt) on underrun
                    }
                }
            }
            if debug {
                out_frames += (data.len() / channels.max(1)) as u64;
                if out_frames >= out_hz as u64 {
                    eprintln!(
                        "[AUDIO] underruns/s={underruns} ring_now={} (0 = keeping up)",
                        ring.len()
                    );
                    underruns = 0;
                    out_frames = 0;
                }
            }
        },
        |error| eprintln!("izarravm audio: output stream error: {error}"),
        None,
    )
}
