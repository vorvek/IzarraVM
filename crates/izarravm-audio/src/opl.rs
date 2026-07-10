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
/// a right shift. The `<< 1` makes this the full 13-bit operator output
/// (`exp_lookup(0)` ~= 4084), matching the chip; the modulation depth in FM
/// depends on this absolute scale.
pub(crate) fn exp_lookup(attenuation: u32) -> i32 {
    let fraction = (attenuation & 0xff) as usize;
    let shift = attenuation >> 8;
    if shift >= 32 {
        return 0; // attenuated past audibility (and past a valid i32 shift)
    }
    ((i32::from(EXP[fraction ^ 0xff]) + 1024) << 1) >> shift
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
/// was derived from the YMF262's documented behavior and cross-checked against
/// a reference implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Operator {
    phase: u32,
    fnum: u16,
    block: u8,
    multiple: u8,
    waveform: u8,
    total_level: u8,
    key_scale_level: u8, // KSL: 0 = off, else 1.5/3/6 dB per octave of pitch
    feedback: u8,        // FB factor for operator-1 self-modulation (0 = off)
    feedback_history: [i32; 2], // last two outputs; averaged for stable feedback
    tremolo: bool,       // AM: this operator follows the amplitude LFO
    vibrato: bool,       // VIB: this operator follows the pitch LFO
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
    /// phase units (1024 = 2*pi) full depth (4*pi = 2048) is half the full-scale
    /// 13-bit output, so the average is shifted by `9 - FB` (one bit for the /2
    /// average, one for the half).
    pub(crate) fn render_feedback(&mut self, extra_attenuation: u16) -> i32 {
        let modulation = if self.feedback == 0 {
            0
        } else {
            (self.feedback_history[0] + self.feedback_history[1]) >> (9 - self.feedback)
        };
        let out = self.sample_modulated(modulation, extra_attenuation);
        self.feedback_history = [out, self.feedback_history[0]];
        out
    }

    /// Rhythm-mode percussion output: a full-scale square whose sign is the
    /// `positive` bit (driven by the noise LFSR and/or the metallic phase mix),
    /// scaled by the operator's level and envelope. Used by the snare, hi-hat
    /// and cymbal, whose exact hardware bit-logic is unpublished.
    fn percussion_sample(&self, extra_attenuation: u16, positive: bool) -> i32 {
        let attenuation = u32::from(self.total_level) * 32
            + u32::from(self.ksl_attenuation())
            + u32::from(extra_attenuation);
        let magnitude = exp_lookup(attenuation);
        if positive { magnitude } else { -magnitude }
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

/// Rhythm-mode operator slots (datasheet p11): bass drum is the 2-op pair
/// 12->15 on channel 6; hi-hat (13) and snare (16) share channel 7; tom-tom
/// (14) and cymbal (17) share channel 8.
const RHYTHM_BD_MOD: usize = 12;
const RHYTHM_BD_CAR: usize = 15;
const RHYTHM_HH: usize = 13;
const RHYTHM_TT: usize = 14;
const RHYTHM_SD: usize = 16;
const RHYTHM_CY: usize = 17;

/// Enharmonic toggle for the hi-hat / cymbal, mixing high bits of the two
/// operators' wave positions. Clean-room approximation: the hardware's exact
/// metallic phase logic is not published in the datasheet or programmer's guide.
fn metal_bit(phase_hh: u32, phase_cy: u32) -> bool {
    let a = (phase_hh >> 10) & 0x3ff;
    let b = (phase_cy >> 10) & 0x3ff;
    ((a >> 8) ^ (a >> 3) ^ (b >> 7) ^ (b >> 2)) & 1 != 0
}

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
    /// Maximal-length 16-bit LFSR feeding the rhythm-mode noise instruments.
    noise: u16,
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

    /// Return whether `self.expired` would be true after
    /// `micros_elapsed` more microseconds of chip time, without mutating
    /// `self`? `expired` is sticky (only a register-0x04 bit7 write clears it,
    /// via `OplChip::write_bank`), so the answer is `self.expired` already, OR
    /// -- when running -- whether the count would cross 0xff within
    /// `micros_elapsed`. Mirrors `advance`'s exact step arithmetic
    /// (accumulated_us + micros, divided by step_us, whole steps added to
    /// count) without the loop: since only the CROSSING matters (not the
    /// reload value, which `expired_after` never needs), this is one division
    /// instead of `advance`'s per-step subtraction loop.
    fn expired_after(&self, micros_elapsed: u64) -> bool {
        if self.expired {
            return true;
        }
        if !self.running {
            return false;
        }
        // Saturating u64 arithmetic keeps the function total: a narrowing
        // `steps as u32` would wrap for steps >= 2^32 (~4 days of guest time in
        // one peek, unreachable under the ~1 ms batch cap, but saturation costs
        // nothing), and saturation can only ever err toward `true`, which the
        // sticky-expired semantics make the correct limit answer anyway.
        let total_us = self.accumulated_us.saturating_add(micros_elapsed);
        let steps = total_us / self.step_us;
        u64::from(self.count).saturating_add(steps) > 0xff
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
            noise: 1,
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
        let up = if pos < half {
            pos
        } else {
            TREMOLO_PERIOD - pos
        };
        let peak = if self.registers[0][0xbd] & 0x80 != 0 {
            204
        } else {
            43
        };
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

    /// Rhythm/percussion mode (reg 0xBD bit5): channels 6-8 become the five
    /// percussion instruments instead of melodic voices.
    fn rhythm_enabled(&self) -> bool {
        self.registers[0][0xbd] & 0x20 != 0
    }

    /// Step the noise LFSR one sample (maximal-length 16-bit Galois polynomial).
    fn advance_noise(&mut self) {
        let feedback = self.noise & 1 != 0;
        self.noise >>= 1;
        if feedback {
            self.noise ^= 0xb400;
        }
    }

    /// Render the five percussion instruments and sum them into `(left, right)`.
    /// All operators take their pitch from channels 6-8 but are keyed by the
    /// 0xBD on-bits, not the channel KEY-ON. Bass drum is a normal 2-op FM voice;
    /// tom-tom is a plain tone; snare/hi-hat/cymbal are noise + metallic squares.
    fn render_rhythm(&mut self) -> (i32, i32) {
        let note_select = self.registers[0][0x08] & 0x40 != 0;
        let bd = self.registers[0][0xbd];
        let noise = self.noise & 1 != 0;
        let ops = [
            RHYTHM_BD_MOD,
            RHYTHM_BD_CAR,
            RHYTHM_HH,
            RHYTHM_TT,
            RHYTHM_SD,
            RHYTHM_CY,
        ];

        // Bass drum operators belong to channel 6, hi-hat/snare to 7, the rest
        // to 8. Load each from its channel, then override the key from 0xBD.
        for op in ops {
            let channel = 6 + (op - RHYTHM_BD_MOD) % 3;
            self.load_operator(op, channel);
        }
        self.operators[RHYTHM_BD_MOD].set_feedback((self.registers[0][0xc6] >> 1) & 0x07);
        self.operators[RHYTHM_BD_MOD].set_key(bd & 0x10 != 0);
        self.operators[RHYTHM_BD_CAR].set_key(bd & 0x10 != 0);
        self.operators[RHYTHM_HH].set_key(bd & 0x01 != 0);
        self.operators[RHYTHM_SD].set_key(bd & 0x08 != 0);
        self.operators[RHYTHM_TT].set_key(bd & 0x04 != 0);
        self.operators[RHYTHM_CY].set_key(bd & 0x02 != 0);
        for op in ops {
            self.operators[op].advance_envelope(self.eg_counter, note_select);
        }

        // Bass drum: 2-op FM (op12 -> op15), additive per channel 6's bit0.
        let bd_mod_att = self.operator_attenuation(RHYTHM_BD_MOD);
        let bd_car_att = self.operator_attenuation(RHYTHM_BD_CAR);
        let bd_mod_out = self.operators[RHYTHM_BD_MOD].render_feedback(bd_mod_att);
        let bass = if self.registers[0][0xc6] & 0x01 != 0 {
            self.operators[RHYTHM_BD_CAR].sample(bd_car_att)
        } else {
            self.operators[RHYTHM_BD_CAR].sample_modulated(bd_mod_out, bd_car_att)
        };

        // Tom-tom: a plain tone.
        let tom = self.operators[RHYTHM_TT].sample(self.operator_attenuation(RHYTHM_TT));

        // Snare/hi-hat/cymbal: full-scale squares toggled by the noise LFSR and
        // the metallic phase mix (clean-room approximation, see `metal_bit`).
        let metal = metal_bit(
            self.operators[RHYTHM_HH].phase,
            self.operators[RHYTHM_CY].phase,
        );
        let snare_bit = ((self.operators[RHYTHM_SD].phase >> 19) & 1 != 0) ^ noise;
        let snare = self.operators[RHYTHM_SD]
            .percussion_sample(self.operator_attenuation(RHYTHM_SD), snare_bit);
        let hihat = self.operators[RHYTHM_HH]
            .percussion_sample(self.operator_attenuation(RHYTHM_HH), metal ^ noise);
        let cymbal = self.operators[RHYTHM_CY]
            .percussion_sample(self.operator_attenuation(RHYTHM_CY), metal);

        let (vibrato, deep) = (self.vibrato_phase(), self.deep_vibrato());
        for op in ops {
            self.operators[op].advance_with_lfo(vibrato, deep);
        }

        // Pan each instrument by its source channel (6 = BD, 7 = HH/SD, 8 = TT/CY).
        let (mut left, mut right) = (0, 0);
        for (out, channel) in [(bass, 6), (hihat, 7), (snare, 7), (tom, 8), (cymbal, 8)] {
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

    /// Render one stereo `(left, right)` sample at the chip's native 49716 Hz
    /// rate. OPL3 mode sums all 18 two-op channels; otherwise the 9 OPL2
    /// channels. Rhythm mode renders channels 6-8 as percussion, and 4-op mode
    /// pairs operators across channel pairs. The EG counter ticks per sample.
    pub fn render_sample(&mut self) -> (i32, i32) {
        self.eg_counter = self.eg_counter.wrapping_add(1);
        self.advance_noise();
        let channels = if self.opl3_enabled() { 18 } else { 9 };
        let rhythm = self.rhythm_enabled();
        let mask = self.four_op_mask();
        let (mut left, mut right) = (0, 0);
        for channel in 0..channels {
            if rhythm && (6..=8).contains(&channel) {
                continue; // channels 6-8 are rendered as percussion below
            }
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
        if rhythm {
            let (l, r) = self.render_rhythm();
            left += l;
            right += r;
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
    ///
    /// Composed of `status_bits` (the pure bit computation) off the live
    /// timers' `expired` flags. The lazy port-read path (`MachineBus::
    /// read_io`, Approximate timing class) calls the same pure function with
    /// timer `expired_after` peeks instead of the live flags, so both callers
    /// share exactly one bit-composition implementation (the `Vga::
    /// status1_bits` precedent).
    pub fn status(&self) -> u8 {
        self.status_bits(self.timer1.expired, self.timer2.expired)
    }

    /// Lazy-path status byte for the Approximate timing class:
    /// the OPL status byte `micros_elapsed` microseconds of chip time from now,
    /// without stepping either timer. Peeks each timer's `expired_after`
    /// instead of reading the live `expired` flag, then composes the
    /// result through the same `status_bits` the live `status()` call uses, so
    /// the two paths can never structurally diverge in bit logic -- only in
    /// which `expired` inputs they feed it.
    pub fn status_after(&self, micros_elapsed: u64) -> u8 {
        let t1_expired = self.timer1.expired_after(micros_elapsed);
        let t2_expired = self.timer2.expired_after(micros_elapsed);
        self.status_bits(t1_expired, t2_expired)
    }

    /// Peek at timer 1's `expired_after`. Exposed for
    /// the machine crate's carry-pinning differential test, which needs to
    /// locate an overflow step boundary without reimplementing `Timer`'s step
    /// arithmetic; NOT `#[cfg(test)]` because that test lives in a downstream
    /// crate (`izarravm-machine`), where a `cfg(test)` item in this crate's
    /// non-dev dependency graph would not exist to link against. Not part of
    /// the chip's production API, same precedent as `envelope_level` above.
    #[doc(hidden)]
    pub fn timer1_expired_after(&self, micros_elapsed: u64) -> bool {
        self.timer1.expired_after(micros_elapsed)
    }

    /// Pure bit computation for the OPL status byte, off caller-supplied
    /// timer-expired flags instead of the live `self.timer1`/`self.timer2`.
    /// The mask bits (register 0x04 bits 6/5) come from `self.registers`,
    /// which only a write can change; a write always ends the batch (Task
    /// 3.2), so a mid-batch predicted status read can never observe a mask
    /// mid-batch write races with -- the mask is exactly as batch-entry-stable
    /// here as it is for the live `status()` call above.
    fn status_bits(&self, t1_expired: bool, t2_expired: bool) -> u8 {
        let control = self.registers[0][0x04];
        let t1_irq = t1_expired && control & 0x40 == 0;
        let t2_irq = t2_expired && control & 0x20 == 0;
        ((t1_irq || t2_irq) as u8) << 7 | (t1_expired as u8) << 6 | (t2_expired as u8) << 5
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
#[path = "opl_test.rs"]
mod tests;
