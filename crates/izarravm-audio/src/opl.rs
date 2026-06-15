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

/// Key-scale-level base attenuation (the 6 dB/oct setting), in 0.75 dB units,
/// indexed by the top four F-number bits. Pitch costs ~6 dB per octave, so a
/// block step is 8 units (= 6 dB); within an octave the attenuation follows
/// log2 of the F-number: `ksl[n] = ceil(8 * log2(16*n))`, `ksl[0] = 0`. The
/// datasheet only prints the per-octave dB rate, so this is derived from it; the
/// result reproduces the standard KSL ROM `{0,32,40,45,...,63,64}` exactly.
fn build_ksl() -> [u16; 16] {
    let mut table = [0u16; 16];
    for (n, slot) in table.iter_mut().enumerate().skip(1) {
        *slot = (8.0 * (16.0 * n as f64).log2()).ceil() as u16;
    }
    table
}

use std::sync::LazyLock;

static LOGSIN: LazyLock<[u16; 256]> = LazyLock::new(build_logsin);
static EXP: LazyLock<[u16; 256]> = LazyLock::new(build_exp);
static KSL: LazyLock<[u16; 16]> = LazyLock::new(build_ksl);

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
    if shift >= 32 {
        return 0; // attenuated past audibility (and past a valid i32 shift)
    }
    (i32::from(EXP[fraction ^ 0xff]) + 1024) >> shift
}

/// Frequency multiplier (MULT register, 0..15), stored doubled so the phase
/// math stays integer: index 0 is x0.5, the rest are whole multiples with the
/// two documented duplicate slots (11->10, 13->12, 14->15... per the spec).
const MULTIPLE_X2: [u32; 16] = [1, 2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 20, 24, 24, 30, 30];

/// Envelope-generator phase. `Release` doubles as the idle / keyed-off state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EgState {
    Attack,
    Decay,
    Sustain,
    Release,
}

/// A single FM operator: a 20-bit phase accumulator, its waveform and level,
/// and an ADSR envelope generator. The envelope datapath (rates, curve, timing)
/// was derived from the YMF262's documented behaviour and cross-checked against
/// a reference; see dev_docs/opl3-plan.md.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Operator {
    phase: u32,
    fnum: u16,
    block: u8,
    multiple: u8,
    waveform: u8,
    total_level: u8,
    key_scale_level: u8,         // KSL: 0 = off, else 1.5/3/6 dB per octave of pitch
    feedback: u8,                // FB factor for operator-1 self-modulation (0 = off)
    feedback_history: [i32; 2],  // last two outputs; averaged for stable feedback
    tremolo: bool,               // AM: this operator follows the amplitude LFO
    vibrato: bool,               // VIB: this operator follows the pitch LFO
    // Envelope generator.
    attack: u8,
    decay: u8,
    sustain: u8,
    release: u8,
    sustained: bool,      // EGT: hold at the sustain level while keyed
    key_scale_rate: bool, // KSR: shorten the envelope at higher pitch
    key_on: bool,
    eg_level: u16, // 0 = loudest, 0x1ff = silent
    eg_state: EgState,
}

impl Default for Operator {
    fn default() -> Self {
        Self {
            phase: 0,
            fnum: 0,
            block: 0,
            multiple: 0,
            waveform: 0,
            total_level: 0,
            key_scale_level: 0,
            feedback: 0,
            feedback_history: [0, 0],
            tremolo: false,
            vibrato: false,
            attack: 0,
            decay: 0,
            sustain: 0,
            release: 0,
            sustained: false,
            key_scale_rate: false,
            key_on: false,
            eg_level: 0x1ff,
            eg_state: EgState::Release,
        }
    }
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

    pub(crate) fn set_key_scale_level(&mut self, value: u8) {
        self.key_scale_level = value & 0x03;
    }

    pub(crate) fn set_feedback(&mut self, value: u8) {
        self.feedback = value & 0x07;
    }

    pub(crate) fn set_tremolo(&mut self, on: bool) {
        self.tremolo = on;
    }

    pub(crate) fn set_vibrato(&mut self, on: bool) {
        self.vibrato = on;
    }

    /// Key-scale-level attenuation for the current pitch, in log-domain units
    /// (the same scale as `eg_attenuation`). 0 when KSL is off. The 6 dB/oct base
    /// `KSL[fnum>>6]` is referenced to the top octave (block 7) and lowered 8
    /// units (6 dB) per octave below it; settings 1/2/3 take 1/4, 1/2 and all of
    /// it (1.5/3/6 dB per octave). 0.75 dB == 32 log units, hence the `<< 5`.
    fn ksl_attenuation(&self) -> u16 {
        if self.key_scale_level == 0 {
            return 0;
        }
        let base = i32::from(KSL[(self.fnum >> 6) as usize]) - 8 * (7 - i32::from(self.block));
        let units = (base.max(0) as u32) >> (3 - u32::from(self.key_scale_level));
        (units << 5) as u16
    }

    pub(crate) fn set_envelope(&mut self, attack: u8, decay: u8, sustain: u8, release: u8) {
        self.attack = attack & 0x0f;
        self.decay = decay & 0x0f;
        self.sustain = sustain & 0x0f;
        self.release = release & 0x0f;
    }

    pub(crate) fn set_eg_type(&mut self, sustained: bool) {
        self.sustained = sustained;
    }

    pub(crate) fn set_key_scale_rate(&mut self, ksr: bool) {
        self.key_scale_rate = ksr;
    }

    pub(crate) fn set_key(&mut self, on: bool) {
        self.key_on = on;
    }

    /// Per-sample phase step for an effective F-number. `f = fnum * 2^block *
    /// rate / 2^20` for MULT x1.
    fn phase_increment(&self, fnum: u32) -> u32 {
        ((fnum << self.block) * MULTIPLE_X2[self.multiple as usize]) >> 1
    }

    /// Advance one sample with no LFO. Operator-level tests use this; the chip
    /// render path always goes through `advance_with_lfo`.
    #[cfg(test)]
    pub(crate) fn advance(&mut self) {
        self.advance_with_lfo(0, false);
    }

    /// Advance the phase, optionally applying vibrato. `vibrato_phase` is the
    /// global 0..7 pitch-LFO phase. When VIB is enabled the F-number is bent by
    /// an 8-step triangle whose peak adds `fnum >> 7` and whose half-steps add
    /// `fnum >> 8` (each one bit shallower for non-deep vibrato), giving about
    /// +/-14 or +/-7 cents.
    pub(crate) fn advance_with_lfo(&mut self, vibrato_phase: u8, deep_vibrato: bool) {
        let mut fnum = i32::from(self.fnum);
        if self.vibrato {
            let (half_shift, peak_shift) = if deep_vibrato { (8, 7) } else { (9, 8) };
            let half = i32::from(self.fnum) >> half_shift;
            let peak = i32::from(self.fnum) >> peak_shift;
            fnum += match vibrato_phase {
                1 | 3 => half,
                2 => peak,
                5 | 7 => -half,
                6 => -peak,
                _ => 0, // phases 0 and 4
            };
        }
        let inc = self.phase_increment(fnum.clamp(0, 0x3ff) as u32);
        self.phase = self.phase.wrapping_add(inc) & 0x000f_ffff;
    }

