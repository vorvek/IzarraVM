//! Yamaha ADPCM codec and chip device.
//!
//! This ports the encoder/decoder algorithms from the public-domain
//! `superctr/adpcm` sound-chip ADPCM library (Ian Karlsson, 2019, Unlicense):
//! Yamaha ADPCM-A (YM2610), Yamaha ADPCM-B (Y8950/YM2608/YM2610), and
//! Yamaha/Creative YMZ280B / AICA. The arithmetic of every `*_step` core is a
//! faithful 1:1 translation of the C reference; the surrounding Rust state
//! structs, streaming chip model, and port/register interface are clean-room.
//!
//! The chip exposed to the machine is [`YamahaAdpcmChip`], a streaming ADPCM-B
//! DAC the way the Y8950/YM2608 ADPCM-B channel is: the guest programs a
//! sample rate, a 4-bit ADPCM format, a stereo mode, and a block length, then
//! streams ADPCM nibbles (over host DMA or a direct data port). The chip
//! decodes them to centered signed 16-bit PCM frames a mixer drains, and edges
//! half/end-buffer interrupts exactly like the Sound Blaster DSP does, so a DOS
//! driver that programs it sees the same DMA/IRQ handshake the real Yamaha
//! hardware gives a host.

use std::collections::VecDeque;

/// Bounded length of the rendered-frame ring, in stereo frames. Mirrors the SB
/// DSP / AD1848 rings: a rate-match buffer between the per-CPU-clock producer
/// and the host drainer; on push when full it drops the oldest frame so the
/// block counter and IRQ timing stay correct even if audio fidelity glitches.
const ADPCM_RING_CAP: usize = 8192;

/// Initial ADPCM step size shared by every Yamaha ADPCM variant: 127.
const INITIAL_STEP_SIZE: i32 = 127;

// ===========================================================================
//  Codec library: faithful ports of the superctr/adpcm Yamaha codecs.
// ===========================================================================

/// Yamaha ADPCM-B (Y8950/YM2608/YM2610) step multiplier table indexed by the
/// 3-bit magnitude nibble. 1:1 with `ymb_codec.c::step_table`.
const YMB_STEP_TABLE: [i32; 8] = [57, 57, 57, 57, 77, 102, 128, 153];

/// Yamaha YMZ280B / AICA step multiplier table indexed by the 3-bit magnitude
/// nibble. 1:1 with `ymz_codec.c::step_table`.
const YMZ_STEP_TABLE: [i32; 8] = [230, 230, 230, 230, 307, 409, 512, 614];

/// Yamaha ADPCM-A (YM2610) IMA-style index step table. 1:1 with
/// `yma_codec.c::yma_step_table`.
const YMA_STEP_TABLE: [u16; 49] = [
    16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107, 118, 130,
    143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796,
    876, 963, 1060, 1166, 1282, 1411, 1552,
];

/// 4-bit Yamaha ADPCM-A delta codes (the full nibble).
const YMA_DELTA_TABLE: [i8; 16] = [1, 3, 5, 7, 9, 11, 13, 15, -1, -3, -5, -7, -9, -11, -13, -15];

/// Index adjust table for Yamaha ADPCM-A (keyed on the low 3 magnitude bits).
/// 1:1 with `yma_codec.c::adjust_table`.
const YMA_ADJUST_TABLE: [i8; 8] = [-1, -1, -1, -1, 2, 5, 7, 9];

/// The four supported 4-bit ADPCM formats, mirroring the Yamaha codecs in the
/// reference library. ADPCM-B is the chip's native format; the others are
/// selectable so a guest can stream any of the Yamaha ADPCM variants the
/// reference encodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AdpcmFormat {
    /// Yamaha ADPCM-B (Y8950/YM2608/YM2610). The chip's native format.
    #[default]
    AdpcmB,
    /// Yamaha ADPCM-A (YM2610), IMA-style 12-bit delta.
    AdpcmA,
    /// Yamaha/Creative YMZ280B 4-bit ADPCM (high nibble first).
    Ymz280B,
    /// Yamaha AICA 4-bit ADPCM (low nibble first).
    Aica,
}

/// Decoder/encoder working state shared by the ADPCM-B and YMZ/AICA variants:
/// the running `history` (predicted sample) and `step_size` (adaptive scale).
/// Reset to `(0, 127)` at the start of every stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YmAdpcmState {
    history: i16,
    step_size: i16,
}

impl Default for YmAdpcmState {
    fn default() -> Self {
        Self {
            history: 0,
            step_size: INITIAL_STEP_SIZE as i16,
        }
    }
}

