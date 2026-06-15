//! Band-limited resampler taking the OPL3's native 49716 Hz stereo output down
//! to the Resonique 2 DAC rate (44100 Hz). A Blackman-windowed sinc low-pass is
//! baked into a polyphase table, so each output frame is a 64-tap weighted sum
//! of the input stream, band-limited to the output Nyquist to avoid aliasing on
//! the downsample. The transition band here (22050..24858 Hz) is narrow, so a
//! fairly long kernel is needed to roll off cleanly within it.

/// Filter taps per output frame (32 each side of the interpolation point).
const TAPS: usize = 64;
const HALF: usize = TAPS / 2;
/// Quantised fractional positions baked into the polyphase table.
const PHASES: usize = 512;

fn sinc(x: f64) -> f64 {
    if x == 0.0 {
        1.0
    } else {
        let px = std::f64::consts::PI * x;
        px.sin() / px
    }
}

/// Symmetric Blackman window over `|t| <= half`, zero outside.
fn blackman(t: f64, half: f64) -> f64 {
    if t.abs() > half {
        return 0.0;
    }
    use std::f64::consts::PI;
    0.42 + 0.5 * (PI * t / half).cos() + 0.08 * (2.0 * PI * t / half).cos()
}

/// Streaming windowed-sinc resampler. Feed input frames with [`process`]; it
/// returns each output frame that has become available, keeping enough history
/// for the filter window across calls.
///
/// [`process`]: Resampler::process
#[derive(Debug, Clone)]
pub struct Resampler {
    step: f64,       // input frames consumed per output frame
    next: f64,       // absolute output position (input-frame units), monotonic
    consumed: usize, // input frames dropped from the front of `history`
    history: Vec<(i32, i32)>,
    table: Vec<f32>, // PHASES rows of TAPS weights
}

impl Resampler {
    pub fn new(input_hz: u32, output_hz: u32) -> Self {
        let step = f64::from(input_hz) / f64::from(output_hz);
        // Cutoff at the lower (output) Nyquist when downsampling, expressed as a
        // fraction of the input rate — the anti-alias band limit.
        let cutoff = (f64::from(output_hz) / f64::from(input_hz)).min(1.0);
        let mut table = vec![0f32; PHASES * TAPS];
        for p in 0..PHASES {
            let frac = p as f64 / PHASES as f64;
            let mut sum = 0.0;
            for j in 0..TAPS {
                // Offset of input tap j from the output point, in input frames.
                let t = frac + (HALF as f64 - 1.0) - j as f64;
                let h = sinc(cutoff * t) * blackman(t, HALF as f64);
                table[p * TAPS + j] = h as f32;
                sum += h;
            }
            // Normalise each row to unity gain so the level is rate-independent.
            for j in 0..TAPS {
                table[p * TAPS + j] = (f64::from(table[p * TAPS + j]) / sum) as f32;
            }
        }
        Self {
            step,
            next: HALF as f64,
            consumed: 0,
            history: vec![(0, 0); HALF], // left context for the first outputs
            table,
        }
    }

    /// Feed input frames; return every output frame that became available.
    pub fn process(&mut self, input: &[(i32, i32)]) -> Vec<(i32, i32)> {
        self.history.extend_from_slice(input);
        let available = self.consumed + self.history.len();
        let mut out = Vec::new();
        // Produce while the window [floor(next)-HALF+1 .. floor(next)+HALF] is in range.
        while self.next.floor() as usize + HALF < available {
            out.push(self.interpolate(self.next));
            self.next += self.step;
        }
        // Drop input before the next output's leftmost tap. `next` itself never
        // rewinds, so chunked and one-shot runs stay bit-identical.
        // ponytail: Vec::drain shifts ~TAPS elements; a ring buffer if it shows.
        let keep_from = (self.next.floor() as usize).saturating_sub(HALF - 1);
        let drop = keep_from.saturating_sub(self.consumed);
        if drop > 0 {
            self.history.drain(0..drop);
            self.consumed += drop;
        }
        out
    }

    fn interpolate(&self, pos: f64) -> (i32, i32) {
        let base = pos.floor() as usize; // absolute input-frame index
        let frac = pos - base as f64;
        let phase = ((frac * PHASES as f64) as usize).min(PHASES - 1);
        let row = &self.table[phase * TAPS..phase * TAPS + TAPS];
        let (mut left, mut right) = (0.0, 0.0);
        for (j, &w) in row.iter().enumerate() {
            let (l, r) = self.history[base + 1 + j - HALF - self.consumed];
            left += f64::from(l) * f64::from(w);
            right += f64::from(r) * f64::from(w);
        }
        (left.round() as i32, right.round() as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    const IN_HZ: f64 = 49_716.0;
    const OUT_HZ: f64 = 44_100.0;

    // A one-second stereo sine at `freq` (input rate), amplitude `amp`.
    fn sine(freq: f64, amp: f64) -> Vec<(i32, i32)> {
        (0..IN_HZ as usize)
            .map(|n| {
                let s = (amp * (2.0 * PI * freq * n as f64 / IN_HZ).sin()).round() as i32;
                (s, s)
            })
            .collect()
    }

    fn resample(freq: f64, amp: f64) -> Vec<(i32, i32)> {
        Resampler::new(49716, 44100).process(&sine(freq, amp))
    }

    fn steady_peak(out: &[(i32, i32)]) -> i32 {
        out[1000..out.len() - 1000]
            .iter()
            .map(|f| f.0.abs())
            .max()
            .unwrap()
    }

    #[test]
    fn produces_the_target_output_rate() {
        let out = Resampler::new(49716, 44100).process(&vec![(0, 0); 49716]);
        let expected = OUT_HZ as i32;
        assert!(
            (out.len() as i32 - expected).abs() < 50,
            "got {} frames, expected ~{expected}",
            out.len()
        );
    }

    #[test]
    fn preserves_a_passband_tone() {
        let amp = 10_000.0;
        let out = resample(1000.0, amp);
        // Amplitude is preserved (unity gain).
        let peak = steady_peak(&out);
        assert!(
            (f64::from(peak) - amp).abs() < amp * 0.05,
            "peak {peak} vs {amp}"
        );
        // Frequency is unchanged at the new rate.
        let mid = &out[1000..out.len() - 1000];
        let crossings = mid.windows(2).filter(|w| w[0].0 <= 0 && w[1].0 > 0).count();
        let measured = crossings as f64 * OUT_HZ / mid.len() as f64;
        assert!((measured - 1000.0).abs() < 5.0, "measured {measured} Hz");
    }

    #[test]
    fn attenuates_content_above_the_output_nyquist() {
        // 24500 Hz is below the input Nyquist (24858) but well above the output
        // Nyquist (22050); without band-limiting it would alias to ~19600 Hz.
        let amp = 10_000.0;
        let passband = f64::from(steady_peak(&resample(1000.0, amp)));
        let stopband = f64::from(steady_peak(&resample(24500.0, amp)));
        assert!(
            stopband < passband * 0.1,
            "above-Nyquist tone should be filtered: stop {stopband} vs pass {passband}"
        );
    }

    #[test]
    fn streaming_matches_single_shot() {
        let input = sine(1000.0, 10_000.0);
        let whole = Resampler::new(49716, 44100).process(&input);
        let mut split_rs = Resampler::new(49716, 44100);
        let mut split = split_rs.process(&input[..20_000]);
        split.extend(split_rs.process(&input[20_000..]));
        assert_eq!(whole, split, "chunked input must match one-shot");
    }
}