    /// Envelope attenuation in exp-table units. The 0..0x1ff envelope is
    /// 0.1875 dB/step, which is 8 of our log units (256 units == 6.02 dB).
    pub(crate) fn eg_attenuation(&self) -> u16 {
        self.eg_level << 3
    }

    /// Key-scale number: block plus one F-number MSB (which bit depends on NTS).
    fn key_scale_number(&self, note_select: bool) -> u8 {
        let bit = (self.fnum >> if note_select { 9 } else { 8 }) & 1;
        (self.block << 1) | bit as u8
    }

    /// Effective envelope rate: `4*rate + offset`, capped at 63. A rate nibble
    /// of 0 stays 0 (the envelope is frozen). The offset is the key-scale
    /// number, or its top two bits when KSR is off (datasheet p9-p10).
    fn effective_rate(&self, rate: u8, note_select: bool) -> u8 {
        if rate == 0 {
            return 0;
        }
        let ksn = self.key_scale_number(note_select);
        let offset = if self.key_scale_rate { ksn } else { ksn >> 2 };
        (4 * rate + offset).min(63)
    }

    /// Sustain target attenuation: 3 dB (16 units) per step, with 0xf the
    /// special 93 dB floor.
    fn sustain_target(&self) -> u16 {
        if self.sustain == 0x0f {
            0x1f0
        } else {
            u16::from(self.sustain) << 4
        }
    }

    /// Advance the envelope one sample using the global EG counter. Key-on from
    /// a released operator starts the attack (and restarts the phase, as the
    /// chip does); key-off drops straight to release.
    pub(crate) fn advance_envelope(&mut self, counter: u32, note_select: bool) {
        match (self.key_on, self.eg_state) {
            (true, EgState::Release) => {
                self.eg_state = EgState::Attack;
                self.phase = 0;
            }
            (false, state) if state != EgState::Release => {
                self.eg_state = EgState::Release;
            }
            _ => {}
        }

        let rate = match self.eg_state {
            EgState::Attack => self.attack,
            EgState::Decay => self.decay,
            EgState::Sustain => {
                if self.sustained {
                    return; // hold at the sustain level until key-off
                }
                self.release // percussive: keep decaying at the release rate
            }
            EgState::Release => self.release,
        };
        let eff = self.effective_rate(rate, note_select);
        let inc = eg_increment(eff, counter);

        match self.eg_state {
            EgState::Attack => {
                if eff >= 60 {
                    self.eg_level = 0; // rate_hi == 15: instant attack
                } else {
                    for _ in 0..inc {
                        if self.eg_level == 0 {
                            break;
                        }
                        self.eg_level -= (self.eg_level >> 3) + 1;
                    }
                }
                if self.eg_level == 0 {
                    self.eg_state = EgState::Decay;
                }
            }
            EgState::Decay => {
                self.eg_level = (self.eg_level + inc).min(0x1ff);
                if self.eg_level >= self.sustain_target() {
                    self.eg_level = self.sustain_target();
                    self.eg_state = EgState::Sustain;
                }
            }
            EgState::Sustain | EgState::Release => {
                self.eg_level = (self.eg_level + inc).min(0x1ff);
            }
        }
    }

    /// Signed operator output for the current phase. `extra_attenuation` carries
    /// the envelope contributions in log-domain units; total level is folded in
    /// here (0.75 dB per step == 32 log units).
    pub(crate) fn sample(&self, extra_attenuation: u16) -> i32 {
        self.sample_modulated(0, extra_attenuation)
    }

    /// Operator output with the carrier phase offset by `phase_modulation`, the
    /// modulator's signed output in wave-position units where 1024 units = one
    /// cycle = 2*pi. A full-scale modulator (~+/-2048) bends the carrier by
    /// ~4*pi, matching the datasheet's maximum feedback depth (FB = 7 -> 4*pi).
    /// Self-feedback reuses this path via `render_feedback`.
    pub(crate) fn sample_modulated(&self, phase_modulation: i32, extra_attenuation: u16) -> i32 {
        let attenuation = u32::from(self.total_level) * 32
            + u32::from(self.ksl_attenuation())
            + u32::from(extra_attenuation);
        let position =
            ((((self.phase >> 10) as i32).wrapping_add(phase_modulation)) & 0x3ff) as u32;
        waveform_output_at(position, self.waveform, attenuation)
    }

    /// Operator-1 output with self-feedback (reg 0xC0 bits 1-3). The chip feeds
    /// the average of the last two outputs back into the phase to keep the loop
    /// stable. The radian table (FB 1..7 = pi/16..4*pi) doubles each step; in
    /// phase units (1024 = 2*pi) full depth (4*pi) is the full-scale output, so
    /// the average is shifted by `8 - FB` (one extra bit for the /2 average).
    pub(crate) fn render_feedback(&mut self, extra_attenuation: u16) -> i32 {
        let modulation = if self.feedback == 0 {
            0
        } else {
            (self.feedback_history[0] + self.feedback_history[1]) >> (8 - self.feedback)
        };
        let out = self.sample_modulated(modulation, extra_attenuation);
        self.feedback_history = [out, self.feedback_history[0]];
        out
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
        // Waveforms 4-7 are OPL3-only (gated to 0-3 unless NEW is set).
        // 4: full sine at double rate in the first half, silent in the second.
        4 => (!second_half)
            .then(|| waveform_attenuation((position << 1) & 0x3ff, 0))
            .flatten(),
        // 5: abs sine at double rate in the first half, silent in the second.
        5 => (!second_half)
            .then(|| waveform_attenuation((position << 1) & 0x3ff, 2))
            .flatten(),
        6 => Some((0, second_half)), // square wave: constant full magnitude
        // 7: logarithmic sawtooth; each half starts loud and decays as the
        // position ramps the attenuation linearly (8 log units per phase step).
        _ => Some((((position & 0x1ff) << 3) as u16, second_half)),
    }
}

fn waveform_output_at(position: u32, waveform: u8, attenuation: u32) -> i32 {
    let Some((wave_attenuation, negative)) = waveform_attenuation(position, waveform) else {
        return 0;
    };
    let magnitude = exp_lookup(u32::from(wave_attenuation) + attenuation);
    if negative { -magnitude } else { magnitude }
}

/// Envelope increment for this sample. The global counter ticks once per
/// sample; an EG step happens every `2^(13 - rate_hi)` samples (rate_hi <= 12)
/// or every sample with a scaled increment above that. The low two rate bits
/// pick an 8-phase pattern averaging 1.0 / 1.25 / 1.5 / 1.75 per step. Derived
/// from the chip's documented timing and validated against a reference.
fn eg_increment(effective_rate: u8, counter: u32) -> u16 {
    if effective_rate == 0 {
        return 0;
    }
    const PATTERN: [[u16; 8]; 4] = [
        [1, 1, 1, 1, 1, 1, 1, 1],
        [1, 1, 1, 2, 1, 1, 1, 2],
        [1, 2, 1, 2, 1, 2, 1, 2],
        [1, 2, 2, 2, 1, 2, 2, 2],
    ];
    let rate_hi = effective_rate >> 2;
    let rate_lo = (effective_rate & 3) as usize;
    if rate_hi < 13 {
        let shift = 13 - rate_hi;
        if counter & ((1 << shift) - 1) != 0 {
            return 0;
        }
        let phase = ((counter >> shift) & 7) as usize;
        PATTERN[rate_lo][phase]
    } else {
        let phase = (counter & 7) as usize;
        PATTERN[rate_lo][phase] << (rate_hi - 13)
    }
}

