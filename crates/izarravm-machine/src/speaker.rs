//! The motherboard PC speaker: a 1-bit beeper driven by PIT channel-2 OUT ANDed
//! with port 0x61 bit 1 (data enable). This converts that 1-bit membrane over
//! emulated time into samples at the DAC rate for the audio mixer to sum.

use std::collections::VecDeque;

/// Output level for an enabled, high membrane. Audible, with headroom against the
/// OPL and DSP sums. Bipolar so a toggling square wave carries no DC bias.
const SPEAKER_AMPLITUDE: i16 = 8000;

/// The host DAC rate the ring is produced at.
const DAC_HZ: u32 = 44_100;

/// Cap the ring so a headless run (which never drains) cannot grow it without
/// bound. The GUI drains every frame, so it never reaches this.
const RING_CAP: usize = 2 * DAC_HZ as usize;

#[derive(Debug, Clone, Default)]
pub(crate) struct Speaker {
    data_enable: bool,   // port 0x61 bit 1
    control_bits: u8,    // low bits last written to 0x61, for readback (bits 0,1)
    sample_phase: f64,   // fractional DAC samples owed
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

    /// Advance emulated time by `clocks` CPU clocks (with `inv_clock` = 1/clock_hz
    /// from the active mode), sampling the membrane into the ring at the DAC rate.
    pub(crate) fn accumulate(&mut self, clocks: u64, inv_clock: f64, ch2_out: bool) {
        if inv_clock <= 0.0 {
            return;
        }
        let seconds = clocks as f64 * inv_clock;
        self.sample_phase += seconds * DAC_HZ as f64;
        let level = if self.data_enable {
            if ch2_out {
                SPEAKER_AMPLITUDE
            } else {
                -SPEAKER_AMPLITUDE
            }
        } else {
            0
        };
        while self.sample_phase >= 1.0 {
            self.sample_phase -= 1.0;
            self.ring.push_back(level);
        }
        while self.ring.len() > RING_CAP {
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
        spk.accumulate(1_000, 1.0 / 1_000_000.0, true); // ~44 samples high
        spk.accumulate(1_000, 1.0 / 1_000_000.0, false); // ~44 samples low
        let s = spk.drain(88);
        assert!(s.iter().any(|&v| v > 0), "high half produced +AMP");
        assert!(s.iter().any(|&v| v < 0), "low half produced -AMP");
    }

    #[test]
    fn disabled_speaker_is_silent() {
        let mut spk = Speaker::default(); // data_enable false
        spk.accumulate(10_000, 1.0 / 1_000_000.0, true); // OUT high but disabled
        assert!(spk.drain(100).iter().all(|&v| v == 0));
    }

    #[test]
    fn drain_pads_with_zero_on_underrun() {
        let mut spk = Speaker::default();
        spk.write_control(0x03);
        spk.accumulate(100, 1.0 / 1_000_000.0, true); // ~4 samples
        let s = spk.drain(50);
        assert_eq!(s.len(), 50);
        assert!(s[40..].iter().all(|&v| v == 0));
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
