// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Band-limited resampler taking the OPL3's native 49716 Hz stereo output down
//! to the ReSonique II DAC rate (44100 Hz). A Blackman-windowed sinc low-pass is
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
        Self {
            step: f64::from(input_hz) / f64::from(output_hz),
            next: HALF as f64,
            consumed: 0,
            history: vec![(0, 0); HALF], // left context for the first outputs
            table: build_table(input_hz, output_hz),
        }
    }

    /// Feed input frames; return every output frame that became available.
    pub fn process(&mut self, input: &[(i32, i32)]) -> Vec<(i32, i32)> {
        let mut out = Vec::new();
        self.process_into(input, &mut out);
        out
    }

    /// Like [`process`](Resampler::process), but appends to `out`, so a caller
    /// can reuse one buffer.
    pub fn process_into(&mut self, input: &[(i32, i32)], out: &mut Vec<(i32, i32)>) {
        self.history.extend_from_slice(input);
        let available = self.consumed + self.history.len();
        // Produce while the window [floor(next)-HALF+1 .. floor(next)+HALF] is in range.
        while self.next.floor() as usize + HALF < available {
            out.push(self.interpolate(self.next));
            self.next += self.step;
        }
        // Drop input before the next output's leftmost tap. `next` itself never
        // rewinds, so chunked and one-shot runs stay bit-identical.
        // Vec::drain shifts ~TAPS elements; switch to a ring buffer if it shows.
        let keep_from = (self.next.floor() as usize).saturating_sub(HALF - 1);
        let drop = keep_from.saturating_sub(self.consumed);
        if drop > 0 {
            self.history.drain(0..drop);
            self.consumed += drop;
        }
    }

    fn interpolate(&self, pos: f64) -> (i32, i32) {
        let (base, row) = phase_row(&self.table, pos);
        let first = base + 1 - HALF - self.consumed;
        weighted_sum(row, self.history[first..first + TAPS].iter().copied())
    }
}

/// The polyphase table for one rate pair: `PHASES` rows of `TAPS` weights.
fn build_table(input_hz: u32, output_hz: u32) -> Vec<f32> {
    // Cutoff at the lower (output) Nyquist when downsampling, expressed as a
    // fraction of the input rate, the anti-alias band limit.
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
    table
}

/// The absolute input-frame index at `pos` and the table row for its phase.
fn phase_row(table: &[f32], pos: f64) -> (usize, &[f32]) {
    let base = pos.floor() as usize;
    let frac = pos - base as f64;
    let phase = ((frac * PHASES as f64) as usize).min(PHASES - 1);
    (base, &table[phase * TAPS..phase * TAPS + TAPS])
}

/// One output frame: the `TAPS` input frames weighted by one table row.
fn weighted_sum(row: &[f32], frames: impl Iterator<Item = (i32, i32)>) -> (i32, i32) {
    let (mut left, mut right) = (0.0, 0.0);
    for (&w, (l, r)) in row.iter().zip(frames) {
        left += f64::from(l) * f64::from(w);
        right += f64::from(r) * f64::from(w);
    }
    (left.round() as i32, right.round() as i32)
}

/// The same filter as [`Resampler`], one frame at a time and without
/// allocation after `new`. Push input frames and pull output frames: the history
/// is a fixed ring of `TAPS` frames. For a given rate pair it returns exactly the
/// frames `Resampler::process` returns.
#[derive(Debug, Clone)]
pub struct StreamResampler {
    step: f64,
    next: f64,
    pushed: usize, // input frames pushed so far, counting the zero lead-in
    ring: [(i32, i32); TAPS],
    table: Vec<f32>,
}

impl StreamResampler {
    pub fn new(input_hz: u32, output_hz: u32) -> Self {
        Self {
            step: f64::from(input_hz) / f64::from(output_hz),
            next: HALF as f64,
            pushed: HALF, // the zero left context, as in `Resampler`
            ring: [(0, 0); TAPS],
            table: build_table(input_hz, output_hz),
        }
    }

    /// True when the next output frame has all its input frames.
    pub fn ready(&self) -> bool {
        (self.next.floor() as usize) + HALF < self.pushed
    }

    pub fn push(&mut self, frame: (i32, i32)) {
        self.ring[self.pushed % TAPS] = frame;
        self.pushed += 1;
    }

    /// The next output frame, or `None` until `ready`.
    pub fn pull(&mut self) -> Option<(i32, i32)> {
        if !self.ready() {
            return None;
        }
        let (base, row) = phase_row(&self.table, self.next);
        let first = base + 1 - HALF;
        let out = weighted_sum(row, (0..TAPS).map(|j| self.ring[(first + j) % TAPS]));
        self.next += self.step;
        Some(out)
    }
}

#[cfg(test)]
#[path = "resample_test.rs"]
mod tests;
