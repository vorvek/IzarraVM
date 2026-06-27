//! The motherboard PC speaker: a 1-bit beeper driven by PIT channel-2 OUT ANDed
//! with port 0x61 bit 1 (data enable). This converts that 1-bit membrane over
//! emulated time into samples at the DAC rate for the audio mixer to sum.

use std::collections::VecDeque;

/// Output level for an enabled membrane. Audible, with headroom against the OPL
/// and DSP sums. Bipolar so a toggling square wave carries no DC bias.
const SPEAKER_AMPLITUDE: f64 = 8000.0;

/// The host DAC rate the ring is produced at.
const DAC_HZ: u32 = 44_100;
const SAMPLE_SECONDS: f64 = 1.0 / DAC_HZ as f64;

// Voicing of the physical cone. A real case-mounted PC speaker is not flat: it
// has a low-mid body resonance that gives it warmth and presence, and a soft top
// rather than a tiny beeper's tinny buzz. Two biquads model that: a gentle
// peaking boost for the body, then a 2nd-order low-pass for the top. Tuned by ear.
const BODY_HZ: f64 = 550.0; // cone body resonance, for warmth and presence
const BODY_Q: f64 = 0.9; // focused enough to read as body, not a broad mud boost
const BODY_GAIN_DB: f64 = 3.0;
const TOP_HZ: f64 = 5000.0; // soft top rolloff, to tame the tinny buzz
const TOP_Q: f64 = 0.6; // below Butterworth: a gentle early rolloff, no harsh knee

// "Inside the case" ambience. The speaker sits in a small desktop case (Amiga
// 3000 / horizontal ATX size), so its sound carries short early reflections with
// a faint metallic ring. A handful of single-digit-millisecond comb delays plus
// one diffusing allpass model that; modest feedback keeps the decay short like a
// component-packed case rather than a room, and low wet keeps it a hint, not a
// hall. Tuned by ear.
const BOX_FEEDBACK: f64 = 0.4; // short decay: a small case, not a room
const BOX_WET: f64 = 0.15; // how much of the box ambience to blend in

/// A direct-form-I biquad. Shapes the box-averaged square into something with a
/// cone's body instead of a sterile beep. Coefficients use the RBJ cookbook.
#[derive(Debug, Clone, Default)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl Biquad {
    /// Peaking EQ: boost a band around `f0` by `db_gain`, roughly flat elsewhere.
    fn peaking(fs: f64, f0: f64, q: f64, db_gain: f64) -> Self {
        let a = 10f64.powf(db_gain / 40.0);
        let w0 = 2.0 * std::f64::consts::PI * f0 / fs;
        let (sin, cos) = (w0.sin(), w0.cos());
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha / a;
        Self {
            b0: (1.0 + alpha * a) / a0,
            b1: (-2.0 * cos) / a0,
            b2: (1.0 - alpha * a) / a0,
            a1: (-2.0 * cos) / a0,
            a2: (1.0 - alpha / a) / a0,
            ..Default::default()
        }
    }

    /// 2nd-order low-pass at corner `fc`.
    fn low_pass(fs: f64, fc: f64, q: f64) -> Self {
        let w0 = 2.0 * std::f64::consts::PI * fc / fs;
        let (sin, cos) = (w0.sin(), w0.cos());
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: ((1.0 - cos) / 2.0) / a0,
            b1: (1.0 - cos) / a0,
            b2: ((1.0 - cos) / 2.0) / a0,
            a1: (-2.0 * cos) / a0,
            a2: (1.0 - alpha) / a0,
            ..Default::default()
        }
    }

    fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// A feedback comb: a short delay line fed back on itself. Short delays with
/// modest feedback give the boxy, faintly metallic resonance of a small case.
#[derive(Debug, Clone)]
struct Comb {
    buf: Vec<f64>,
    pos: usize,
    feedback: f64,
}

impl Comb {
    fn new(delay: usize, feedback: f64) -> Self {
        Self {
            buf: vec![0.0; delay.max(1)],
            pos: 0,
            feedback,
        }
    }

    fn process(&mut self, x: f64) -> f64 {
        let y = self.buf[self.pos];
        self.buf[self.pos] = x + y * self.feedback;
        self.pos = (self.pos + 1) % self.buf.len();
        y
    }
}

/// A Schroeder allpass: diffuses the comb resonances so the box reads as
/// ambience rather than a single pitched ring.
#[derive(Debug, Clone)]
struct Allpass {
    buf: Vec<f64>,
    pos: usize,
    gain: f64,
}

