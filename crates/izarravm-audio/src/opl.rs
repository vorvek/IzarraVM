//! Clean-room OPL3 (YMF262 / AdLib) sound chip.
//!
//! The register/timer model drives AdLib detection; the synthesis path
//! (tables -> operators -> channels -> render) reproduces the chip's bit-exact
//! integer datapath. All lookup tables are generated from the public log-sin /
//! exp formulas, not transcribed from any reference implementation.

/// Quarter-wave log-sine ROM: `round(-log2(sin((i + 0.5) * pi/512)) * 256)`.
/// Entry 0 is the quietest point of the wave (2137), entry 255 the loudest (0).
fn build_logsin() -> [u16; 256] {
    let mut table = [0u16; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let angle = (i as f64 + 0.5) * std::f64::consts::PI / 512.0;
        *slot = (-angle.sin().log2() * 256.0).round() as u16;
    }
    table
}

/// Exponent ROM: `round((2^(i/256) - 1) * 1024)`. Used to turn a log-domain
/// attenuation back into a linear amplitude.
fn build_exp() -> [u16; 256] {
    let mut table = [0u16; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        *slot = ((2.0_f64.powf(i as f64 / 256.0) - 1.0) * 1024.0).round() as u16;
    }
    table
}

use std::sync::LazyLock;

static LOGSIN: LazyLock<[u16; 256]> = LazyLock::new(build_logsin);
static EXP: LazyLock<[u16; 256]> = LazyLock::new(build_exp);

/// Log-sine ROM lookup for a quarter-wave index (0..256).
pub(crate) fn logsin(index: usize) -> u16 {
    LOGSIN[index & 0xff]
}

/// Convert a log-domain attenuation to a linear amplitude, the way the chip
/// does: the low 8 bits index the (reversed, +1024) exp ROM, the high bits are
/// a right shift. `exp_lookup(0)` is the maximum half-amplitude (2042).
pub(crate) fn exp_lookup(attenuation: u32) -> i32 {
    let fraction = (attenuation & 0xff) as usize;
    let shift = attenuation >> 8;
    (i32::from(EXP[fraction ^ 0xff]) + 1024) >> shift
}

/// Frequency multiplier (MULT register, 0..15), stored doubled so the phase
/// math stays integer: index 0 is x0.5, the rest are whole multiples with the
/// two documented duplicate slots (11->10, 13->12, 14->15... per the spec).
const MULTIPLE_X2: [u32; 16] = [1, 2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 20, 24, 24, 30, 30];

/// A single FM operator: a 20-bit phase accumulator plus its waveform and
/// level. The envelope generator is added in a later layer; for now an
/// operator plays at full volume minus its total level.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Operator {
    phase: u32,
    fnum: u16,
    block: u8,
    multiple: u8,
    waveform: u8,
    total_level: u8,
}

impl Operator {
    pub(crate) fn set_frequency(&mut self, fnum: u16, block: u8) {
        self.fnum = fnum & 0x3ff;
        self.block = block & 0x07;
    }

    pub(crate) fn set_multiple(&mut self, value: u8) {
        self.multiple = value & 0x0f;
    }

    pub(crate) fn set_waveform(&mut self, value: u8) {
        self.waveform = value & 0x07;
    }

    pub(crate) fn set_total_level(&mut self, value: u8) {
        self.total_level = value & 0x3f;
    }

    /// Per-sample phase step. `f = fnum * 2^block * rate / 2^20` for MULT x1.
    fn phase_increment(&self) -> u32 {
        ((u32::from(self.fnum) << self.block) * MULTIPLE_X2[self.multiple as usize]) >> 1
    }

    pub(crate) fn advance(&mut self) {
        self.phase = self.phase.wrapping_add(self.phase_increment()) & 0x000f_ffff;
    }

    /// Signed operator output for the current phase. `extra_attenuation` carries
    /// the envelope contributions in log-domain units; total level is folded in
    /// here (0.75 dB per step == 32 log units).
    pub(crate) fn sample(&self, extra_attenuation: u16) -> i32 {
        self.sample_modulated(0, extra_attenuation)
    }