/// Register slot offset for the 18 operators in one bank. The OPL leaves gaps at
/// offsets 6,7,14,15, so operator `i` reads its registers at
/// `base + OPERATOR_SLOT[i % 18]`; the second bank repeats the same offsets.
const OPERATOR_SLOT: [usize; 18] = [
    0, 1, 2, 3, 4, 5, 8, 9, 10, 11, 12, 13, 16, 17, 18, 19, 20, 21,
];

/// The (modulator, carrier) operator indices for a 2-op channel (0..18).
/// Channels 0-8 live in bank 0 (operators 0-17), 9-17 in bank 1 (18-35).
fn channel_operators(channel: usize) -> (usize, usize) {
    let local = channel % 9;
    let base = (channel / 9) * 18 + (local / 3) * 6 + (local % 3);
    (base, base + 3)
}

/// 2-op channels that can become the primary half of a 4-op voice. Each pairs
/// with the channel three higher (e.g. 0 with 3); reg 0x104 bit N enables the
/// Nth pair here.
const FOUR_OP_PRIMARY: [usize; 6] = [0, 1, 2, 9, 10, 11];

/// Tremolo LFO period in samples at 49716 Hz (3.7 Hz). Vibrato uses a power-of-2
/// 8-phase counter (`eg_counter >> 10`, 8192 samples ~= 6.07 Hz) instead.
const TREMOLO_PERIOD: u32 = 13437;

/// Whether `channel` is the primary half of an active 4-op voice (renders all
/// four operators) under the reg 0x104 `mask`.
fn four_op_primary(channel: usize, mask: u8) -> bool {
    FOUR_OP_PRIMARY
        .iter()
        .position(|&p| p == channel)
        .is_some_and(|bit| mask & (1 << bit) != 0)
}

/// Whether `channel` is the secondary half of an active 4-op voice. Such a
/// channel is skipped: its two operators are rendered by the paired primary.
fn four_op_secondary(channel: usize, mask: u8) -> bool {
    channel >= 3
        && FOUR_OP_PRIMARY
            .iter()
            .position(|&p| p == channel - 3)
            .is_some_and(|bit| mask & (1 << bit) != 0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OplChip {
    /// Two register banks: 0 = primary (ports 0x388/0x389, channels 0-8),
    /// 1 = secondary (ports 0x38A/0x38B, channels 9-17 and the OPL3 control
    /// registers 0x104 four-op-enable / 0x105 NEW).
    registers: [[u8; 256]; 2],
    /// Latched register address per bank (port base+0 / base+2).
    address: [u8; 2],
    timer1: Timer,
    timer2: Timer,
    operators: [Operator; 36],
    eg_counter: u32,
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
            registers: [[0; 256]; 2],
            address: [0, 0],
            timer1: Timer::new(80),
            timer2: Timer::new(320),
            operators: std::array::from_fn(|_| Operator::default()),
            eg_counter: 0,
        }
    }
}

impl OplChip {
    pub fn register(&self, index: u8) -> u8 {
        self.registers[0][index as usize]
    }

    /// Write a primary-bank register (port 0x389). OPL2 programs and AdLib
    /// detection use this; OPL3 secondary-bank writes arrive via `write_port`.
    pub fn write_register(&mut self, index: u8, value: u8) {
        self.write_bank(0, index, value);
    }

    /// Write `value` into `bank`'s register `index`, applying the timer-control
    /// side effects (primary 0x04 only) and storing everything else verbatim.
    fn write_bank(&mut self, bank: usize, index: u8, value: u8) {
        if bank == 0 && index == 0x04 {
            // bit0/bit1: start timer 1/2 (rising edge reloads from preset).
            // bit7: reset both overflow flags.
            if value & 0x80 != 0 {
                self.timer1.expired = false;
                self.timer2.expired = false;
            }
            let start1 = value & 0x01 != 0;
            let start2 = value & 0x02 != 0;
            if start1 && !self.timer1.running {
                self.timer1.start(self.registers[0][0x02]);
            } else {
                self.timer1.running = start1;
            }
            if start2 && !self.timer2.running {
                self.timer2.start(self.registers[0][0x03]);
            } else {
                self.timer2.running = start2;
            }
        }

        self.registers[bank][index as usize] = value;
    }

    /// OPL3 mode (reg 0x105 bit0 / NEW): enables 18 channels, 8 waveforms and
    /// stereo. Cleared by default, where the chip behaves as an OPL2.
    fn opl3_enabled(&self) -> bool {
        self.registers[1][0x05] & 0x01 != 0
    }

    /// Tremolo (AM) attenuation for the current LFO phase: a triangle rising to
    /// the reg 0xBD bit7 depth (4.8 dB, else 1.0 dB) and back, in log-domain
    /// units (256 units = 6.02 dB, so 4.8 dB ~= 204 and 1.0 dB ~= 43).
    fn tremolo_attenuation(&self) -> u16 {
        let pos = self.eg_counter % TREMOLO_PERIOD;
        let half = TREMOLO_PERIOD / 2;
        let up = if pos < half { pos } else { TREMOLO_PERIOD - pos };
        let peak = if self.registers[0][0xbd] & 0x80 != 0 { 204 } else { 43 };
        (up * peak / half) as u16
    }

    /// Vibrato (pitch) LFO phase 0..7 at ~6.07 Hz (one step per 1024 samples).
    /// The per-operator F-number bend is applied in `advance_with_lfo`.
    fn vibrato_phase(&self) -> u8 {
        ((self.eg_counter >> 10) & 7) as u8
    }

    fn deep_vibrato(&self) -> bool {
        self.registers[0][0xbd] & 0x40 != 0
    }

    /// An operator's total attenuation: its envelope plus the tremolo LFO when
    /// AM is enabled for it.
    fn operator_attenuation(&self, op: usize) -> u16 {
        let mut attenuation = self.operators[op].eg_attenuation();
        if self.operators[op].tremolo {
            attenuation += self.tremolo_attenuation();
        }
        attenuation
    }

    /// Render one stereo `(left, right)` sample at the chip's native 49716 Hz
    /// rate. OPL3 mode sums all 18 two-op channels; otherwise the 9 OPL2
    /// channels. (4-op and rhythm mode are added in later layers.) The EG
    /// counter ticks per sample.
    pub fn render_sample(&mut self) -> (i32, i32) {
        self.eg_counter = self.eg_counter.wrapping_add(1);
        let channels = if self.opl3_enabled() { 18 } else { 9 };
        let mask = self.four_op_mask();
        let (mut left, mut right) = (0, 0);
        for channel in 0..channels {
            if four_op_secondary(channel, mask) {
                continue; // operators rendered by the paired 4-op primary
            }
            let out = if four_op_primary(channel, mask) {
                self.render_four_op(channel)
            } else {
                self.render_channel(channel)
            };
            // 4-op voices are panned by their primary channel; per-carrier
            // routing across both channels' pan bits is left for later if needed.
            let (l, r) = self.channel_pan(channel);
            if l {
                left += out;
            }
            if r {
                right += out;
            }
        }
        (left, right)
    }