impl Allpass {
    fn new(delay: usize, gain: f64) -> Self {
        Self {
            buf: vec![0.0; delay.max(1)],
            pos: 0,
            gain,
        }
    }

    fn process(&mut self, x: f64) -> f64 {
        let buffered = self.buf[self.pos];
        let y = buffered - x;
        self.buf[self.pos] = x + buffered * self.gain;
        self.pos = (self.pos + 1) % self.buf.len();
        y
    }
}

/// The case as an acoustic box: a few short, mutually detuned combs plus one
/// allpass, blended in dry+wet. Models the early reflections inside the plastic
/// and aluminium enclosure without a full reverb tail.
#[derive(Debug, Clone)]
struct BoxReverb {
    combs: Vec<Comb>,
    allpass: Allpass,
    wet: f64,
}

impl BoxReverb {
    fn new(fs: f64) -> Self {
        // Convert milliseconds to whole samples. The delays are small and not
        // simple ratios of each other, so the comb peaks do not pile onto one
        // pitch (which would sound like a single resonant tone, not a box).
        let ms = |m: f64| ((fs * m / 1000.0) as usize).max(1);
        Self {
            combs: vec![
                Comb::new(ms(3.0), BOX_FEEDBACK),
                Comb::new(ms(4.1), BOX_FEEDBACK),
                Comb::new(ms(5.3), BOX_FEEDBACK),
                Comb::new(ms(6.2), BOX_FEEDBACK),
            ],
            allpass: Allpass::new(ms(1.6), 0.5),
            wet: BOX_WET,
        }
    }

    fn process(&mut self, x: f64) -> f64 {
        let mut sum = 0.0;
        for comb in &mut self.combs {
            sum += comb.process(x);
        }
        sum /= self.combs.len() as f64;
        let diffused = self.allpass.process(sum);
        x + self.wet * diffused
    }
}

/// Cap the ring so a headless run (which never drains) cannot grow it without
/// bound. The GUI drains every frame, so it never reaches this.
const RING_CAP: usize = 2 * DAC_HZ as usize;

#[derive(Debug, Clone)]
pub(crate) struct Speaker {
    data_enable: bool,   // port 0x61 bit 1
    control_bits: u8,    // low bits last written to 0x61, for readback (bits 0,1)
    ch2_out: bool,       // current PIT channel-2 OUT level
    sample_elapsed: f64, // seconds accumulated into the current DAC sample
    sample_area: f64,    // target-level integral for the current DAC sample
    body: Biquad,        // cone body resonance (low-mid warmth)
    top: Biquad,         // top rolloff (tames the tinny buzz)
    case: BoxReverb,     // "inside the case" early-reflection ambience
    ring: VecDeque<i16>, // produced mono samples awaiting drain
    ever_enabled: bool,  // sticky: set the first time data enable goes high
}

impl Default for Speaker {
    fn default() -> Self {
        let fs = DAC_HZ as f64;
        Self {
            data_enable: false,
            control_bits: 0,
            ch2_out: false,
            sample_elapsed: 0.0,
            sample_area: 0.0,
            body: Biquad::peaking(fs, BODY_HZ, BODY_Q, BODY_GAIN_DB),
            top: Biquad::low_pass(fs, TOP_HZ, TOP_Q),
            case: BoxReverb::new(fs),
            ring: VecDeque::new(),
            ever_enabled: false,
        }
    }
}

impl Speaker {
    /// Apply a write to port 0x61: bit 0 is GATE2 (the caller drives the PIT
    /// gate), bit 1 is the speaker data enable. Other bits are ignored.
    pub(crate) fn write_control(&mut self, value: u8) {
        self.control_bits = value & 0x03;
        self.data_enable = value & 0x02 != 0;
        if self.data_enable {
            self.ever_enabled = true;
        }
    }

    /// Whether the speaker data enable was ever set high. The power-on chime drives
    /// this during POST, so a headless run can confirm the chime played without
    /// draining the audio ring.
    pub(crate) fn ever_enabled(&self) -> bool {
        self.ever_enabled
    }

    /// The low two bits last written to 0x61 (GATE2 and data enable), for readback.
    pub(crate) fn control_bits(&self) -> u8 {
        self.control_bits
    }