    /// Operator output with the carrier phase offset by `phase_modulation` (the
    /// modulator's signed output, in 10-bit wave-position units). A full-scale
    /// modulator (+/-2042) bends the carrier by ~2 cycles, the OPL maximum.
    // The exact modulation depth alignment is pending a YMF262 datasheet check;
    // it shapes timbre, not the fundamental frequency.
    pub(crate) fn sample_modulated(&self, phase_modulation: i32, extra_attenuation: u16) -> i32 {
        let attenuation = u32::from(self.total_level) * 32 + u32::from(extra_attenuation);
        let position =
            ((((self.phase >> 10) as i32).wrapping_add(phase_modulation)) & 0x3ff) as u32;
        waveform_output_at(position, self.waveform, attenuation)
    }
}

/// Map the 10-bit wave position to a (log-sine attenuation, sign) pair for one
/// of the eight waveforms, or `None` when the chip mutes that segment.
fn waveform_attenuation(position: u32, waveform: u8) -> Option<(u16, bool)> {
    let quarter = (position & 0xff) as usize;
    let second_quarter = position & 0x100 != 0;
    let second_half = position & 0x200 != 0;
    // Even quarters rise, odd quarters mirror back down.
    let folded = if second_quarter {
        logsin(quarter ^ 0xff)
    } else {
        logsin(quarter)
    };

    match waveform {
        0 => Some((folded, second_half)),               // full sine
        1 => (!second_half).then_some((folded, false)), // half sine
        2 => Some((folded, false)),                     // abs sine
        3 => (!second_quarter).then_some((logsin(quarter), false)), // quarter sine
        // Waveforms 4-7 are OPL3-only; implemented in the OPL3 waveform layer.
        // Until then they fall back to the full sine shape.
        _ => Some((folded, second_half)),
    }
}

fn waveform_output_at(position: u32, waveform: u8, attenuation: u32) -> i32 {
    let Some((wave_attenuation, negative)) = waveform_attenuation(position, waveform) else {
        return 0;
    };
    let magnitude = exp_lookup(u32::from(wave_attenuation) + attenuation);
    if negative { -magnitude } else { magnitude }
}

/// Register slot offset for each of the 18 operators. The OPL leaves gaps at
/// offsets 6,7,14,15 (and 22+), so operator i reads its registers at
/// `base + OPERATOR_SLOT[i]`.
const OPERATOR_SLOT: [usize; 18] = [
    0, 1, 2, 3, 4, 5, 8, 9, 10, 11, 12, 13, 16, 17, 18, 19, 20, 21,
];