    /// The reg 0x104 four-operator enable mask (six channel pairs), or 0 when
    /// the chip is not in OPL3 mode.
    fn four_op_mask(&self) -> u8 {
        if self.opl3_enabled() {
            self.registers[1][0x04] & 0x3f
        } else {
            0
        }
    }

    /// Render a 4-op voice whose primary 2-op channel is `channel` (and whose
    /// secondary is `channel + 3`). All four operators take their pitch and
    /// key-on from the primary channel; only operator 1 uses feedback. The
    /// connection bits of the two channels select one of four algorithms.
    fn render_four_op(&mut self, channel: usize) -> i32 {
        let note_select = self.registers[0][0x08] & 0x40 != 0;
        let bank = channel / 9;
        let ch = channel % 9;
        let c0_first = self.registers[bank][0xc0 + ch];
        let c0_second = self.registers[bank][0xc0 + ch + 3];
        let (op1, op2) = channel_operators(channel);
        let (op3, op4) = channel_operators(channel + 3);

        for op in [op1, op2, op3, op4] {
            self.load_operator(op, channel);
        }
        self.operators[op1].set_feedback((c0_first >> 1) & 0x07);
        for op in [op1, op2, op3, op4] {
            self.operators[op].advance_envelope(self.eg_counter, note_select);
        }
        let (a1, a2, a3, a4) = (
            self.operator_attenuation(op1),
            self.operator_attenuation(op2),
            self.operator_attenuation(op3),
            self.operator_attenuation(op4),
        );

        let o1 = self.operators[op1].render_feedback(a1);
        let out = match (c0_first & 1, c0_second & 1) {
            (0, 0) => {
                // FM-FM: serial 1 -> 2 -> 3 -> 4.
                let o2 = self.operators[op2].sample_modulated(o1, a2);
                let o3 = self.operators[op3].sample_modulated(o2, a3);
                self.operators[op4].sample_modulated(o3, a4)
            }
            (0, 1) => {
                // FM-AM: (1 -> 2) + (3 -> 4).
                let o2 = self.operators[op2].sample_modulated(o1, a2);
                let o3 = self.operators[op3].sample(a3);
                let o4 = self.operators[op4].sample_modulated(o3, a4);
                o2 + o4
            }
            (1, 0) => {
                // AM-FM: 1 + (2 -> 3 -> 4).
                let o2 = self.operators[op2].sample(a2);
                let o3 = self.operators[op3].sample_modulated(o2, a3);
                let o4 = self.operators[op4].sample_modulated(o3, a4);
                o1 + o4
            }
            _ => {
                // AM-AM: 1 + (2 -> 3) + 4.
                let o2 = self.operators[op2].sample(a2);
                let o3 = self.operators[op3].sample_modulated(o2, a3);
                let o4 = self.operators[op4].sample(a4);
                o1 + o3 + o4
            }
        };

        let (vibrato, deep) = (self.vibrato_phase(), self.deep_vibrato());
        for op in [op1, op2, op3, op4] {
            self.operators[op].advance_with_lfo(vibrato, deep);
        }
        out
    }

    /// Which outputs a channel feeds. OPL3 pans via reg 0xC0 bit4 (left) / bit5
    /// (right) on the carrier; OPL2 mode has no panning, so every channel feeds
    /// both. A channel with neither bit set in OPL3 mode is silent.
    fn channel_pan(&self, channel: usize) -> (bool, bool) {
        if !self.opl3_enabled() {
            return (true, true);
        }
        let c0 = self.registers[channel / 9][0xc0 + channel % 9];
        (c0 & 0x10 != 0, c0 & 0x20 != 0)
    }

    fn render_channel(&mut self, channel: usize) -> i32 {
        let note_select = self.registers[0][0x08] & 0x40 != 0;
        let bank = channel / 9;
        let ch = channel % 9;
        let c0 = self.registers[bank][0xc0 + ch];
        let (modulator, carrier) = channel_operators(channel);
        self.load_operator(modulator, channel);
        self.load_operator(carrier, channel);
        self.operators[modulator].set_feedback((c0 >> 1) & 0x07);
        self.operators[modulator].advance_envelope(self.eg_counter, note_select);
        self.operators[carrier].advance_envelope(self.eg_counter, note_select);

        let additive = c0 & 0x01 != 0;
        let modulator_att = self.operator_attenuation(modulator);
        let carrier_att = self.operator_attenuation(carrier);
        let modulator_out = self.operators[modulator].render_feedback(modulator_att);
        let output = if additive {
            modulator_out + self.operators[carrier].sample(carrier_att)
        } else {
            self.operators[carrier].sample_modulated(modulator_out, carrier_att)
        };

        let (vibrato, deep) = (self.vibrato_phase(), self.deep_vibrato());
        self.operators[modulator].advance_with_lfo(vibrato, deep);
        self.operators[carrier].advance_with_lfo(vibrato, deep);
        output
    }

    /// Refresh one operator's parameters from its registers, preserving phase
    /// and envelope state. The operator and its channel share a bank.
    fn load_operator(&mut self, operator: usize, channel: usize) {
        let bank = channel / 9;
        let ch = channel % 9;
        let slot = OPERATOR_SLOT[operator % 18];
        let regs = &self.registers[bank];
        let fnum = u16::from(regs[0xa0 + ch]) | ((u16::from(regs[0xb0 + ch]) & 0x03) << 8);
        let block = (regs[0xb0 + ch] >> 2) & 0x07;
        let r20 = regs[0x20 + slot];
        let r40 = regs[0x40 + slot];
        let total_level = r40 & 0x3f;
        let ad = regs[0x60 + slot];
        let sr = regs[0x80 + slot];
        let key_on = regs[0xb0 + ch] & 0x20 != 0;
        // Waveform select is gated: forced to sine unless WSEnable (0x01 bit5);
        // waveforms 4-7 only exist in OPL3 mode (NEW), else masked to 0-3.
        let waveform = if self.registers[0][0x01] & 0x20 == 0 {
            0
        } else if self.opl3_enabled() {
            regs[0xe0 + slot] & 0x07
        } else {
            regs[0xe0 + slot] & 0x03
        };

        let op = &mut self.operators[operator];
        op.set_frequency(fnum, block);
        op.set_multiple(r20 & 0x0f);
        op.set_key_scale_rate(r20 & 0x10 != 0);
        op.set_eg_type(r20 & 0x20 != 0);
        op.set_vibrato(r20 & 0x40 != 0);
        op.set_tremolo(r20 & 0x80 != 0);
        op.set_total_level(total_level);
        op.set_key_scale_level(r40 >> 6);
        op.set_waveform(waveform);
        op.set_envelope(ad >> 4, ad & 0x0f, sr >> 4, sr & 0x0f);
        op.set_key(key_on);
    }