impl YmAdpcmState {
    /// One ADPCM-B decode step for a sign-extended 4-bit code. 1:1 port of
    /// `ymb_codec.c::ymb_step` minus the C-isms: the magnitude table is
    /// [`YMB_STEP_TABLE`], the scale shifts are `>>3` (diff) and `>>6` (next
    /// step), and the step size clamps to `[127, 24576]`.
    pub fn step_b(&mut self, code: i8) -> i16 {
        let sign = code & 8;
        let delta = (code & 7) as i32;
        let step_size = i32::from(self.step_size);
        let diff = ((1 + (delta << 1)) * step_size) >> 3;
        let nstep = (YMB_STEP_TABLE[delta as usize] * step_size) >> 6;
        let mut newval = i32::from(self.history);
        if sign > 0 {
            newval -= diff;
        } else {
            newval += diff;
        }
        self.step_size = nstep.clamp(127, 24576) as i16;
        newval = newval.clamp(-32768, 32767);
        self.history = newval as i16;
        self.history
    }

    /// One YMZ280B/AICA decode step for a sign-extended 4-bit code. 1:1 port of
    /// `ymz_codec.c::ymz_step` minus the C-isms: uses [`YMZ_STEP_TABLE`], the
    /// diff is clamped to `[0, 32767]` before sign application (the official
    /// AICA encoder behaviour the reference notes), shifts are `>>3` / `>>8`.
    pub fn step_z(&mut self, code: i8) -> i16 {
        let sign = code & 8;
        let delta = (code & 7) as i32;
        let step_size = i32::from(self.step_size);
        let mut diff = ((1 + (delta << 1)) * step_size) >> 3;
        let nstep = (YMZ_STEP_TABLE[delta as usize] * step_size) >> 8;
        diff = diff.clamp(0, 32767);
        let mut newval = i32::from(self.history);
        if sign > 0 {
            newval -= diff;
        } else {
            newval += diff;
        }
        self.step_size = nstep.clamp(127, 24576) as i16;
        newval = newval.clamp(-32768, 32767);
        self.history = newval as i16;
        self.history
    }
}

/// Yamaha ADPCM-A (YM2610) decoder state. ADPCM-A keeps a 12-bit `history` and
/// a 0..49 `step_hist` index into [`YMA_STEP_TABLE`], distinct from the
/// B/YMZ predictor+scale pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct YmaAdpcmState {
    history: i16,
    step_hist: u8,
}

impl YmaAdpcmState {
    /// One ADPCM-A decode step for a 4-bit code. 1:1 port of
    /// `yma_codec.c::yma_step`: the 12-bit history is computed with wrapping
    /// addition (no saturation in the predictor), sign-extended from bit 11,
    /// and the index is adjusted by [`YMA_ADJUST_TABLE`] and clamped to 0..=48.
    /// Returns the decoded 12-bit sample (the caller shifts it into 16-bit).
    pub fn step(&mut self, code: u8) -> i16 {
        let step_size = i32::from(YMA_STEP_TABLE[self.step_hist as usize]);
        let delta = i32::from(YMA_DELTA_TABLE[(code & 15) as usize]) * step_size / 8;
        let mut out = (i32::from(self.history) + delta) & 0xfff;
        if out & 0x800 != 0 {
            out |= !0xfff; // sign-extend the 12-bit value
        }
        self.history = out as i16;
        let adjusted = self.step_hist as i8 + YMA_ADJUST_TABLE[(code & 7) as usize];
        self.step_hist = adjusted.clamp(0, 48) as u8;
        self.history
    }
}

/// Decode a full ADPCM-B nibble stream (`len` nibbles packed into `len/2` bytes,
/// high nibble first) into centered signed 16-bit samples. 1:1 port of
/// `ymb_codec.c::ymb_decode`.
pub fn decode_adpcm_b(bytes: &[u8], out: &mut [i16]) {
    decode_packed_high(bytes, out, |state, code| state.step_b(code));
}

/// Decode a full YMZ280B nibble stream (high nibble first), with the per-sample
/// high-pass `history * 254/256` the reference applies. 1:1 port of
/// `ymz_codec.c::ymz_decode`.
pub fn decode_ymz280b(bytes: &[u8], out: &mut [i16]) {
    decode_packed_high(bytes, out, |state, code| {
        state.history = (i32::from(state.history) * 254 / 256) as i16;
        state.step_z(code)
    });
}

/// Decode a full Yamaha ADPCM-A nibble stream (high nibble first), shifting the
/// 12-bit predictor up to 16-bit as `yma_decode` does (`<< 4`). 1:1 port of
/// `yma_codec.c::yma_decode`.
pub fn decode_adpcm_a(bytes: &[u8], out: &mut [i16]) {
    // ADPCM-A keeps a separate predictor+index state type, so it runs its own
    // high-nibble-first loop (1:1 with `yma_codec.c::yma_decode`).
    let mut state = YmaAdpcmState::default();
    let mut nibble = 0u8;
    let mut ptr = 0usize;
    let count = out.len().min(bytes.len() * 2);
    for slot in out.iter_mut().take(count) {
        let mut step = (bytes[ptr] as i8) << nibble;
        step >>= 4;
        if nibble != 0 {
            ptr += 1;
        }
        nibble ^= 4;
        *slot = state.step(step as u8) << 4;
    }
}