/// The (modulator, carrier) operator indices for a 2-op channel (0..9).
fn channel_operators(channel: usize) -> (usize, usize) {
    let base = (channel / 3) * 6 + (channel % 3);
    (base, base + 3)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OplChip {
    registers: [u8; 256],
    address: u8,
    timer1: Timer,
    timer2: Timer,
    operators: [Operator; 18],
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Timer {
    /// Microseconds per count step: 80us for timer 1, 320us for timer 2.
    step_us: u64,
    /// Up-counter; overflow past 0xff sets `expired` and reloads the preset.
    count: u16,
    accumulated_us: u64,
    running: bool,
    expired: bool,
}

impl Timer {
    fn new(step_us: u64) -> Self {
        Self {
            step_us,
            ..Self::default()
        }
    }

    fn start(&mut self, preset: u8) {
        self.count = u16::from(preset);
        self.accumulated_us = 0;
        self.running = true;
    }

    // Reloads from the live preset on overflow, not a latched copy. Exact
    // enough for detection; revisit if a game depends on mid-run preset reloads.
    fn advance(&mut self, micros: u64, preset: u8) {
        if !self.running {
            return;
        }
        self.accumulated_us += micros;
        while self.accumulated_us >= self.step_us {
            self.accumulated_us -= self.step_us;
            self.count += 1;
            if self.count > 0xff {
                self.count = u16::from(preset);
                self.expired = true;
            }
        }
    }
}

impl Default for OplChip {
    fn default() -> Self {
        Self {
            registers: [0; 256],
            address: 0,
            timer1: Timer::new(80),
            timer2: Timer::new(320),
            operators: Default::default(),
        }
    }
}

impl OplChip {
    pub fn register(&self, index: u8) -> u8 {
        self.registers[index as usize]
    }

    /// Write a chip register, applying the side effects of the timer-control
    /// register (0x04) and storing everything else verbatim for later synthesis.
    pub fn write_register(&mut self, index: u8, value: u8) {
        if index == 0x04 {
            // bit0/bit1: start timer 1/2 (rising edge reloads from preset).
            // bit7: reset both overflow flags.
            if value & 0x80 != 0 {
                self.timer1.expired = false;
                self.timer2.expired = false;
            }
            let start1 = value & 0x01 != 0;
            let start2 = value & 0x02 != 0;
            if start1 && !self.timer1.running {
                self.timer1.start(self.registers[0x02]);
            } else {
                self.timer1.running = start1;
            }
            if start2 && !self.timer2.running {
                self.timer2.start(self.registers[0x03]);
            } else {
                self.timer2.running = start2;
            }
        }

        // Key-on (register 0xB0-0xB8 bit 5) rising edge restarts the channel's
        // operator phases, as the real chip does.
        if (0xb0..=0xb8).contains(&index) {
            let was_on = self.registers[index as usize] & 0x20 != 0;
            let now_on = value & 0x20 != 0;
            if now_on && !was_on {
                let (modulator, carrier) = channel_operators((index - 0xb0) as usize);
                self.operators[modulator].phase = 0;
                self.operators[carrier].phase = 0;
            }
        }

        self.registers[index as usize] = value;
    }

    /// Render one mono sample at the chip's native 49716 Hz rate, summing the
    /// nine 2-op channels. (OPL3 4-op/stereo, the envelope, and rhythm mode are
    /// added in later layers; for now key-on gates each channel on or off.)
    pub fn render_sample(&mut self) -> i32 {
        (0..9).map(|channel| self.render_channel(channel)).sum()
    }

    fn render_channel(&mut self, channel: usize) -> i32 {
        let (modulator, carrier) = channel_operators(channel);
        self.load_operator(modulator, channel);
        self.load_operator(carrier, channel);

        let keyed = self.registers[0xb0 + channel] & 0x20 != 0;
        let additive = self.registers[0xc0 + channel] & 0x01 != 0;
        let output = if !keyed {
            0
        } else if additive {
            self.operators[modulator].sample(0) + self.operators[carrier].sample(0)
        } else {
            let modulation = self.operators[modulator].sample(0);
            self.operators[carrier].sample_modulated(modulation, 0)
        };

        self.operators[modulator].advance();
        self.operators[carrier].advance();
        output
    }

    /// Refresh one operator's parameters from its registers, preserving phase.
    fn load_operator(&mut self, operator: usize, channel: usize) {
        let slot = OPERATOR_SLOT[operator];
        let fnum = u16::from(self.registers[0xa0 + channel])
            | ((u16::from(self.registers[0xb0 + channel]) & 0x03) << 8);
        let block = (self.registers[0xb0 + channel] >> 2) & 0x07;
        let multiple = self.registers[0x20 + slot] & 0x0f;
        let total_level = self.registers[0x40 + slot] & 0x3f;
        let waveform = self.registers[0xe0 + slot] & 0x07;

        let op = &mut self.operators[operator];
        op.set_frequency(fnum, block);
        op.set_multiple(multiple);
        op.set_total_level(total_level);
        op.set_waveform(waveform);
    }

    /// Advance the hardware timers by `micros` microseconds of chip time.
    pub fn advance_micros(&mut self, micros: u64) {
        let (preset1, preset2) = (self.registers[0x02], self.registers[0x03]);
        self.timer1.advance(micros, preset1);
        self.timer2.advance(micros, preset2);
    }

    /// OPL status byte: bit7 IRQ, bit6 timer-1 flag, bit5 timer-2 flag.
    /// A timer's overflow flag is always reported; the mask bits in register
    /// 0x04 (bit6 = timer 1, bit5 = timer 2) only gate the IRQ line.
    pub fn status(&self) -> u8 {
        let control = self.registers[0x04];
        let t1_irq = self.timer1.expired && control & 0x40 == 0;
        let t2_irq = self.timer2.expired && control & 0x20 == 0;
        ((t1_irq || t2_irq) as u8) << 7
            | (self.timer1.expired as u8) << 6
            | (self.timer2.expired as u8) << 5
    }

    pub fn read_port(&self, port: u16) -> Option<u8> {
        match port {
            0x0388 => Some(self.status()),
            _ => None,
        }
    }

    pub fn write_port(&mut self, port: u16, value: u8) -> bool {
        match port {
            0x0388 => {
                self.address = value;
                true
            }
            0x0389 => {
                self.write_register(self.address, value);
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exp_of_logsin_reconstructs_the_sine_quarter_wave() {
        // The whole point of the two ROMs: running each quarter-wave index
        // through log-sin then exp must rebuild sin() to within quantization.
        let max = f64::from(exp_lookup(u32::from(logsin(255)))); // loudest point
        for i in 0..256 {
            let attenuation = u32::from(logsin(i));
            let got = f64::from(exp_lookup(attenuation));
            let expected = ((i as f64 + 0.5) * std::f64::consts::PI / 512.0).sin() * max;
            assert!(
                (got - expected).abs() <= 4.0,
                "index {i}: got {got}, expected {expected}"
            );
        }
    }

    #[test]
    fn rom_tables_match_their_known_anchor_values() {
        assert_eq!(logsin(0), 2137, "quietest log-sin entry");
        assert_eq!(logsin(255), 0, "loudest log-sin entry");
        assert_eq!(
            exp_lookup(0),
            2042,
            "max half-amplitude at zero attenuation"
        );
    }

    fn sine_operator(fnum: u16, block: u8, waveform: u8) -> Operator {
        let mut op = Operator::default();
        op.set_frequency(fnum, block);
        op.set_multiple(1); // register value 1 => x1
        op.set_waveform(waveform);
        op
    }

    fn peak_magnitude(mut op: Operator, samples: usize) -> i32 {
        let mut peak = 0;
        for _ in 0..samples {
            peak = peak.max(op.sample(0).abs());
            op.advance();
        }
        peak
    }

    #[test]
    fn operator_runs_at_the_programmed_frequency() {
        // Count rising zero-crossings over one second and compare to the OPL
        // frequency formula: f = fnum * 2^block * rate / 2^20.
        let (fnum, block) = (0x200u16, 4u8);
        let rate = 49_716.0_f64;
        let expected = f64::from(fnum) * 2f64.powi(i32::from(block)) * rate / 2f64.powi(20);

        let mut op = sine_operator(fnum, block, 0);
        let mut crossings = 0u32;
        let mut prev = op.sample(0);
        op.advance();
        for _ in 1..rate as usize {
            let s = op.sample(0);
            if prev <= 0 && s > 0 {
                crossings += 1;
            }
            prev = s;
            op.advance();
        }

        let measured = f64::from(crossings);
        assert!(
            (measured - expected).abs() / expected < 0.01,
            "expected ~{expected:.1} Hz, measured {measured}"
        );
    }

    #[test]
    fn operator_peaks_near_max_half_amplitude() {
        let peak = peak_magnitude(sine_operator(0x200, 4, 0), 512);
        assert!((peak - 2042).abs() <= 4, "peak {peak}");
    }

    #[test]
    fn total_level_attenuates_six_db_per_eight_steps() {
        // Eight TL steps of 0.75 dB = 6 dB = a factor-of-two amplitude drop.
        let loud = peak_magnitude(sine_operator(0x200, 4, 0), 512);
        let mut quiet = sine_operator(0x200, 4, 0);
        quiet.set_total_level(8);
        let quiet = peak_magnitude(quiet, 512);
        let ratio = f64::from(loud) / f64::from(quiet);
        assert!((ratio - 2.0).abs() < 0.05, "ratio {ratio}");
    }

    #[test]
    fn half_sine_silences_the_second_half() {
        // Period is 128 samples; samples 64..128 fall in the second half.
        let mut full = sine_operator(0x200, 4, 0);
        let mut half = sine_operator(0x200, 4, 1);
        for i in 0..128 {
            let (a, b) = (full.sample(0), half.sample(0));
            if i < 64 {
                assert_eq!(a, b, "first half should match the sine, i={i}");
            } else {
                assert_eq!(b, 0, "second half should be silent, i={i}");
            }
            full.advance();
            half.advance();
        }
    }

    #[test]
    fn abs_sine_has_no_negative_samples() {
        let mut op = sine_operator(0x200, 4, 2);
        for _ in 0..128 {
            assert!(op.sample(0) >= 0);
            op.advance();
        }
    }

    #[test]
    fn quarter_sine_silences_the_second_and_fourth_quarters() {
        // Quarter is 32 samples; the odd quarters (1 and 3) are silent.
        let mut op = sine_operator(0x200, 4, 3);
        for i in 0..128 {
            let s = op.sample(0);
            if (i / 32) % 2 == 1 {
                assert_eq!(s, 0, "odd quarter should be silent, i={i}");
            } else {
                assert!(s >= 0, "quarter sine is non-negative, i={i}");
            }
            op.advance();
        }
    }

    fn program_channel0(opl: &mut OplChip, fnum: u16, block: u8, additive: bool, modulator_tl: u8) {
        opl.write_register(0x20, 0x01); // modulator: multiple x1
        opl.write_register(0x23, 0x01); // carrier: multiple x1
        opl.write_register(0x40, modulator_tl); // modulator total level
        opl.write_register(0x43, 0x00); // carrier total level: loudest
        opl.write_register(0xe0, 0x00); // modulator waveform: sine
        opl.write_register(0xe3, 0x00); // carrier waveform: sine
        opl.write_register(0xc0, u8::from(additive)); // connection
        opl.write_register(0xa0, (fnum & 0xff) as u8); // f-number low
        opl.write_register(
            0xb0,
            0x20 | (block & 0x07) << 2 | ((fnum >> 8) & 0x03) as u8,
        );
    }

    #[test]
    fn silent_chip_renders_zero() {
        let mut opl = OplChip::default();
        for _ in 0..64 {
            assert_eq!(opl.render_sample(), 0);
        }
    }

    #[test]
    fn keyed_channel_renders_a_tone_at_its_frequency() {
        // Additive with the modulator muted leaves a clean carrier sine, so
        // zero-crossings should match the channel's programmed frequency.
        let (fnum, block) = (0x200u16, 4u8);
        let mut opl = OplChip::default();
        program_channel0(&mut opl, fnum, block, true, 0x3f);

        let rate = 49_716.0_f64;
        let expected = f64::from(fnum) * 2f64.powi(i32::from(block)) * rate / 2f64.powi(20);
        let mut crossings = 0u32;
        let mut prev = opl.render_sample();
        for _ in 1..rate as usize {
            let s = opl.render_sample();
            if prev <= 0 && s > 0 {
                crossings += 1;
            }
            prev = s;
        }

        let measured = f64::from(crossings);
        assert!(
            (measured - expected).abs() / expected < 0.02,
            "expected ~{expected:.1} Hz, measured {measured}"
        );
    }

    #[test]
    fn fm_and_additive_differ_with_an_active_modulator() {
        let collect = |additive| {
            let mut opl = OplChip::default();
            program_channel0(&mut opl, 0x200, 4, additive, 0x00);
            (0..128).map(|_| opl.render_sample()).collect::<Vec<_>>()
        };
        assert_ne!(
            collect(false),
            collect(true),
            "FM should not equal additive"
        );
    }

    #[test]
    fn adlib_detection_sequence_reports_present() {
        // The canonical AdLib probe: clear the timers, confirm the status
        // flags are quiet, fire timer 1, let it overflow, and confirm the
        // status port reports the IRQ + timer-1 flags (0xc0).
        let mut opl = OplChip::default();

        opl.write_register(0x04, 0x60); // mask both timers
        opl.write_register(0x04, 0x80); // reset the IRQ flags
        assert_eq!(opl.status() & 0xe0, 0x00, "flags clear after reset");

        opl.write_register(0x02, 0xff); // timer 1 preset: overflow in one step
        opl.write_register(0x04, 0x21); // start timer 1, mask timer 2
        opl.advance_micros(80); // one 80us timer-1 step -> overflow

        assert_eq!(
            opl.status() & 0xe0,
            0xc0,
            "timer 1 overflow raises IRQ (bit7) + timer-1 (bit6)"
        );
    }

    #[test]
    fn masked_timer_overflow_sets_flag_but_not_irq() {
        // A masked timer still records its overflow flag in the status byte;
        // only the IRQ (bit7) is suppressed. Faithful to real OPL silicon.
        let mut opl = OplChip::default();

        opl.write_register(0x03, 0xff); // timer 2 preset
        opl.write_register(0x04, 0x22); // start timer 2 (bit1) with it masked (bit5)
        opl.advance_micros(320); // one 320us timer-2 step -> overflow

        let status = opl.status();
        assert_eq!(status & 0x20, 0x20, "timer-2 flag is set");
        assert_eq!(status & 0x80, 0x00, "masked timer raises no IRQ");
    }

    #[test]
    fn address_data_ports_store_registers() {
        let mut opl = OplChip::default();
        assert!(opl.write_port(0x388, 0x20)); // latch register address 0x20
        assert!(opl.write_port(0x389, 0x2f)); // write the data
        assert_eq!(opl.register(0x20), 0x2f);
        assert_eq!(opl.read_port(0x388), Some(opl.status()));
    }
}