    /// Advance emulated time, integrating PIT channel-2 transitions at their
    /// sub-sample times before shaping the result through the cone voicing.
    pub(crate) fn accumulate<I>(&mut self, seconds: f64, initial_ch2_out: bool, transitions: I)
    where
        I: IntoIterator<Item = (f64, bool)>,
    {
        if seconds <= 0.0 {
            return;
        }

        self.ch2_out = initial_ch2_out;
        let mut cursor = 0.0;
        for (at, level) in transitions {
            let at = at.clamp(0.0, seconds);
            if at > cursor {
                self.advance_segment(at - cursor);
                cursor = at;
            }
            self.ch2_out = level;
        }

        if seconds > cursor {
            self.advance_segment(seconds - cursor);
        }
    }

    fn target_level(&self) -> f64 {
        if !self.data_enable {
            0.0
        } else if self.ch2_out {
            SPEAKER_AMPLITUDE
        } else {
            -SPEAKER_AMPLITUDE
        }
    }

    fn advance_segment(&mut self, mut seconds: f64) {
        while seconds > f64::EPSILON {
            let remaining = SAMPLE_SECONDS - self.sample_elapsed;
            let step = seconds.min(remaining);
            self.sample_area += self.target_level() * step;
            self.sample_elapsed += step;
            seconds -= step;

            if self.sample_elapsed + f64::EPSILON >= SAMPLE_SECONDS {
                self.emit_sample();
                self.sample_elapsed = 0.0;
                self.sample_area = 0.0;
            }
        }
    }

    fn emit_sample(&mut self) {
        let avg_target = self.sample_area / SAMPLE_SECONDS;
        // Shape the box-averaged square through the cone voicing (low-mid body
        // resonance, then top rolloff), then place it inside the case.
        let shaped = self.top.process(self.body.process(avg_target));
        let boxed = self.case.process(shaped);
        let sample = boxed.round().clamp(i16::MIN as f64, i16::MAX as f64) as i16;
        self.ring.push_back(sample);
        if self.ring.len() > RING_CAP {
            self.ring.pop_front();
        }
    }

    /// Drain up to `n` produced samples, padding with 0 on underrun so the mixer
    /// always gets a full window.
    pub(crate) fn drain(&mut self, n: usize) -> Vec<i16> {
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(self.ring.pop_front().unwrap_or(0));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_membrane_toggles_with_ch2_out() {
        let mut spk = Speaker::default();
        spk.write_control(0x03); // gate + data enable
        spk.accumulate(0.001, true, std::iter::empty::<(f64, bool)>()); // ~44 samples high
        spk.accumulate(0.001, false, std::iter::empty::<(f64, bool)>()); // ~44 samples low
        let s = spk.drain(88);
        assert!(s.iter().any(|&v| v > 0), "high half produced +AMP");
        assert!(s.iter().any(|&v| v < 0), "low half produced -AMP");
    }

    #[test]
    fn disabled_speaker_is_silent() {
        let mut spk = Speaker::default(); // data_enable false
        spk.accumulate(0.01, true, std::iter::empty::<(f64, bool)>()); // OUT high but disabled
        assert!(spk.drain(100).iter().all(|&v| v == 0));
    }

    #[test]
    fn drain_pads_with_zero_on_underrun() {
        let mut spk = Speaker::default();
        spk.write_control(0x03);
        spk.accumulate(0.0001, true, std::iter::empty::<(f64, bool)>()); // ~4 samples
        let s = spk.drain(50);
        assert_eq!(s.len(), 50);
        assert!(s[40..].iter().all(|&v| v == 0));
    }

    #[test]
    fn sub_sample_pulse_width_changes_the_sample() {
        let mut short = Speaker::default();
        short.write_control(0x03);
        short.accumulate(
            SAMPLE_SECONDS,
            false,
            [
                (SAMPLE_SECONDS * 0.25, true),
                (SAMPLE_SECONDS * 0.50, false),
            ],
        );

        let mut long = Speaker::default();
        long.write_control(0x03);
        long.accumulate(SAMPLE_SECONDS, false, [(SAMPLE_SECONDS * 0.25, true)]);

        let short = short.drain(1)[0];
        let long = long.drain(1)[0];
        assert!(
            short < 0,
            "short high pulse should average low, got {short}"
        );
        assert!(long > 0, "long high pulse should average high, got {long}");
        assert!(long > short, "pulse width must affect the rendered sample");
    }

    #[test]
    fn ever_enabled_latches_on_first_enable() {
        let mut spk = Speaker::default();
        assert!(!spk.ever_enabled());
        spk.write_control(0x01); // gate only, data enable off
        assert!(!spk.ever_enabled());
        spk.write_control(0x03); // data enable on
        assert!(spk.ever_enabled());
        spk.write_control(0x00); // off again, but the latch stays set
        assert!(spk.ever_enabled());
    }
}