/// Decode a full AICA nibble stream (low nibble first), with the per-sample
/// high-pass the reference applies. 1:1 port of `ymz_codec.c::aica_decode`.
pub fn decode_aica(bytes: &[u8], out: &mut [i16]) {
    let mut state = YmAdpcmState::default();
    let mut nibble = 4u8;
    let mut ptr = 0usize;
    let count = out.len().min(bytes.len() * 2);
    for slot in out.iter_mut().take(count) {
        let mut step = (bytes[ptr] as i8) << nibble;
        step >>= 4;
        if nibble == 0 {
            ptr += 1;
        }
        nibble ^= 4;
        state.history = (i32::from(state.history) * 254 / 256) as i16;
        *slot = state.step_z(step);
    }
}

/// Shared high-nibble-first packed decoder for the B/YMZ variants, which differ
/// only in their per-step closure. Emits `bytes.len() * 2` samples (capped by
/// `out.len()`).
fn decode_packed_high(
    bytes: &[u8],
    out: &mut [i16],
    mut step_fn: impl FnMut(&mut YmAdpcmState, i8) -> i16,
) {
    let mut state = YmAdpcmState::default();
    let mut nibble = 0u8;
    let mut ptr = 0usize;
    let count = out.len().min(bytes.len() * 2);
    for slot in out.iter_mut().take(count) {
        let mut step = (bytes[ptr] as i8) << nibble;
        step >>= 4;
        if nibble != 0 {
            ptr += 1;
        }
        nibble ^= 4;
        *slot = step_fn(&mut state, step);
    }
}

/// Encode a signed 16-bit sample stream into packed ADPCM-B nibbles. 1:1 port
/// of `ymb_codec.c::ymb_encode`.
pub fn encode_adpcm_b(samples: &[i16], out: &mut [u8]) {
    let mut state = YmAdpcmState::default();
    let mut buf_sample = 0u8;
    let mut nibble = 0u8;
    let mut ptr = 0usize;
    for &sample in samples {
        let step = ((sample & -8) as i32) - i32::from(state.history);
        // The C reference computes `(abs(step)<<16)/(step_size<<14)` in 32-bit
        // int; both operands are non-negative, so the result equals unsigned
        // division. Mirror that in u32 to match the reference wrap on a large
        // sample delta.
        let denom = (state.step_size as u32) << 14;
        let mut adpcm_sample = (step.unsigned_abs() << 16) / denom;
        adpcm_sample = adpcm_sample.min(7);
        if step < 0 {
            adpcm_sample |= 8;
        }
        if nibble != 0 {
            out[ptr] = buf_sample | (adpcm_sample as u8 & 15);
            ptr += 1;
        } else {
            buf_sample = (adpcm_sample as u8 & 15) << 4;
        }
        nibble ^= 1;
        state.step_b(adpcm_sample as i8);
    }
}

/// Decode a single 4-bit ADPCM code in the given format, advancing the matching
/// predictor state. `state` carries the B/YMZ predictor; `state_a` carries the
/// ADPCM-A predictor+index. Only the one matching the format is touched.
pub(crate) fn decode_one(
    format: AdpcmFormat,
    code: i8,
    state: &mut YmAdpcmState,
    state_a: &mut YmaAdpcmState,
) -> i16 {
    match format {
        AdpcmFormat::AdpcmB => state.step_b(code),
        AdpcmFormat::Ymz280B => {
            state.history = (i32::from(state.history) * 254 / 256) as i16;
            state.step_z(code)
        }
        AdpcmFormat::Aica => {
            state.history = (i32::from(state.history) * 254 / 256) as i16;
            state.step_z(code)
        }
        AdpcmFormat::AdpcmA => state_a.step(code as u8) << 4,
    }
}

// ===========================================================================
//  Chip device: a streaming Yamaha ADPCM-B DAC channel.
// ===========================================================================

/// Number of I/O ports the chip decodes at `AdpcmConfig.base` (4 ports).
pub const ADPCM_PORT_COUNT: u16 = 4;

/// Default I/O base. 0x240 is free on the Izarra 3000 (the SB16 window is
/// 0x220-0x22F; the WSS window is 0x530-0x537), so the ADPCM DAC lives in the
/// classic free region a real sound-card add-on used.
pub const ADPCM_DEFAULT_BASE: u16 = 0x0240;

/// Board IRQ/DMA wiring carried on the chip so a guest driver can read back its
/// resources (mirrors the AD1848 config region). Defaults avoid the SB16 (IRQ5/
/// DMA1) and WSS (IRQ7/DMA0) lines so all three sound paths run concurrently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdpcmConfig {
    pub enabled: bool,
    pub base: u16,
    pub irq: u8,
    pub dma: u8,
}

impl Default for AdpcmConfig {
    fn default() -> Self {
        // IRQ10 / DMA3: both free of the SB16 and WSS defaults, so the three
        // audio paths never contend on a PIC/DMA line.
        Self {
            enabled: true,
            base: ADPCM_DEFAULT_BASE,
            irq: 10,
            dma: 3,
        }
    }
}