    /// Current envelope attenuation (0 = loud, 0x1ff = silent) of an operator.
    /// Exposed for the dev-only cross-check harness; not part of the chip's API.
    #[doc(hidden)]
    pub fn envelope_level(&self, operator: usize) -> u16 {
        self.operators[operator].eg_level
    }

    /// Advance the hardware timers by `micros` microseconds of chip time.
    pub fn advance_micros(&mut self, micros: u64) {
        let (preset1, preset2) = (self.registers[0][0x02], self.registers[0][0x03]);
        self.timer1.advance(micros, preset1);
        self.timer2.advance(micros, preset2);
    }

    /// OPL status byte: bit7 IRQ, bit6 timer-1 flag, bit5 timer-2 flag.
    /// A timer's overflow flag is always reported; the mask bits in register
    /// 0x04 (bit6 = timer 1, bit5 = timer 2) only gate the IRQ line.
    pub fn status(&self) -> u8 {
        let control = self.registers[0][0x04];
        let t1_irq = self.timer1.expired && control & 0x40 == 0;
        let t2_irq = self.timer2.expired && control & 0x20 == 0;
        ((t1_irq || t2_irq) as u8) << 7
            | (self.timer1.expired as u8) << 6
            | (self.timer2.expired as u8) << 5
    }

    pub fn read_port(&self, port: u16) -> Option<u8> {
        match port {
            // The status byte is mirrored on both base+0 and base+2.
            0x0388 | 0x038a => Some(self.status()),
            _ => None,
        }
    }

