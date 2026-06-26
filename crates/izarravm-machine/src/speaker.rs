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
/// A small cone response keeps PIT/PWM edges from aliasing as hard digital steps.
const CONE_RESPONSE_SECONDS: f64 = 60e-6;

/// Cap the ring so a headless run (which never drains) cannot grow it without
/// bound. The GUI drains every frame, so it never reaches this.
const RING_CAP: usize = 2 * DAC_HZ as usize;

#[derive(Debug, Clone, Default)]
pub(crate) struct Speaker {
    data_enable: bool,   // port 0x61 bit 1
    control_bits: u8,    // low bits last written to 0x61, for readback (bits 0,1)
    ch2_out: bool,       // current PIT channel-2 OUT level
    sample_elapsed: f64, // seconds accumulated into the current DAC sample
    sample_area: f64,    // target-level integral for the current DAC sample
    cone: f64,           // filtered membrane position
    ring: VecDeque<i16>, // produced mono samples awaiting drain
    ever_enabled: bool,  // sticky: set the first time data enable goes high
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
    /// sub-sample times before applying a simple cone response.
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
        let alpha = 1.0 - (-SAMPLE_SECONDS / CONE_RESPONSE_SECONDS).exp();
        self.cone += (avg_target - self.cone) * alpha;
        let sample = self.cone.round().clamp(i16::MIN as f64, i16::MAX as f64) as i16;
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