/// Internal register indices (programmed via the address/data latch).
mod reg {
    pub const CONTROL: u8 = 0;
    pub const RATE_LOW: u8 = 1;
    pub const RATE_HIGH: u8 = 2;
    pub const FORMAT: u8 = 3;
    pub const COUNT_LOW: u8 = 4;
    pub const COUNT_HIGH: u8 = 5;
    pub const VOLUME: u8 = 6;
}

/// Status byte bits.
const STATUS_PLAYING: u8 = 0x01;
const STATUS_AUTO_INIT: u8 = 0x02;
const STATUS_IRQ: u8 = 0x80;

/// Control register bits.
const CONTROL_START: u8 = 0x01;
const CONTROL_AUTO_INIT: u8 = 0x02;
const CONTROL_RESET: u8 = 0x04;

/// Per-channel decode state: the active ADPCM predictor plus the high/low
/// nibble packing over the byte stream. One byte yields two samples: the high
/// nibble first, then the buffered low nibble.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct ChannelState {
    state: YmAdpcmState,
    state_a: YmaAdpcmState,
    /// The byte whose high nibble was just decoded; the low nibble is pending.
    pending_low: Option<i8>,
}

impl ChannelState {
    fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Decode one sample for `ch`: on the high phase pull a byte from `fetch`,
/// decode its high nibble, and buffer the low nibble for the next call; on the
/// low phase decode the buffered nibble. Returns None only when a fetch is
/// needed and the source yields nothing (DMA/FIFO starved). A free function (not
/// a method) so the caller can keep the byte source borrowing disjoint from the
/// channel's predictor fields.
fn decode_channel(
    ch: &mut ChannelState,
    format: AdpcmFormat,
    fetch: &mut impl FnMut() -> Option<u8>,
) -> Option<i16> {
    if let Some(low) = ch.pending_low.take() {
        return Some(decode_one(format, low, &mut ch.state, &mut ch.state_a));
    }
    let byte = fetch()?;
    let hi = ((byte as i8) >> 4) & 0x0F;
    let lo = (byte as i8) & 0x0F;
    ch.pending_low = Some(lo);
    Some(decode_one(format, hi, &mut ch.state, &mut ch.state_a))
}

/// One Yamaha ADPCM streaming DAC channel.
///
/// Guest programming model (AdLib-style address/data latch at the chip base):
/// - write the register index to the address port,
/// - write its value to the data port,
/// - then either stream ADPCM bytes through host DMA (the producer pulls a byte
///   per two decoded samples per channel) or push them directly to the data FIFO
///   port.
///
/// The chip decodes nibbles into centered signed 16-bit stereo frames at the
/// programmed sample rate, edges half/end-buffer interrupts like the SB DSP, and
/// buffers them in a ring the host mixer drains.
#[derive(Debug, Clone, PartialEq)]
pub struct YamahaAdpcmChip {
    config: AdpcmConfig,
    /// AdLib-style address latch for indirect register writes.
    address: u8,
    /// Indirect register file (only the documented indices are used).
    regs: [u8; 8],
    left: ChannelState,
    right: ChannelState,
    /// Direct-write ADPCM data FIFO (the non-DMA streaming path).
    fifo: VecDeque<u8>,
    /// Programmed per-channel output rate in Hz.
    rate_hz: u32,
    /// Selected 4-bit ADPCM format.
    format: AdpcmFormat,
    /// Whether each output frame consumes two nibbles (left then right).
    stereo: bool,
    /// Per-channel volume scalar (0..=255 maps to 0..=1.0 linear gain).
    volume: u8,
    /// Block length, in ADPCM nibbles (one nibble = one decoded sample).
    block_size: u32,
    block_remaining: u32,
    auto_init: bool,
    playing: bool,
    half_reached: bool,
    irq_pending: bool,
    /// Rendered stereo frames, drained by the host audio path.
    rendered: VecDeque<(i16, i16)>,
}

impl Default for YamahaAdpcmChip {
    fn default() -> Self {
        Self::new(AdpcmConfig::default())
    }
}

impl YamahaAdpcmChip {
    /// Build a chip with the given board IRQ/DMA/base wiring.
    pub fn new(config: AdpcmConfig) -> Self {
        Self {
            config,
            address: 0,
            regs: [0; 8],
            left: ChannelState::default(),
            right: ChannelState::default(),
            fifo: VecDeque::new(),
            rate_hz: 11_025,
            format: AdpcmFormat::AdpcmB,
            stereo: false,
            volume: 255,
            block_size: 0,
            block_remaining: 0,
            auto_init: false,
            playing: false,
            half_reached: false,
            irq_pending: false,
            rendered: VecDeque::new(),
        }
    }

    pub fn config(&self) -> AdpcmConfig {
        self.config
    }

    /// The chip's four-port window `[base, base + 4)`.
    pub const fn window(&self) -> (u16, u16) {
        (
            self.config.base,
            self.config.base.saturating_add(ADPCM_PORT_COUNT),
        )
    }

    pub fn rate_hz(&self) -> u32 {
        self.rate_hz
    }