    pub fn write_port(&mut self, port: u16, value: u8) -> bool {
        match port {
            0x0388 => self.address[0] = value,
            0x0389 => self.write_bank(0, self.address[0], value),
            0x038a => self.address[1] = value,
            0x038b => self.write_bank(1, self.address[1], value),
            _ => return false,
        }
        true
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
    fn exp_lookup_saturates_to_zero_for_large_attenuation() {
        // A fully-silent envelope plus total level can push attenuation past a
        // valid 32-bit shift; it must saturate to silence, not overflow.
        assert_eq!(exp_lookup(0x1ff << 3), 0);
        assert_eq!(exp_lookup(0x2000), 0);
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

    fn ksl_operator(fnum: u16, block: u8, setting: u8) -> Operator {
        let mut op = sine_operator(fnum, block, 0);
        op.set_key_scale_level(setting);
        op
    }

    #[test]
    fn ksl_table_follows_the_six_db_per_octave_derivation() {
        // ceil(8*log2(16n)) in 0.75 dB units: 8 units = 6 dB = one octave.
        assert_eq!(KSL[0], 0, "no attenuation at the lowest F-number");
        for n in 1..16usize {
            let expected = (8.0 * (16.0 * n as f64).log2()).ceil() as u16;
            assert_eq!(KSL[n], expected, "ksl[{n}]");
        }
        // Reproduces the standard KSL ROM; top entry = 64 units = 48 dB.
        assert_eq!(
            *KSL,
            [0, 32, 40, 45, 48, 51, 53, 55, 56, 58, 59, 60, 61, 62, 63, 64]
        );
    }

    #[test]
    fn ksl_zero_leaves_output_unattenuated() {
        let plain = peak_magnitude(sine_operator(0x300, 6, 0), 256);
        let off = peak_magnitude(ksl_operator(0x300, 6, 0), 256);
        assert_eq!(off, plain, "KSL=0 must not change the output");
    }

    #[test]
    fn ksl_attenuates_six_db_per_octave_at_max_setting() {
        // Setting 3 = 6 dB/oct; one octave up at the same F-number halves output.
        let lo = peak_magnitude(ksl_operator(0x200, 5, 3), 256);
        let hi = peak_magnitude(ksl_operator(0x200, 6, 3), 256);
        let ratio = f64::from(lo) / f64::from(hi);
        assert!((ratio - 2.0).abs() < 0.05, "expected ~2x per octave, got {ratio}");
    }

    #[test]
    fn ksl_settings_scale_the_attenuation() {
        // block 6, fnum 0x200 (n=8): base = KSL[8] - 8*(7-6) = 56 - 8 = 48 units.
        // Settings 1/2/3 attenuate by a quarter/half/all of the 6 dB/oct value.
        let base = 48u16;
        assert_eq!(ksl_operator(0x200, 6, 3).ksl_attenuation(), base << 5);
        assert_eq!(ksl_operator(0x200, 6, 2).ksl_attenuation(), (base >> 1) << 5);
        assert_eq!(ksl_operator(0x200, 6, 1).ksl_attenuation(), (base >> 2) << 5);
        assert_eq!(ksl_operator(0x200, 6, 0).ksl_attenuation(), 0);
    }

    #[test]
    fn ksl_clamps_to_zero_for_low_pitch() {
        // Bottom octave with a small F-number sits below the reference: no cost.
        assert_eq!(ksl_operator(0x000, 0, 3).ksl_attenuation(), 0);
    }

    // Channel 0 with only the modulator (op 0) audible: carrier never opens
    // (attack 0), additive, modulator at instant attack, self-feedback `fb`.
    fn feedback_channel_samples(fb: u8) -> Vec<i32> {
        let mut opl = OplChip::default();
        opl.write_register(0x20, 0x01); // modulator: multiple x1
        opl.write_register(0x23, 0x01); // carrier: multiple x1
        opl.write_register(0x40, 0x00); // modulator loud
        opl.write_register(0x43, 0x00);
        opl.write_register(0x60, 0xf0); // modulator: instant attack
        opl.write_register(0x63, 0x00); // carrier: attack 0 -> stays silent
        opl.write_register(0x80, 0x00);
        opl.write_register(0x83, 0x00);
        opl.write_register(0xe0, 0x00); // modulator waveform: sine
        opl.write_register(0xc0, 0x01 | (fb << 1)); // additive + feedback factor
        opl.write_register(0xa0, 0x00);
        opl.write_register(0xb0, 0x20 | (4 << 2) | 0x02); // key-on, block 4, fnum 0x200
        (0..256).map(|_| opl.render_sample().0).collect()
    }

    #[test]
    fn feedback_zero_is_a_clean_sine() {
        // FB=0 leaves the modulator unmodulated: it must equal a bare sine.
        let mut reference = sine_operator(0x200, 4, 0);
        let expected: Vec<i32> = (0..256)
            .map(|_| {
                let s = reference.sample(0);
                reference.advance();
                s
            })
            .collect();
        assert_eq!(feedback_channel_samples(0), expected);
    }

    #[test]
    fn feedback_alters_and_bounds_the_modulator() {
        let plain = feedback_channel_samples(0);
        let fed = feedback_channel_samples(7);
        assert_ne!(plain, fed, "feedback must reshape the waveform");
        let peak = fed.iter().map(|s| s.abs()).max().unwrap();
        assert!(peak <= 2100, "self-feedback stays bounded, got {peak}");
    }

    #[test]
    fn stronger_feedback_deviates_further_from_a_sine() {
        // Distance from the FB=0 sine grows with the feedback factor.
        let base = feedback_channel_samples(0);
        let dist = |fb| {
            feedback_channel_samples(fb)
                .iter()
                .zip(&base)
                .map(|(a, b)| u64::from((a - b).unsigned_abs()))
                .sum::<u64>()
        };
        assert!(dist(2) < dist(5), "FB=5 should deviate more than FB=2");
    }

    #[test]
    fn opl3_waveform4_is_a_double_rate_sine_in_the_first_half() {
        // Period 128, first half 0..64. WAVE4 packs a full sine into the first
        // half (both signs) and silences the second.
        let mut op = sine_operator(0x200, 4, 4);
        let (mut saw_pos, mut saw_neg) = (false, false);
        for i in 0..128 {
            let s = op.sample(0);
            if i < 64 {
                saw_pos |= s > 0;
                saw_neg |= s < 0;
            } else {
                assert_eq!(s, 0, "second half is silent, i={i}");
            }
            op.advance();
        }
        assert!(saw_pos && saw_neg, "first half is a full sine");
    }

    #[test]
    fn opl3_waveform5_is_double_rate_abs_sine_in_the_first_half() {
        let mut op = sine_operator(0x200, 4, 5);
        let mut peak = 0;
        for i in 0..128 {
            let s = op.sample(0);
            if i < 64 {
                assert!(s >= 0, "abs sine is non-negative, i={i}");
                peak = peak.max(s);
            } else {
                assert_eq!(s, 0, "second half is silent, i={i}");
            }
            op.advance();
        }
        assert!(peak > 1000, "first half has audible humps");
    }

    #[test]
    fn opl3_waveform6_is_a_square_wave() {
        // Constant full-scale magnitude (exp_lookup(0) = 2042), sign flips at half.
        let mut op = sine_operator(0x200, 4, 6);
        for i in 0..128 {
            let expected = if i < 64 { 2042 } else { -2042 };
            assert_eq!(op.sample(0), expected, "square wave, i={i}");
            op.advance();
        }
    }

    #[test]
    fn opl3_waveform7_is_a_log_sawtooth() {
        // Each half starts at the peak and decays; sign flips at the half.
        let mut op = sine_operator(0x200, 4, 7);
        let samples: Vec<i32> = (0..128)
            .map(|_| {
                let s = op.sample(0);
                op.advance();
                s
            })
            .collect();
        assert!(samples[0] > 2000, "first half starts at the positive peak");
        assert!(samples[64] < -2000, "second half starts at the negative peak");
        assert!(samples[32] < samples[0], "first half decays toward zero");
        assert!(samples[96].abs() < samples[64].abs(), "second half decays");
    }

    fn program_channel0(opl: &mut OplChip, fnum: u16, block: u8, additive: bool, modulator_tl: u8) {
        opl.write_register(0x20, 0x01); // modulator: multiple x1
        opl.write_register(0x23, 0x01); // carrier: multiple x1
        opl.write_register(0x40, modulator_tl); // modulator total level
        opl.write_register(0x43, 0x00); // carrier total level: loudest
        opl.write_register(0x60, 0xf0); // both operators: attack 15 (instant), decay 0
        opl.write_register(0x63, 0xf0);
        opl.write_register(0x80, 0x00); // sustain 0, release 0
        opl.write_register(0x83, 0x00);
        opl.write_register(0xe0, 0x00); // modulator waveform: sine
        opl.write_register(0xe3, 0x00); // carrier waveform: sine
        opl.write_register(0xc0, u8::from(additive)); // connection
        opl.write_register(0xa0, (fnum & 0xff) as u8); // f-number low
        opl.write_register(
            0xb0,
            0x20 | (block & 0x07) << 2 | ((fnum >> 8) & 0x03) as u8,
        );
    }

    // Carrier (operator 3) envelope after rendering `samples`, keyed at block 4.
    fn carrier_eg_after(setup: impl Fn(&mut OplChip), samples: usize) -> u16 {
        let mut opl = OplChip::default();
        setup(&mut opl);
        for _ in 0..samples {
            opl.render_sample();
        }
        opl.envelope_level(3)
    }

    fn key_carrier(opl: &mut OplChip, ar: u8, dr: u8, sl: u8, rr: u8) {
        opl.write_register(0x23, 0x21); // EGT sustained, multiple 1
        opl.write_register(0x43, 0x00); // total level 0
        opl.write_register(0x63, (ar << 4) | dr);
        opl.write_register(0x83, (sl << 4) | rr);
        opl.write_register(0xa0, 0x00);
        opl.write_register(0xc0, 0x01); // additive, so the carrier reaches output
        opl.write_register(0xb0, 0x20 | (4 << 2)); // key-on, block 4
    }

    #[test]
    fn attack_opens_the_envelope_to_full_volume() {
        let eg = carrier_eg_after(|opl| key_carrier(opl, 15, 0, 0, 0), 4);
        assert_eq!(eg, 0, "instant attack reaches full volume");
    }

    #[test]
    fn zero_attack_rate_keeps_the_operator_silent() {
        let eg = carrier_eg_after(|opl| key_carrier(opl, 0, 0, 0, 0), 5000);
        assert_eq!(eg, 0x1ff, "attack rate 0 never opens");
    }

    #[test]
    fn higher_attack_rate_opens_faster() {
        let slow = carrier_eg_after(|opl| key_carrier(opl, 6, 0, 0, 0), 1500);
        let fast = carrier_eg_after(|opl| key_carrier(opl, 8, 0, 0, 0), 1500);
        assert!(
            fast < slow,
            "AR=8 should be further along than AR=6: {fast} vs {slow}"
        );
    }

    #[test]
    fn decay_falls_to_the_sustain_level_and_holds() {
        let eg = carrier_eg_after(|opl| key_carrier(opl, 15, 12, 8, 0), 2000);
        assert_eq!(eg, 0x80, "decay settles and holds at sustain level 8");
    }

    #[test]
    fn key_off_releases_to_silence() {
        let mut opl = OplChip::default();
        key_carrier(&mut opl, 15, 0, 0, 8); // instant attack, release rate 8
        for _ in 0..8 {
            opl.render_sample();
        }
        assert_eq!(opl.envelope_level(3), 0, "keyed and open");

        opl.write_register(0xb0, 4 << 2); // key-off, keep block
        for _ in 0..20_000 {
            opl.render_sample();
        }
        assert_eq!(opl.envelope_level(3), 0x1ff, "released to silence");
    }

    #[test]
    fn silent_chip_renders_zero() {
        let mut opl = OplChip::default();
        for _ in 0..64 {
            assert_eq!(opl.render_sample(), (0, 0));
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
        let mut prev = opl.render_sample().0;
        for _ in 1..rate as usize {
            let s = opl.render_sample().0;
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
            (0..128).map(|_| opl.render_sample().0).collect::<Vec<_>>()
        };
        assert_ne!(
            collect(false),
            collect(true),
            "FM should not equal additive"
        );
    }

    #[test]
    fn channel_applies_key_scale_level_from_registers() {
        // Decode reg 0x40 bits6-7 into the carrier: at block 6 / fnum 0x200 a
        // KSL of 3 is 36 dB of attenuation, so the keyed tone is far quieter.
        let peak = |ksl_bits: u8| {
            let mut opl = OplChip::default();
            opl.write_register(0x23, 0x21); // carrier: sustained, multiple x1
            opl.write_register(0x43, ksl_bits << 6); // KSL in bits 6-7, total level 0
            opl.write_register(0x63, 0xf0); // attack 15 (instant)
            opl.write_register(0x83, 0x00);
            opl.write_register(0xa0, 0x00); // f-number low
            opl.write_register(0xc0, 0x01); // additive, so the carrier reaches output
            opl.write_register(0xb0, 0x20 | (6 << 2) | 0x02); // key-on, block 6, fnum 0x200
            (0..64).map(|_| opl.render_sample().0.abs()).max().unwrap()
        };
        let loud = peak(0);
        let scaled = peak(3);
        assert!(
            scaled * 10 < loud,
            "KSL=3 at block 6 must strongly attenuate: {scaled} vs {loud}"
        );
    }

    // Write `value` into secondary-bank register `index` via ports 0x38A/0x38B.
    fn write_secondary(opl: &mut OplChip, index: u8, value: u8) {
        opl.write_port(0x38a, index);
        opl.write_port(0x38b, value);
    }

    #[test]
    fn opl3_mode_unlocks_the_secondary_bank_channels() {
        // Channel 9 lives in the secondary bank; it is silent until OPL3 mode
        // (reg 0x105 / NEW) is enabled and the chip renders all 18 channels.
        let setup = |opl: &mut OplChip| {
            write_secondary(opl, 0x23, 0x21); // carrier (op 21): sustained, multiple x1
            write_secondary(opl, 0x43, 0x00); // carrier total level 0
            write_secondary(opl, 0x63, 0xf0); // attack 15 (instant)
            write_secondary(opl, 0x83, 0x00);
            write_secondary(opl, 0xa0, 0x00); // channel 9 f-number low
            write_secondary(opl, 0xc0, 0x31); // additive + left/right enable
            write_secondary(opl, 0xb0, 0x20 | (4 << 2) | 0x02); // key-on, block 4, fnum 0x200
        };
        let peak = |opl: &mut OplChip| (0..256).map(|_| opl.render_sample().0.abs()).max().unwrap();

        let mut off = OplChip::default();
        setup(&mut off);
        assert_eq!(peak(&mut off), 0, "secondary channel is silent without OPL3 mode");

        let mut on = OplChip::default();
        write_secondary(&mut on, 0x05, 0x01); // NEW: enable OPL3 mode
        setup(&mut on);
        assert!(peak(&mut on) > 1000, "secondary channel sounds in OPL3 mode");
    }

    #[test]
    fn opl3_panning_routes_the_channel_to_selected_outputs() {
        // reg 0xC0 bit4 = left, bit5 = right; neither set leaves the channel mute.
        let peaks = |c0: u8| {
            let mut opl = OplChip::default();
            write_secondary(&mut opl, 0x05, 0x01); // NEW
            write_secondary(&mut opl, 0x23, 0x21); // carrier sustained, multiple x1
            write_secondary(&mut opl, 0x43, 0x00);
            write_secondary(&mut opl, 0x63, 0xf0); // instant attack
            write_secondary(&mut opl, 0x83, 0x00);
            write_secondary(&mut opl, 0xa0, 0x00);
            write_secondary(&mut opl, 0xc0, c0);
            write_secondary(&mut opl, 0xb0, 0x20 | (4 << 2) | 0x02);
            let (mut lpk, mut rpk) = (0, 0);
            for _ in 0..256 {
                let (l, r) = opl.render_sample();
                lpk = lpk.max(l.abs());
                rpk = rpk.max(r.abs());
            }
            (lpk, rpk)
        };
        assert!(matches!(peaks(0x11), (l, 0) if l > 1000), "additive, left only");
        assert!(matches!(peaks(0x21), (0, r) if r > 1000), "additive, right only");
        assert!(matches!(peaks(0x31), (l, r) if l > 1000 && r > 1000), "both");
        assert_eq!(peaks(0x01), (0, 0), "no pan bits: silent");
    }

    #[test]
    fn four_op_mode_consumes_the_secondary_channel() {
        // Program channel 3 as a loud keyed tone (operators 6 and 9). Enabling
        // 4-op for pair 0/3 hands those operators to channel 0's 4-op voice,
        // which is unkeyed here, so the previously audible tone goes silent.
        let setup_channel3 = |opl: &mut OplChip| {
            write_secondary(opl, 0x05, 0x01); // OPL3 mode
            opl.write_register(0x2b, 0x21); // op 9 (slot 11): sustained, multiple x1
            opl.write_register(0x4b, 0x00); // op 9 total level 0
            opl.write_register(0x6b, 0xf0); // op 9 attack 15
            opl.write_register(0x8b, 0x00);
            opl.write_register(0xa3, 0x00); // channel 3 f-number low
            opl.write_register(0xc3, 0x31); // additive + left/right
            opl.write_register(0xb3, 0x20 | (4 << 2) | 0x02); // key-on, block 4, fnum 0x200
        };
        let peak = |opl: &mut OplChip| (0..256).map(|_| opl.render_sample().0.abs()).max().unwrap();

        let mut two_op = OplChip::default();
        setup_channel3(&mut two_op);
        assert!(peak(&mut two_op) > 1000, "channel 3 sounds as an independent 2-op");

        let mut four_op = OplChip::default();
        setup_channel3(&mut four_op);
        write_secondary(&mut four_op, 0x04, 0x01); // enable 4-op for pair 0/3
        assert_eq!(peak(&mut four_op), 0, "4-op mode consumes channel 3");
    }

    #[test]
    fn four_op_algorithms_produce_different_timbres() {
        // A keyed 4-op voice on channel 0 with all operators loud; the four
        // connection settings route the operators differently.
        let collect = |cnt1: u8, cnt2: u8| {
            let mut opl = OplChip::default();
            write_secondary(&mut opl, 0x05, 0x01); // OPL3 mode
            write_secondary(&mut opl, 0x04, 0x01); // 4-op for pair 0/3
            for slot in [0, 3, 8, 11] {
                // operators 0, 3, 6, 9: loud, instant attack, multiple x1
                opl.write_register(0x20 + slot, 0x01);
                opl.write_register(0x40 + slot, 0x00);
                opl.write_register(0x60 + slot, 0xf0);
                opl.write_register(0x80 + slot, 0x00);
            }
            opl.write_register(0xa0, 0x00); // channel 0 f-number low
            opl.write_register(0xc0, 0x30 | cnt1); // pan + connection bit 1
            opl.write_register(0xc3, cnt2); // secondary connection bit
            opl.write_register(0xb0, 0x20 | (4 << 2) | 0x02); // key-on, block 4, fnum 0x200
            (0..128).map(|_| opl.render_sample().0).collect::<Vec<_>>()
        };
        let fm_fm = collect(0, 0);
        assert!(fm_fm.iter().any(|&s| s != 0), "the 4-op voice is audible");
        assert_ne!(fm_fm, collect(0, 1), "FM-FM differs from FM-AM");
        assert_ne!(fm_fm, collect(1, 0), "FM-FM differs from AM-FM");
        assert_ne!(fm_fm, collect(1, 1), "FM-FM differs from AM-AM");
    }

    #[test]
    fn tremolo_dips_the_amplitude_when_enabled() {
        // AM on a steady loud carrier swings the peak by ~4.8 dB (a ~1.74x
        // factor) over the 3.7 Hz cycle; without AM the amplitude is constant.
        let peaks = |am: u8, dam: u8| {
            let mut opl = OplChip::default();
            opl.write_register(0x23, 0x20 | am | 0x01); // carrier: sustained (+AM), mult x1
            opl.write_register(0x43, 0x00);
            opl.write_register(0x63, 0xf0); // instant attack
            opl.write_register(0x83, 0x00);
            opl.write_register(0xbd, dam); // tremolo depth (bit7)
            opl.write_register(0xa0, 0x00);
            opl.write_register(0xc0, 0x01); // additive
            opl.write_register(0xb0, 0x20 | (4 << 2) | 0x02); // key-on, block 4, fnum 0x200
            let (mut lo, mut hi) = (i32::MAX, 0);
            for _ in 0..(TREMOLO_PERIOD / 128) {
                let peak = (0..128).map(|_| opl.render_sample().0.abs()).max().unwrap();
                lo = lo.min(peak);
                hi = hi.max(peak);
            }
            (lo, hi)
        };

        let (lo, hi) = peaks(0x80, 0x80); // AM on, DAM=1 (4.8 dB)
        let ratio = f64::from(hi) / f64::from(lo);
        assert!((ratio - 1.74).abs() < 0.2, "expected ~4.8 dB swing, got {ratio}");

        let (lo, hi) = peaks(0x00, 0x80); // AM off
        assert_eq!(lo, hi, "no tremolo without AM");
    }

    #[test]
    fn vibrato_wobbles_the_pitch_when_enabled() {
        // VIB bends the carrier frequency over the 6.1 Hz cycle, so the rendered
        // waveform diverges from a steady-pitch one; without VIB it is identical.
        let render = |vib: u8, dvb: u8| {
            let mut opl = OplChip::default();
            opl.write_register(0x20, vib | 0x01); // modulator: VIB (bit6) + multiple x1
            opl.write_register(0x40, 0x00); // modulator loud
            opl.write_register(0x60, 0xf0); // instant attack
            opl.write_register(0x80, 0x00);
            opl.write_register(0x63, 0x00); // carrier attack 0 -> silent
            opl.write_register(0xbd, dvb); // vibrato depth (bit6)
            opl.write_register(0xc0, 0x01); // additive
            opl.write_register(0xa0, 0x00);
            opl.write_register(0xb0, 0x20 | (4 << 2) | 0x02); // key-on, block 4, fnum 0x200
            (0..4096).map(|_| opl.render_sample().0).collect::<Vec<_>>()
        };
        assert_eq!(render(0x00, 0x40), render(0x00, 0x00), "no vibrato when VIB is off");
        assert_ne!(render(0x40, 0x40), render(0x00, 0x40), "VIB bends the pitch");
    }

    #[test]
    fn vibrato_bends_the_fnumber_to_the_full_step_depth() {
        // One sample of phase advance equals the bent F-number's increment.
        // block 4, multiple x1: increment = fnum << 4. Deep vibrato adds
        // fnum>>7 at the peak (phase 2) and fnum>>8 at the half-steps; the
        // shallow setting is one bit weaker. (Regression: depth was halved.)
        let fnum = 0x200u32;
        let advance = |phase: u8, deep: bool| {
            let mut op = sine_operator(fnum as u16, 4, 0);
            op.set_vibrato(true);
            op.advance_with_lfo(phase, deep);
            op.phase
        };
        assert_eq!(advance(0, true), fnum << 4, "phase 0: no bend");
        assert_eq!(advance(2, true), (fnum + (fnum >> 7)) << 4, "deep peak = +fnum>>7");
        assert_eq!(advance(6, true), (fnum - (fnum >> 7)) << 4, "deep trough = -fnum>>7");
        assert_eq!(advance(1, true), (fnum + (fnum >> 8)) << 4, "deep half-step = +fnum>>8");
        assert_eq!(advance(2, false), (fnum + (fnum >> 8)) << 4, "shallow peak = +fnum>>8");
    }

    #[test]
    fn waveforms_above_three_require_opl3_mode() {
        // E0=6 is a square wave (has negative samples) in OPL3 mode, but masks
        // to waveform 2 (abs sine, non-negative) when the chip is an OPL2.
        let has_negative = |new: bool| {
            let mut opl = OplChip::default();
            opl.write_register(0x01, 0x20); // WSEnable
            if new {
                write_secondary(&mut opl, 0x05, 0x01); // OPL3 mode
            }
            opl.write_register(0x20, 0x01); // modulator multiple x1
            opl.write_register(0x40, 0x00); // modulator loud
            opl.write_register(0x60, 0xf0); // modulator instant attack
            opl.write_register(0x80, 0x00);
            opl.write_register(0x63, 0x00); // carrier attack 0 -> silent
            opl.write_register(0xe0, 0x06); // modulator waveform 6 (square / abs sine)
            opl.write_register(0xc0, 0x31); // additive + left/right (pan ignored as OPL2)
            opl.write_register(0xa0, 0x00);
            opl.write_register(0xb0, 0x20 | (4 << 2) | 0x02); // key-on, block 4, fnum 0x200
            (0..256).any(|_| opl.render_sample().0 < 0)
        };
        assert!(!has_negative(false), "OPL2 masks waveform 6 to abs sine");
        assert!(has_negative(true), "OPL3 waveform 6 is a square wave");
    }

    #[test]
    fn waveform_select_requires_wsenable() {
        // A half-sine (E0=1) silences the wave's negative half, but only when
        // WSEnable (reg 0x01 bit5) is set; otherwise the chip forces a full sine.
        let has_negative = |wse: u8| {
            let mut opl = OplChip::default();
            opl.write_register(0x01, wse); // 0x20 = WSEnable, 0x00 = off
            opl.write_register(0x20, 0x01); // modulator multiple x1
            opl.write_register(0x40, 0x00); // modulator loud
            opl.write_register(0x60, 0xf0); // modulator instant attack
            opl.write_register(0x80, 0x00);
            opl.write_register(0x63, 0x00); // carrier attack 0 -> stays silent
            opl.write_register(0xe0, 0x01); // modulator waveform: half-sine
            opl.write_register(0xc0, 0x01); // additive
            opl.write_register(0xa0, 0x00);
            opl.write_register(0xb0, 0x20 | (4 << 2) | 0x02); // key-on, block 4, fnum 0x200
            (0..256).any(|_| opl.render_sample().0 < 0)
        };
        assert!(has_negative(0x00), "WSEnable off forces a full sine (has negatives)");
        assert!(!has_negative(0x20), "WSEnable on lets half-sine silence negatives");
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