    /// Per-channel output frame rate (the programmed rate).
    pub fn output_frame_rate(&self) -> u32 {
        self.rate_hz
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn is_stereo(&self) -> bool {
        self.stereo
    }

    pub fn format(&self) -> AdpcmFormat {
        self.format
    }

    /// In-region port offset decode: returns Some(offset) when `port` falls in
    /// the chip's four-port window and the chip is enabled.
    pub fn offset(&self, port: u16) -> Option<u16> {
        if !self.config.enabled {
            return None;
        }
        let (start, end) = self.window();
        if (start..end).contains(&port) {
            Some(port - start)
        } else {
            None
        }
    }

    /// Read one of the four device ports by in-region offset:
    /// - 0: status (playing / auto-init / IRQ),
    /// - 1: address latch echo,
    /// - 2: resource readback (low nibble DMA, high nibble IRQ),
    /// - 3: data FIFO occupancy flag (bit6 = at least one byte queued).
    pub fn read_port(&mut self, offset: u16) -> u8 {
        match offset {
            0 => {
                let mut s = 0u8;
                if self.playing {
                    s |= STATUS_PLAYING;
                }
                if self.auto_init {
                    s |= STATUS_AUTO_INIT;
                }
                if self.irq_pending {
                    s |= STATUS_IRQ;
                }
                s
            }
            1 => self.address,
            2 => ((self.config.irq & 0x0F) << 4) | (self.config.dma & 0x0F),
            3 => {
                if !self.fifo.is_empty() {
                    0x40
                } else {
                    0x00
                }
            }
            _ => 0xFF,
        }
    }

    /// Write one of the four device ports by in-region offset:
    /// - 0: address latch (indirect register index),
    /// - 1: data (write the latched register),
    /// - 2: command (alias of the control register for fast start/reset),
    /// - 3: ADPCM data byte (pushes to the direct-write FIFO).
    pub fn write_port(&mut self, offset: u16, value: u8) {
        match offset {
            0 => self.address = value & 0x07,
            1 => self.write_register(self.address, value),
            2 => self.write_register(reg::CONTROL, value),
            3 if self.fifo.len() < ADPCM_RING_CAP * 2 => {
                self.fifo.push_back(value);
            }
            _ => {}
        }
    }

    /// Write an indirect register, applying side effects. The control register
    /// arms/halts/resets playback; the format register selects the codec and
    /// stereo; the count registers program the block length in nibbles.
    pub fn write_register(&mut self, index: u8, value: u8) {
        let idx = (index & 0x07) as usize;
        self.regs[idx] = value;
        match idx as u8 {
            reg::CONTROL => {
                if value & CONTROL_RESET != 0 {
                    self.reset_playback();
                } else if value & CONTROL_START != 0 {
                    self.arm();
                } else {
                    // Clearing START halts playback but keeps the buffer position.
                    self.playing = false;
                }
            }
            reg::RATE_LOW | reg::RATE_HIGH => {
                let lo = u32::from(self.regs[reg::RATE_LOW as usize]);
                let hi = u32::from(self.regs[reg::RATE_HIGH as usize]);
                self.rate_hz = (hi << 8) | lo;
            }
            reg::FORMAT => {
                self.format = match value & 0x0F {
                    1 => AdpcmFormat::AdpcmA,
                    2 => AdpcmFormat::Ymz280B,
                    3 => AdpcmFormat::Aica,
                    _ => AdpcmFormat::AdpcmB,
                };
                self.stereo = value & 0x10 != 0;
            }
            reg::COUNT_LOW | reg::COUNT_HIGH => {
                let lo = u32::from(self.regs[reg::COUNT_LOW as usize]);
                let hi = u32::from(self.regs[reg::COUNT_HIGH as usize]);
                self.block_size = ((hi << 8) | lo) + 1;
            }
            reg::VOLUME => self.volume = value,
            _ => {}
        }
    }

    /// Arm playback from the latched rate/format/count/control registers. The
    /// ADPCM predictor is reset to the stream start state, as the real Yamaha
    /// chips reset their predictor at key-on.
    fn arm(&mut self) {
        self.left.reset();
        self.right.reset();
        self.auto_init = self.regs[reg::CONTROL as usize] & CONTROL_AUTO_INIT != 0;
        self.block_remaining = self.block_size.max(1);
        self.half_reached = false;
        self.irq_pending = false;
        self.playing = true;
    }

    /// Hard reset of the playback engine (the CONTROL_RESET path). Keeps the
    /// programmed rate/format/volume so a guest can re-arm without reprogramming.
    pub fn reset_playback(&mut self) {
        self.playing = false;
        self.auto_init = false;
        self.block_remaining = 0;
        self.half_reached = false;
        self.irq_pending = false;
        self.left.reset();
        self.right.reset();
        self.fifo.clear();
    }

    /// Take and clear a pending half/end IRQ (a guest ISR reads the status port
    /// bit7; the machine forwards the line to the PIC here).
    pub fn take_irq(&mut self) -> bool {
        let pending = self.irq_pending;
        self.irq_pending = false;
        pending
    }

    /// Advance the DMA playback by exactly one stereo frame and push the result
    /// onto the rendered-frame ring. This is the per-CPU-clock producer entry
    /// point, mirroring [`crate::SbDsp::tick_sample`]. `dma_fetch` feeds the
    /// ADPCM byte stream (one byte per two decoded samples per channel); the
    /// direct-write FIFO is the fallback source. The IRQ raised inside
    /// [`render_frame`] is left pending for the caller to forward via
    /// [`take_irq`].
    pub fn tick_sample<F>(&mut self, dma_fetch: F)
    where
        F: FnMut() -> Option<u8>,
    {
        if let Some(frame) = self.render_frame(dma_fetch) {
            if self.rendered.len() >= ADPCM_RING_CAP {
                self.rendered.pop_front();
            }
            self.rendered.push_back(frame);
        }
    }

    /// Produce one stereo output frame for the current mode, or None if the
    /// channel is idle or starved. `block_remaining` is decremented by the
    /// nibbles consumed (1 mono, 2 stereo) and the half/end IRQs are edged like
    /// the SB DSP path. Each nibble is decoded through the selected Yamaha
    /// codec, then the per-channel volume scalar is applied.
    pub fn render_frame<F>(&mut self, mut dma_fetch: F) -> Option<(i16, i16)>
    where
        F: FnMut() -> Option<u8>,
    {
        if !self.playing {
            return None;
        }
        let format = self.format;
        // Combine the host DMA fetch and the direct-write FIFO fallback into one
        // byte source so the channel decoder needs no borrow on the chip.
        let mut fetch = || dma_fetch().or_else(|| self.fifo.pop_front());
        let left = decode_channel(&mut self.left, format, &mut fetch)?;
        let right = if self.stereo {
            decode_channel(&mut self.right, format, &mut fetch)?
        } else {
            left
        };
        let consumed = if self.stereo { 2 } else { 1 };
        self.advance_block(consumed);
        let gain = f32::from(self.volume) / 255.0;
        Some((scale(left, gain), scale(right, gain)))
    }

    /// Decrement the block counter by `consumed` nibbles and edge the half and
    /// end-of-buffer IRQs. Shared structure with the SB DSP `advance_block`.
    fn advance_block(&mut self, consumed: u32) {
        self.block_remaining = self.block_remaining.saturating_sub(consumed);
        if !self.half_reached && self.block_remaining <= self.block_size / 2 {
            self.half_reached = true;
            self.irq_pending = true;
        }
        if self.block_remaining == 0 {
            self.irq_pending = true;
            if self.auto_init {
                self.block_remaining = self.block_size;
                self.half_reached = false;
            } else {
                self.playing = false;
            }
        }
    }

    /// Pop the oldest rendered stereo frame for the host audio path, or None
    /// when the ring is empty.
    pub fn drain_frame(&mut self) -> Option<(i16, i16)> {
        self.rendered.pop_front()
    }

    /// CPU clocks until the next half/end-buffer IRQ edge, or None when idle.
    /// Mirrors [`crate::SbDsp::clocks_until_next_irq`]; the counter ticks in
    /// decoded nibbles (one per sample), so the rate is the per-channel rate.
    pub fn clocks_until_next_irq(&self, rate_hz: u32, clock_hz: u64) -> Option<u64> {
        if !self.playing || rate_hz == 0 {
            return None;
        }
        let half_left = if self.half_reached {
            u32::MAX
        } else {
            self.block_remaining.saturating_sub(self.block_size / 2)
        };
        let end_left = self.block_remaining;
        let samples = half_left.min(end_left).max(1) as u64;
        Some(((samples * clock_hz).div_ceil(rate_hz as u64)).max(1))
    }

    /// Test/inspection accessor for the remaining block count.
    pub fn block_remaining(&self) -> u32 {
        self.block_remaining
    }
}

/// Apply a linear volume gain to a decoded sample, clamping to i16.
fn scale(sample: i16, gain: f32) -> i16 {
    (i32::from(sample) as f32 * gain)
        .clamp(-32768.0, 32767.0)
        .round() as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Codec library: bit-exact anchors against the C reference. ---------

    #[test]
    fn adpcm_b_encoded_silence_round_trips_to_near_zero() {
        // ADPCM-B has no all-zero silence code (code 0 adds a +15 step), so a raw
        // 0x00 stream ramps the predictor. Instead, ENCODE a flat-zero signal:
        // the encoder emits alternating sign codes that hold the predictor at ~0,
        // and decoding them back stays near zero.
        let input = [0i16; 32];
        let mut encoded = [0u8; 16];
        encode_adpcm_b(&input, &mut encoded);
        let mut decoded = [0i16; 32];
        decode_adpcm_b(&encoded, &mut decoded);
        assert!(
            decoded.iter().all(|&s| s.abs() < 256),
            "encoded silence round-trips near zero: {decoded:?}"
        );
    }

    #[test]
    fn adpcm_b_step_matches_reference_arithmetic() {
        // code 0x08 (sign set, delta 0): diff = (1*127)>>3 = 15;
        // history -> -15; nstep = (57*127)>>6 = 112, clamped to 127 (no change).
        let mut s = YmAdpcmState::default();
        assert_eq!(s.step_b(8), -15, "code 8: history -15");
        assert_eq!(s.step_size, 127, "nstep clamps up to 127");
    }

    #[test]
    fn adpcm_b_encode_decode_round_trips_a_slow_sine() {
        // The codec is a differential predictor: it tracks a slowly varying
        // signal within a few quantization steps but cannot jump instantly. A
        // low-amplitude, long-period sine has small per-sample deltas, so the
        // round-trip error stays tight everywhere (no initial transient).
        let input: Vec<i16> = (0..512)
            .map(|i| (1500.0 * (2.0 * std::f64::consts::PI * i as f64 / 128.0).sin()) as i16)
            .collect();
        let mut encoded = vec![0u8; input.len() / 2];
        encode_adpcm_b(&input, &mut encoded);
        let mut decoded = vec![0i16; input.len()];
        decode_adpcm_b(&encoded, &mut decoded);
        for (orig, dec) in input.iter().zip(&decoded) {
            assert!(
                (i32::from(*orig) - i32::from(*dec)).abs() < 256,
                "slow sine round-trip within envelope: {orig} vs {dec}"
            );
        }
    }

    #[test]
    fn ymz_step_matches_reference_arithmetic() {
        // code 0x08 (sign set, delta 0): diff = (1*127)>>3 = 15, clamped to
        // [0,32767] -> 15; history -> -15; nstep = (230*127)>>8 = 114, clamped
        // up to 127.
        let mut s = YmAdpcmState::default();
        assert_eq!(s.step_z(8), -15, "ymz code 8: history -15");
        assert_eq!(s.step_size, 127, "ymz nstep clamps up to 127");
    }

    #[test]
    fn aica_reads_the_low_nibble_first_like_ymz_on_the_swapped_byte() {
        // AICA and YMZ280B differ only in nibble order: AICA's low-nibble-first
        // read of byte 0x12 equals YMZ's high-nibble-first read of the
        // nibble-swapped byte 0x21. The per-sample high-pass couples successive
        // samples, so this holds on the FIRST sample (history 0, high-pass of 0).
        let mut ymz = [0i16; 1];
        let mut aica = [0i16; 1];
        decode_ymz280b(&[0x21], &mut ymz);
        decode_aica(&[0x12], &mut aica);
        assert_eq!(
            aica[0], ymz[0],
            "aica low-first of 0x12 == ymz high-first of 0x21"
        );
    }

    #[test]
    fn adpcm_a_step_matches_reference() {
        // code 0 (delta_table[0]=1, step_table[0]=16): delta = 1*16/8 = 2;
        // history -> 2 (12-bit, positive). adjust_table[0]=-1 -> step_hist stays
        // 0 (clamped). Returned <<4 = 32.
        let mut s = YmaAdpcmState::default();
        assert_eq!(s.step(0), 2);
        assert_eq!(s.step_hist, 0);
        let mut s2 = YmaAdpcmState::default();
        assert_eq!(s2.step(0) << 4, 32, "decode shifts the 12-bit value up");
    }

    #[test]
    fn adpcm_a_step_advances_the_predictor_monotonically_for_code_zero() {
        // Code 0 (delta_table[0]=1, step_table[0]=16): delta = 1*16/8 = 2, so a
        // stream of code 0 steps the 12-bit history up by 2 each sample. The
        // decoded value (12-bit, shifted <<4) grows by 32 per sample, never near
        // a fixed silence -- there is no zero code in ADPCM-A either.
        let mut s = YmaAdpcmState::default();
        assert_eq!(s.step(0), 2);
        assert_eq!(s.step(0), 4);
        assert_eq!(s.step(0), 6, "code 0 advances history by 2 each step");
    }

    // ---- Chip device: register/decode/IRQ behaviour. -----------------------

    fn write_reg(chip: &mut YamahaAdpcmChip, index: u8, value: u8) {
        chip.write_port(0, index);
        chip.write_port(1, value);
    }

    fn arm_mono(chip: &mut YamahaAdpcmChip, count: u16) {
        write_reg(chip, reg::RATE_LOW, 0x11);
        write_reg(chip, reg::RATE_HIGH, 0x2B); // 0x2B11 = 11025 Hz
        write_reg(chip, reg::COUNT_LOW, (count - 1) as u8);
        write_reg(chip, reg::COUNT_HIGH, ((count - 1) >> 8) as u8);
        write_reg(chip, reg::FORMAT, 0); // ADPCM-B, mono
        write_reg(chip, reg::CONTROL, CONTROL_START);
    }

    #[test]
    fn defaults_select_adpcm_b_at_11025_mono() {
        let chip = YamahaAdpcmChip::default();
        assert_eq!(chip.format(), AdpcmFormat::AdpcmB);
        assert_eq!(chip.rate_hz(), 11_025);
        assert!(!chip.is_stereo());
        assert!(!chip.is_playing());
    }

    #[test]
    fn start_control_arms_playback_at_programmed_rate() {
        let mut chip = YamahaAdpcmChip::default();
        arm_mono(&mut chip, 8);
        assert!(chip.is_playing(), "START arms playback");
        assert_eq!(chip.rate_hz(), 11_025);
    }

    #[test]
    fn offset_decodes_only_the_four_port_window() {
        let chip = YamahaAdpcmChip::default();
        assert_eq!(chip.offset(0x0240), Some(0));
        assert_eq!(chip.offset(0x0243), Some(3));
        assert_eq!(chip.offset(0x0244), None, "just past the window");
        assert_eq!(chip.offset(0x0220), None, "SB16 window not claimed");
    }

    #[test]
    fn status_reflects_playing() {
        let mut chip = YamahaAdpcmChip::default();
        assert_eq!(chip.read_port(0), 0x00, "idle status clear");
        arm_mono(&mut chip, 8);
        assert_eq!(
            chip.read_port(0) & STATUS_PLAYING,
            STATUS_PLAYING,
            "playing bit set after arm"
        );
    }

    #[test]
    fn tick_sample_decodes_dma_bytes_and_edges_irqs() {
        let mut chip = YamahaAdpcmChip::default();
        // Block 8 nibbles = 4 bytes mono. Feed all-zero ADPCM -> silence.
        arm_mono(&mut chip, 8);
        let bytes = [0x00u8; 4];
        let mut i = 0;
        let mut irq_at = Vec::new();
        for s in 1..=8 {
            chip.tick_sample(|| {
                if i < bytes.len() {
                    let b = bytes[i];
                    i += 1;
                    Some(b)
                } else {
                    None
                }
            });
            if chip.take_irq() {
                irq_at.push(s);
            }
        }
        // Mono: one byte -> two samples, so 8 nibbles = 4 bytes -> 8 samples.
        assert_eq!(irq_at, vec![4, 8], "half at 4, end at 8 nibbles");
        // Single mode stops at terminal count.
        assert!(!chip.is_playing(), "single mode halts after the block");
    }

    #[test]
    fn auto_init_keeps_playing_past_terminal_count() {
        let mut chip = YamahaAdpcmChip::default();
        arm_mono(&mut chip, 8);
        // Re-arm as auto-init.
        write_reg(&mut chip, reg::CONTROL, CONTROL_AUTO_INIT | CONTROL_START);
        let mut feed = || Some(0x00u8);
        for _ in 0..16 {
            chip.tick_sample(&mut feed);
            let _ = chip.take_irq();
        }
        assert!(chip.is_playing(), "auto-init continues past TC");
    }

    #[test]
    fn direct_fifo_path_decodes_without_dma() {
        // Push ADPCM bytes through the data FIFO port and decode with a DMA
        // fetcher that yields nothing.
        let mut chip = YamahaAdpcmChip::default();
        arm_mono(&mut chip, 4);
        for _ in 0..2 {
            chip.write_port(3, 0x00);
        }
        let mut frames = 0;
        for _ in 0..4 {
            if chip.render_frame(|| None).is_some() {
                frames += 1;
            }
        }
        assert!(frames >= 2, "FIFO-fed decode produced frames: {frames}");
    }

    #[test]
    fn volume_scales_the_decoded_sample() {
        let mut loud = YamahaAdpcmChip::default();
        arm_mono(&mut loud, 8);
        write_reg(&mut loud, reg::VOLUME, 255);
        let mut quiet = YamahaAdpcmChip::default();
        arm_mono(&mut quiet, 8);
        write_reg(&mut quiet, reg::VOLUME, 64);
        // Code 0x88 produces a nonzero step; louder volume yields larger magnitude.
        let feed = || Some(0x88u8);
        let lf = loud.render_frame(feed).unwrap().0.unsigned_abs();
        let qf = quiet.render_frame(feed).unwrap().0.unsigned_abs();
        assert!(lf > qf, "full volume ({lf}) louder than 64/255 ({qf})");
    }

    #[test]
    fn reset_control_stops_and_clears_state() {
        let mut chip = YamahaAdpcmChip::default();
        arm_mono(&mut chip, 8);
        assert!(chip.is_playing());
        write_reg(&mut chip, reg::CONTROL, CONTROL_RESET);
        assert!(!chip.is_playing(), "RESET halts playback");
        assert_eq!(chip.block_remaining(), 0);
    }

    #[test]
    fn resource_readback_port_reports_irq_and_dma() {
        let mut chip = YamahaAdpcmChip::new(AdpcmConfig {
            irq: 10,
            dma: 3,
            ..AdpcmConfig::default()
        });
        let rb = chip.read_port(2);
        assert_eq!(rb >> 4, 10, "high nibble = IRQ");
        assert_eq!(rb & 0x0F, 3, "low nibble = DMA");
    }
}
