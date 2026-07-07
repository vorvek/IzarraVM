//! Sound Blaster 16-class DSP (CT1747) clean-room core: reset handshake,
//! command/data protocol, 8-bit plus 16-bit single/auto-init DMA playback, and
//! Creative ADPCM (4-bit, 2.6-bit, 2-bit) decode on the 8-bit DMA path. The
//! CT1745 mixer lives next to this in the machine crate. Input/ADC and
//! MIDI/MPU-401 are not modeled yet.

use crate::pcm::{push_frame_capped, sample_i16, sample_u8, sample_u16};
use std::collections::VecDeque;

pub const DSP_VERSION_HI: u8 = 4;
pub const DSP_VERSION_LO: u8 = 5;

// ===========================================================================
//  Creative ADPCM decoder (DSP commands 0x74-0x77, 0x16/0x17, 0x1F/0x7D/0x7F).
//
//  The SB DSP decodes its own ADPCM: the DMA delivers packed codes and each
//  encoded byte expands to 2 (4-bit), 3 (2.6-bit), or 4 (2-bit) 8-bit unsigned
//  PCM samples through an adaptive predictor. The step/adjust tables and the
//  predictor arithmetic are a 1:1 port of DOSBox-X's decode_ADPCM_* functions
//  (src/hardware/sblaster.cpp, GPLv2-or-later); see NOTICE. DOSBox-X names the
//  2.6-bit format "ADPCM_3" (three samples per byte), mirrored here as Bits26.
// ===========================================================================

/// 4-bit code -> reference delta, indexed by `code + step` (0..=63).
const ADPCM4_SCALE: [i8; 64] = [
    0, 1, 2, 3, 4, 5, 6, 7, 0, -1, -2, -3, -4, -5, -6, -7, //
    1, 3, 5, 7, 9, 11, 13, 15, -1, -3, -5, -7, -9, -11, -13, -15, //
    2, 6, 10, 14, 18, 22, 26, 30, -2, -6, -10, -14, -18, -22, -26, -30, //
    4, 12, 20, 28, 36, 44, 52, 60, -4, -12, -20, -28, -36, -44, -52, -60,
];
/// 4-bit step adjustment (added mod 256), indexed by `code + step` (0..=63).
const ADPCM4_ADJUST: [u8; 64] = [
    0, 0, 0, 0, 0, 16, 16, 16, 0, 0, 0, 0, 0, 16, 16, 16, //
    240, 0, 0, 0, 0, 16, 16, 16, 240, 0, 0, 0, 0, 16, 16, 16, //
    240, 0, 0, 0, 0, 16, 16, 16, 240, 0, 0, 0, 0, 16, 16, 16, //
    240, 0, 0, 0, 0, 0, 0, 0, 240, 0, 0, 0, 0, 0, 0, 0,
];
/// 2.6-bit (three-per-byte) reference delta, indexed by `code + step` (0..=39).
const ADPCM3_SCALE: [i8; 40] = [
    0, 1, 2, 3, 0, -1, -2, -3, //
    1, 3, 5, 7, -1, -3, -5, -7, //
    2, 6, 10, 14, -2, -6, -10, -14, //
    4, 12, 20, 28, -4, -12, -20, -28, //
    5, 15, 25, 35, -5, -15, -25, -35,
];
/// 2.6-bit step adjustment (added mod 256), indexed by `code + step` (0..=39).
const ADPCM3_ADJUST: [u8; 40] = [
    0, 0, 0, 8, 0, 0, 0, 8, //
    248, 0, 0, 8, 248, 0, 0, 8, //
    248, 0, 0, 8, 248, 0, 0, 8, //
    248, 0, 0, 8, 248, 0, 0, 8, //
    248, 0, 0, 0, 248, 0, 0, 0,
];
/// 2-bit reference delta, indexed by `code + step` (0..=23).
const ADPCM2_SCALE: [i8; 24] = [
    0, 1, 0, -1, 1, 3, -1, -3, //
    2, 6, -2, -6, 4, 12, -4, -12, //
    8, 24, -8, -24, 16, 48, -16, -48,
];
/// 2-bit step adjustment (added mod 256), indexed by `code + step` (0..=23).
const ADPCM2_ADJUST: [u8; 24] = [
    0, 4, 0, 4, //
    252, 4, 252, 4, 252, 4, 252, 4, //
    252, 4, 252, 4, 252, 4, 252, 4, //
    252, 0, 252, 0,
];

/// Which Creative ADPCM packing the armed transfer uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdpcmMode {
    /// 4-bit, two samples per byte (high nibble first).
    Bits4,
    /// 2.6-bit, three samples per byte (bits 7:5, 4:2, then 1:0 shifted up).
    Bits26,
    /// 2-bit, four samples per byte (high pair first).
    Bits2,
}

impl AdpcmMode {
    /// The (scale, adjust, max-index) triple for this packing.
    fn tables(self) -> (&'static [i8], &'static [u8], i32) {
        match self {
            AdpcmMode::Bits4 => (&ADPCM4_SCALE, &ADPCM4_ADJUST, 63),
            AdpcmMode::Bits26 => (&ADPCM3_SCALE, &ADPCM3_ADJUST, 39),
            AdpcmMode::Bits2 => (&ADPCM2_SCALE, &ADPCM2_ADJUST, 23),
        }
    }
}

/// Running Creative ADPCM decode state for one armed transfer: the adaptive
/// `reference` (the last decoded 8-bit sample), the `step` index bias, whether
/// the next DMA byte is still the reference-init byte, and a small FIFO of
/// already-decoded samples (one byte expands to up to four).
#[derive(Debug, Clone, PartialEq)]
struct AdpcmState {
    mode: AdpcmMode,
    reference: u8,
    step: i32,
    haveref: bool,
    buf: VecDeque<u8>,
}

impl AdpcmState {
    fn new(mode: AdpcmMode, haveref: bool) -> Self {
        Self {
            mode,
            // Centered silence until a reference byte (if any) overwrites it.
            reference: 0x80,
            step: 0,
            haveref,
            buf: VecDeque::new(),
        }
    }

    /// Decode one code, advancing the predictor. 1:1 with DOSBox-X's
    /// decode_ADPCM_*_sample: `samp = code + step` clamped to the table range,
    /// the reference moves by the scale delta (clamped 0..=255), and the step
    /// index is bumped by the adjust value modulo 256.
    fn decode_code(&mut self, code: u8) -> u8 {
        let (scale_map, adjust_map, max_idx) = self.mode.tables();
        let samp = (i32::from(code) + self.step).clamp(0, max_idx) as usize;
        self.reference =
            (i32::from(self.reference) + i32::from(scale_map[samp])).clamp(0, 255) as u8;
        self.step = (self.step + i32::from(adjust_map[samp])) & 0xFF;
        self.reference
    }

    /// Expand one encoded DMA byte into its 2/3/4 decoded samples.
    fn decode_byte(&mut self, byte: u8) {
        match self.mode {
            AdpcmMode::Bits4 => {
                let s = self.decode_code(byte >> 4);
                self.buf.push_back(s);
                let s = self.decode_code(byte & 0x0F);
                self.buf.push_back(s);
            }
            AdpcmMode::Bits26 => {
                let s = self.decode_code((byte >> 5) & 0x07);
                self.buf.push_back(s);
                let s = self.decode_code((byte >> 2) & 0x07);
                self.buf.push_back(s);
                let s = self.decode_code((byte & 0x03) << 1);
                self.buf.push_back(s);
            }
            AdpcmMode::Bits2 => {
                for shift in [6, 4, 2, 0] {
                    let s = self.decode_code((byte >> shift) & 0x03);
                    self.buf.push_back(s);
                }
            }
        }
    }
}

/// One DSP. The reset port (0x226) drives a microsecond countdown; when it
/// elapses the DSP queues 0xAA on read-data and asserts data-available.
#[derive(Debug, Clone, PartialEq)]
pub struct SbDsp {
    reset_micros: Option<f64>,
    read_data: VecDeque<u8>,
    data_available: bool,
    // Last byte handed back on the read-data port. The bus holds its last value,
    // so a read with nothing queued returns this rather than a fixed byte.
    last_read: u8,
    // Command interpreter: bytes written to 0x22C stream in here.
    pending: Option<PendingCommand>,
    // Immediate-command state.
    direct_dac_byte: Option<u8>,
    test_reg: u8,
    speaker_on: bool,
    // 8-bit DMA playback state (Tasks 5-6).
    rate_hz: u32,
    // Whether rate_hz was programmed as an interleaved BYTE rate (the 0x40 time
    // constant pre-multiplies by the channel count for stereo) rather than a
    // per-channel rate. The 0x41 set-sample-rate command programs the
    // per-channel rate directly, so it must not be halved for SB Pro stereo.
    rate_is_byte_rate: bool,
    block_size: u32,
    block_remaining: u32,
    auto_init: bool,
    playing: bool,
    irq_pending: bool,
    half_reached: bool,
    // 16-bit DMA playback state (SB16 0xBx family). dma_16bit selects the word
    // fetch and sample-depth path; stereo selects one vs. two words per frame;
    // sample_signed selects signed vs. unsigned 16-bit conversion.
    dma_16bit: bool,
    stereo: bool,
    sample_signed: bool,
    // SB Pro 8-bit stereo (mixer register 0x0E bit1): interleaves two bytes per
    // output frame (left then right). Set from the mixer each producer tick.
    sbpro_stereo: bool,
    // Creative ADPCM decode state when an ADPCM command armed the 8-bit path;
    // None for plain PCM. Set by the 0x74/0x75/0x76/0x77/0x16/0x17/0x1F/0x7D/0x7F
    // commands, cleared by any PCM arm or a DSP reset.
    adpcm: Option<AdpcmState>,
    // Rendered stereo frames produced by the per-CPU-clock producer, drained by
    // the host audio path. Rate-match buffer: on push when full the oldest
    // frame drops (fidelity may glitch, block counter/IRQ timing stay
    // correct). Cap and policy live in `pcm::push_frame_capped`.
    rendered: VecDeque<(i16, i16)>,
}

#[derive(Debug, Clone, PartialEq)]
struct PendingCommand {
    command: u8,
    args: Vec<u8>,
}

impl Default for SbDsp {
    fn default() -> Self {
        Self {
            reset_micros: None,
            read_data: VecDeque::new(),
            data_available: false,
            last_read: 0xFF,
            pending: None,
            direct_dac_byte: None,
            test_reg: 0,
            speaker_on: false,
            rate_hz: 22_050,
            rate_is_byte_rate: true,
            block_size: 0,
            block_remaining: 0,
            auto_init: false,
            playing: false,
            irq_pending: false,
            half_reached: false,
            dma_16bit: false,
            stereo: false,
            sample_signed: false,
            sbpro_stereo: false,
            adpcm: None,
            rendered: VecDeque::new(),
        }
    }
}

impl SbDsp {
    /// Advance the DSP's reset-settle countdown by `micros` microseconds. When
    /// the countdown elapses the DSP queues 0xAA on read-data.
    pub fn advance_micros(&mut self, micros: f64) {
        if let Some(remaining) = self.reset_micros.as_mut() {
            *remaining -= micros;
            if *remaining <= 0.0 {
                self.queue_read(0xAA);
                self.reset_micros = None;
            }
        }
    }

    fn queue_read(&mut self, byte: u8) {
        self.read_data.push_back(byte);
        self.data_available = true;
    }

    /// Number of argument bytes a DSP command consumes before it can dispatch.
    fn command_arity(command: u8) -> usize {
        match command {
            0x10 | 0xE4 => 1, // direct DAC / test-register write
            0x40 => 1,        // set time constant
            0x41 => 2,        // set sample rate
            0x14 => 2,        // 8-bit single-cycle DMA output, length low/high
            // Single-cycle Creative ADPCM output: mode byte set by the opcode,
            // 2-byte length inline (like 0x14). Auto-init ADPCM (0x1F/0x7D/0x7F)
            // takes no args -- it uses the 0x48 block size.
            0x74 | 0x75 | 0x76 | 0x77 | 0x16 | 0x17 => 2,
            0x48 => 2, // set block size for auto-init/high-speed modes
            // The SB16 0xBx family (16-bit DMA output/input, single + auto-init)
            // takes a mode byte plus a 2-byte transfer count.
            0xB0..=0xBF => 3,
            _ => 0,
        }
    }

    /// Push a command/data byte into the interpreter; dispatches when complete.
    fn write_command_byte(&mut self, byte: u8) {
        if let Some(mut pending) = self.pending.take() {
            pending.args.push(byte);
            if pending.args.len() >= Self::command_arity(pending.command) {
                self.dispatch(pending.command, &pending.args);
            } else {
                self.pending = Some(pending);
            }
            return;
        }
        let arity = Self::command_arity(byte);
        if arity == 0 {
            self.dispatch(byte, &[]);
        } else {
            self.pending = Some(PendingCommand {
                command: byte,
                args: Vec::new(),
            });
        }
    }

    /// Execute a fully-assembled command with its argument bytes.
    fn dispatch(&mut self, command: u8, args: &[u8]) {
        match command {
            0x10 => self.direct_dac_byte = args.first().copied(),
            0xE4 => self.test_reg = args.first().copied().unwrap_or(0),
            0xE1 => {
                self.queue_read(DSP_VERSION_HI);
                self.queue_read(DSP_VERSION_LO);
            }
            0xE8 => self.queue_read(self.test_reg),
            0xD1 => self.speaker_on = true,
            0xD3 => self.speaker_on = false,
            0xE3 => {
                // The CT1747 copyright string, NUL-terminated, as the DSP returns it.
                for &b in b"Copyright (C) Creative Technology Ltd. 1992-94\0" {
                    self.queue_read(b);
                }
            }
            0x40 => {
                // Set time constant: rate = 1_000_000 / (256 - tc). The stereo
                // time constant encodes the interleaved byte rate (the guest
                // pre-multiplies by the channel count), so this is a byte rate.
                if let Some(&tc) = args.first() {
                    let divisor = 256u32.wrapping_sub(u32::from(tc));
                    if let Some(rate) = 1_000_000u32.checked_div(divisor) {
                        self.rate_hz = rate;
                        self.rate_is_byte_rate = true;
                    }
                }
            }
            0x41 => {
                // Set sample rate in Hz, high byte then low byte (SB16). Unlike
                // the time constant, this is already the per-channel rate for
                // stereo (no channel-count pre-multiply), so it is not a byte
                // rate and must not be halved for SB Pro stereo.
                if args.len() >= 2 {
                    self.rate_hz = (u32::from(args[0]) << 8) | u32::from(args[1]);
                    self.rate_is_byte_rate = false;
                }
            }
            0x48 => {
                // Set DSP block transfer size, low byte then high byte (n+1 bytes).
                if args.len() >= 2 {
                    let count = (u32::from(args[0]) | (u32::from(args[1]) << 8)) + 1;
                    self.block_size = count;
                }
            }
            0x14 => {
                if args.len() >= 2 {
                    self.block_size = (u32::from(args[0]) | (u32::from(args[1]) << 8)) + 1;
                }
                self.arm_dma(false);
            }
            0x1C => self.arm_dma(true), // 8-bit auto-init output, normal speed
            // Single-cycle Creative ADPCM: set the block size from the inline
            // length (encoded-byte count) like 0x14, then arm the decoder. The
            // reference variants (0x75/0x77/0x17) take the first DMA byte as the
            // predictor seed.
            0x74 | 0x75 | 0x76 | 0x77 | 0x16 | 0x17 => {
                if args.len() >= 2 {
                    self.block_size = (u32::from(args[0]) | (u32::from(args[1]) << 8)) + 1;
                }
                let (mode, haveref) = match command {
                    0x74 => (AdpcmMode::Bits4, false),
                    0x75 => (AdpcmMode::Bits4, true),
                    0x76 => (AdpcmMode::Bits26, false),
                    0x77 => (AdpcmMode::Bits26, true),
                    0x16 => (AdpcmMode::Bits2, false),
                    _ => (AdpcmMode::Bits2, true), // 0x17
                };
                self.arm_adpcm(mode, haveref, false);
            }
            // Auto-init Creative ADPCM (always reference-seeded on the first
            // block); the block size comes from the prior 0x48.
            0x1F => self.arm_adpcm(AdpcmMode::Bits2, true, true),
            0x7D => self.arm_adpcm(AdpcmMode::Bits4, true, true),
            0x7F => self.arm_adpcm(AdpcmMode::Bits26, true, true),
            // 0x90/0x91 are the SB Pro high-speed variants of auto-init/single.
            // Limit: high-speed command-lockout (DSP ignores commands until
            // reset) not modeled; games exit via the DSP reset handled below.
            0x90 => self.arm_dma(true),  // 8-bit auto-init, high-speed
            0x91 => self.arm_dma(false), // 8-bit single, high-speed
            0xB0..=0xBF => self.arm_16bit(command, args),
            0xD0 => self.playing = false,   // halt DMA (position kept)
            0xD4 => self.playing = true,    // continue DMA
            0xDA => self.auto_init = false, // exit auto-init: stop at next TC
            0xF2 => {
                // Request the 8-bit interrupt immediately (the documented DSP
                // IRQ-probe command drivers use to verify the IRQ wiring). Same
                // pending state a DMA block boundary raises; acknowledged by
                // reading port 0x22E.
                self.irq_pending = true;
            }
            _ => {}
        }
    }

    fn arm_dma(&mut self, auto_init: bool) {
        // 8-bit DMA is mono unsigned PCM: clear the 16-bit/stereo/signed latches
        // and drop any ADPCM decode state so the raw-byte path runs.
        self.dma_16bit = false;
        self.stereo = false;
        self.sample_signed = false;
        self.adpcm = None;
        self.auto_init = auto_init;
        self.playing = true;
        self.block_remaining = self.block_size;
        self.half_reached = false;
    }

    /// Arm the 8-bit DMA path as a Creative ADPCM transfer. Mono unsigned like
    /// `arm_dma`, but the fetched bytes are decoded through `AdpcmState`. The
    /// block counter still counts encoded DMA bytes (including the reference
    /// seed byte), so the half/end IRQs land exactly as the raw 8-bit path.
    fn arm_adpcm(&mut self, mode: AdpcmMode, haveref: bool, auto_init: bool) {
        self.dma_16bit = false;
        self.stereo = false;
        self.sample_signed = false;
        self.adpcm = Some(AdpcmState::new(mode, haveref));
        self.auto_init = auto_init;
        self.playing = true;
        self.block_remaining = self.block_size;
        self.half_reached = false;
    }

    /// Arm the SB16 16-bit DMA path from a 0xBx command (mode byte + 2-byte
    /// count). The command's auto-init bit is bit2 (0x04); bit3 (0x08) selects
    /// A/D input. Mode byte: bit5 (0x20) = stereo, bit4 (0x10) = signed. Input
    /// commands arm nothing (ADC is out of scope).
    fn arm_16bit(&mut self, command: u8, args: &[u8]) {
        if command & 0x08 != 0 {
            // A/D (input) command; out of scope, so do not arm playback.
            return;
        }
        let auto_init = command & 0x04 != 0;
        let mode = args.first().copied().unwrap_or(0);
        let stereo = mode & 0x20 != 0;
        let signed = mode & 0x10 != 0;
        // A 16-bit PCM transfer never carries ADPCM state.
        self.adpcm = None;
        // Count is low byte then high byte, value n means n+1 16-bit samples.
        let count_lo = u32::from(args.get(1).copied().unwrap_or(0));
        let count_hi = u32::from(args.get(2).copied().unwrap_or(0));
        let count = (count_lo | (count_hi << 8)) + 1;
        self.dma_16bit = true;
        self.stereo = stereo;
        self.sample_signed = signed;
        self.auto_init = auto_init;
        self.block_size = count;
        self.block_remaining = count;
        self.half_reached = false;
        self.playing = true;
    }

    pub fn rate_hz(&self) -> u32 {
        self.rate_hz
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn is_auto_init(&self) -> bool {
        self.auto_init
    }

    /// Whether the armed DMA mode is the SB16 16-bit (0xBx) path.
    pub fn is_16bit(&self) -> bool {
        self.dma_16bit
    }

    /// Whether the armed DMA mode is stereo (two words per output frame).
    pub fn is_stereo(&self) -> bool {
        self.stereo
    }

    /// Set the SB Pro 8-bit stereo flag from the mixer (register 0x0E bit1).
    pub fn set_sbpro_stereo(&mut self, on: bool) {
        self.sbpro_stereo = on;
    }

    /// Whether SB Pro 8-bit stereo is selected by the mixer. This is a derived
    /// view of the mixer's 0x0E bit1 and is sticky across mode changes (it is
    /// not cleared when a 16-bit 0xBx mode is armed). Every consumer MUST AND it
    /// with `!is_16bit()`, since SB Pro byte-interleave only applies to the
    /// 8-bit DMA path; `render_frame` and `output_frame_rate` both do.
    pub fn is_sbpro_stereo(&self) -> bool {
        self.sbpro_stereo
    }

    pub fn block_remaining(&self) -> u32 {
        self.block_remaining
    }

    /// Advance the DMA playback by exactly one stereo frame and push the result
    /// onto the rendered-frame ring. This is the per-CPU-clock producer entry
    /// point: it wraps the existing [`render_frame`] (which advances the block
    /// counter and edges the half/end IRQs) and buffers the frame for the host
    /// drainer. A `None` frame (channel idle or DMA exhausted) is not pushed.
    /// The IRQ raised inside `render_frame` is left pending for the caller to
    /// forward to the PIC via [`take_irq`].
    pub fn tick_sample<B, W>(&mut self, byte_fetch: B, word_fetch: W)
    where
        B: FnMut() -> Option<u8>,
        W: FnMut() -> Option<u16>,
    {
        if let Some(frame) = self.render_frame(byte_fetch, word_fetch) {
            push_frame_capped(&mut self.rendered, frame);
        }
    }

    /// Pop the oldest rendered stereo frame for the host audio path, or None
    /// when the ring is empty (silent DSP = OPL passthrough).
    pub fn drain_frame(&mut self) -> Option<(i16, i16)> {
        self.rendered.pop_front()
    }

    /// CPU clocks until the next half/end-buffer IRQ edges, or None when the DSP
    /// is not playing. The next edge is the sooner of the half-buffer point
    /// (`block_remaining - block_size/2`, unless already reached) and the
    /// end-of-buffer point (`block_remaining`). Converted to CPU clocks via
    /// `ceil(samples * clock_hz / rate_hz)`, clamped to at least one. `rate_hz`
    /// must be the rate at which the block counter actually drains (the raw
    /// byte/word rate from [`rate_hz`](Self::rate_hz), not the per-channel
    /// output frame rate): the counter ticks in bytes for 8-bit and words for
    /// 16-bit. With the byte/word rate this is exact for every 8-bit mode
    /// (including SB Pro stereo, which drains two bytes per frame at the full
    /// byte rate). For 16-bit stereo the counter advances two words per frame
    /// while `rate_hz` is per-frame, so this stays a conservative (never under-)
    /// estimate, which is what the HLT fast-forward needs.
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

    /// Produce one stereo output frame for the current DMA mode, or None if the
    /// channel is idle. `byte_fetch` feeds the 8-bit DMA path and `word_fetch`
    /// the 16-bit path; only the one matching the armed mode is pulled. Mono
    /// modes duplicate their single sample to both channels. `block_remaining`
    /// is decremented by the words consumed (1 for 8-bit and 16-bit mono, 2 for
    /// 16-bit stereo), and the half/end-buffer IRQ is edged exactly as the 8-bit
    /// path does.
    pub fn render_frame<B, W>(&mut self, mut byte_fetch: B, mut word_fetch: W) -> Option<(i16, i16)>
    where
        B: FnMut() -> Option<u8>,
        W: FnMut() -> Option<u16>,
    {
        // Creative ADPCM (mono, 8-bit path) decodes one sample per frame,
        // pulling an encoded byte only when its decoded-sample FIFO runs dry.
        if self.adpcm.is_some() {
            let sample = self.pop_adpcm_sample(&mut byte_fetch)?;
            let s = sample_u8(sample);
            return Some((s, s));
        }
        if !self.playing {
            if self.dma_16bit {
                return None;
            }
            return self.direct_dac_byte.map(|b| {
                let s = sample_u8(b);
                (s, s)
            });
        }
        if self.dma_16bit {
            let left = self.sample_word(word_fetch()?);
            let right = if self.stereo {
                self.sample_word(word_fetch()?)
            } else {
                left
            };
            let words = if self.stereo { 2 } else { 1 };
            self.advance_block(words);
            Some((left, right))
        } else if self.sbpro_stereo {
            // SB Pro 8-bit stereo: two interleaved bytes per frame, left then
            // right, advancing the block counter by both bytes consumed.
            // The SB Pro silent-byte priming / L<->R channel-swap alignment quirk
            // is not modeled, so the first byte of each frame is always Left.
            let left = sample_u8(byte_fetch()?);
            let right = sample_u8(byte_fetch()?);
            self.advance_block(2);
            Some((left, right))
        } else {
            let s = sample_u8(byte_fetch()?);
            self.advance_block(1);
            Some((s, s))
        }
    }

    /// Pop the next decoded Creative ADPCM sample, fetching and decoding encoded
    /// DMA bytes as needed. Each fetched byte advances the block counter by one
    /// (bytes, not decoded samples, are what the DMA length counts) and edges the
    /// half/end IRQ. The reference-init byte seeds the predictor and yields no
    /// sample, so the loop fetches again. Returns None when the FIFO is empty and
    /// the channel is stopped or the DMA source is starved; buffered samples from
    /// the final byte still drain even after the block ends.
    fn pop_adpcm_sample<B>(&mut self, byte_fetch: &mut B) -> Option<u8>
    where
        B: FnMut() -> Option<u8>,
    {
        loop {
            match self.adpcm.as_mut() {
                Some(state) => {
                    if let Some(sample) = state.buf.pop_front() {
                        return Some(sample);
                    }
                }
                None => return None,
            }
            if !self.playing {
                return None;
            }
            let byte = byte_fetch()?;
            self.advance_block(1);
            match self.adpcm.as_mut() {
                Some(state) => {
                    if state.haveref {
                        state.haveref = false;
                        state.reference = byte;
                        state.step = 0;
                    } else {
                        state.decode_byte(byte);
                    }
                }
                None => return None,
            }
        }
    }

    /// Per-channel output frame rate. The SB Pro time constant (0x40) programs
    /// the interleaved BYTE rate, so in 8-bit stereo each channel runs at half
    /// that. The 0x41 set-sample-rate command instead programs the per-channel
    /// rate directly (no channel-count pre-multiply), so it must not be halved.
    /// Every other mode (mono, or any 16-bit) frames at the programmed rate.
    pub fn output_frame_rate(&self) -> u32 {
        if self.sbpro_stereo && !self.dma_16bit && self.rate_is_byte_rate {
            // `rate_hz / 2` truncates on an odd byte rate; acceptable, since it
            // stays within the time-constant's own quantization.
            self.rate_hz / 2
        } else {
            self.rate_hz
        }
    }

    /// Convert one 16-bit DMA word per the armed sample format.
    fn sample_word(&self, word: u16) -> i16 {
        if self.sample_signed {
            sample_i16(word)
        } else {
            sample_u16(word)
        }
    }

    /// Decrement the block counter by `consumed` words and edge the half and
    /// end-of-buffer IRQs. Shared by the 8-bit and 16-bit render paths.
    fn advance_block(&mut self, consumed: u32) {
        self.block_remaining = self.block_remaining.saturating_sub(consumed);
        // Half-buffer IRQ fires once, at the block midpoint.
        if !self.half_reached && self.block_remaining <= self.block_size / 2 {
            self.half_reached = true;
            self.irq_pending = true;
        }
        if self.block_remaining == 0 {
            // End-of-buffer IRQ. Auto-init reloads and keeps going; single mode stops.
            self.irq_pending = true;
            if self.auto_init {
                self.block_remaining = self.block_size;
                self.half_reached = false;
            } else {
                self.playing = false;
            }
        }
    }

    /// Mono wrapper over [`render_frame`] for the 8-bit path (kept so the 8-bit
    /// unit tests stay green). Returns the single channel duplicated L/R as one
    /// i16.
    pub fn render_sample<F: FnMut() -> Option<u8>>(&mut self, mut fetch: F) -> Option<i16> {
        self.render_frame(&mut fetch, || None).map(|(l, _)| l)
    }

    /// Take and clear a pending half/end IRQ (cleared when the host reads 0x22E).
    pub fn take_irq(&mut self) -> bool {
        let pending = self.irq_pending;
        self.irq_pending = false;
        pending
    }

    /// Last byte written by a direct 8-bit DAC command (0x10).
    pub fn direct_dac_byte(&self) -> Option<u8> {
        self.direct_dac_byte
    }

    /// Whether the read-data port (0x22A) has a byte queued (the bit 0x80 a guest
    /// polls on 0x22E). During DMA playback this is always false: the DMA path
    /// never queues read-data bytes (only reset/version/copyright responses do).
    /// The lazy 0x22E read uses this to answer the poll without setting
    /// io_touched (see the machine's read_io lazy arm).
    pub fn data_available(&self) -> bool {
        self.data_available
    }

    pub fn read_port(&mut self, port: u16) -> Option<u8> {
        match port {
            0x22A => {
                // A real DSP read-data port holds the last byte it drove when the
                // queue is empty; it does not re-emit the 0xAA reset acknowledge.
                // Returning a fixed 0xAA here would let a poll mistake an empty
                // port for a fresh reset.
                let byte = self.read_data.pop_front().unwrap_or(self.last_read);
                self.last_read = byte;
                self.data_available = !self.read_data.is_empty();
                Some(byte)
            }
            // 0x22E is the 8-bit read-buffer status port and the 8-bit DMA
            // interrupt-acknowledge port; 0x22F is its 16-bit counterpart. Only
            // one DMA mode runs at a time, so a read of either status port clears
            // the single pending half/end IRQ.
            0x22E | 0x22F => {
                self.irq_pending = false;
                Some(if self.data_available { 0x80 } else { 0x00 })
            }
            _ => None,
        }
    }

    pub fn write_port(&mut self, port: u16, value: u8) -> bool {
        match port {
            0x226 => {
                // Write 1 arms the reset; write 0 starts the ~100us settle.
                if value == 0x01 {
                    self.reset_micros = Some(0.0);
                } else {
                    self.reset_micros = Some(100.0);
                    self.read_data.clear();
                    self.data_available = false;
                    // Real hardware halts playback and clears the interrupt
                    // latch on reset; clear the DMA state so a high-speed game's
                    // reset stops the channel cleanly. Clearing irq_pending here
                    // (and never re-arming it in arm_dma/arm_16bit) prevents a
                    // half/end IRQ that went pending before the reset from firing
                    // spuriously on the next re-armed playback. rate_hz and
                    // block_size are intentionally preserved: this is the
                    // halt-on-reset behavior, not a power-on parameter wipe.
                    self.playing = false;
                    self.auto_init = false;
                    self.block_remaining = 0;
                    self.half_reached = false;
                    self.irq_pending = false;
                    self.pending = None;
                    // The mode latches must clear too, or the idle 16-bit
                    // guard in render_frame keeps direct DAC dead after any
                    // 16-bit session.
                    self.dma_16bit = false;
                    self.stereo = false;
                    self.sample_signed = false;
                    // Drop any in-flight ADPCM decode so a post-reset PCM or
                    // direct-DAC path is not stuck decoding.
                    self.adpcm = None;
                }
                true
            }
            0x22C => {
                self.write_command_byte(value);
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
    fn reset_handshake_yields_0xaa() {
        let mut dsp = SbDsp::default();
        dsp.write_port(0x226, 0x01);
        dsp.write_port(0x226, 0x00);
        dsp.advance_micros(120.0); // > the ~100us the DSP needs to respond
        // 0x22E bit7 = data available.
        assert_eq!(dsp.read_port(0x22E), Some(0x80));
        assert_eq!(dsp.read_port(0x22A), Some(0xAA));
        assert_eq!(dsp.read_port(0x22E), Some(0x00), "data consumed");
    }

    #[test]
    fn empty_read_data_does_not_fake_a_reset_ack() {
        // With nothing queued and no reset, the read-data port returns the idle
        // bus value, not the 0xAA the DSP only emits after a real reset.
        let mut dsp = SbDsp::default();
        assert_eq!(dsp.read_port(0x22A), Some(0xFF));
    }

    #[test]
    fn dsp_claims_only_its_own_ports() {
        let mut dsp = SbDsp::default();
        assert!(
            !dsp.write_port(0x224, 0x00),
            "mixer (0x224) stays out of scope"
        );
        assert!(dsp.write_port(0x226, 0x00), "reset is a DSP port");
    }

    fn write_cmd(dsp: &mut SbDsp, bytes: &[u8]) {
        for &b in bytes {
            dsp.write_port(0x22C, b);
        }
    }

    #[test]
    fn version_command_returns_sb16_4_5() {
        let mut dsp = SbDsp::default();
        write_cmd(&mut dsp, &[0xE1]);
        assert_eq!(dsp.read_port(0x22A), Some(DSP_VERSION_HI));
        assert_eq!(dsp.read_port(0x22A), Some(DSP_VERSION_LO));
    }

    #[test]
    fn test_register_write_then_read_round_trips() {
        let mut dsp = SbDsp::default();
        write_cmd(&mut dsp, &[0xE4, 0x5A]);
        write_cmd(&mut dsp, &[0xE8]);
        assert_eq!(dsp.read_port(0x22A), Some(0x5A));
    }

    #[test]
    fn direct_dac_command_latches_one_byte() {
        let mut dsp = SbDsp::default();
        write_cmd(&mut dsp, &[0x10, 0x80]);
        assert_eq!(dsp.direct_dac_byte(), Some(0x80));
    }

    #[test]
    fn reset_clears_the_16bit_mode_latch_so_direct_dac_works_again() {
        let mut dsp = SbDsp::default();
        // Arm a 16-bit signed stereo auto-init playback (0xB6, mode 0x30).
        write_cmd(&mut dsp, &[0xB6, 0x30, 0x07, 0x00]);
        // Game resets the DSP (halt playback), then falls back to direct DAC.
        dsp.write_port(0x226, 0x01);
        dsp.write_port(0x226, 0x00);
        write_cmd(&mut dsp, &[0x10, 0x80]);
        let frame = dsp.render_frame(|| None, || None);
        assert_eq!(
            frame,
            Some((0, 0)),
            "direct DAC byte 0x80 (midpoint) must render after a reset"
        );
    }

    #[test]
    fn time_constant_sets_the_playback_rate() {
        let mut dsp = SbDsp::default();
        write_cmd(&mut dsp, &[0x40, 0x9C]); // tc 0x9C -> 1e6/(256-156)=1e6/100 = 10000 Hz
        assert_eq!(dsp.rate_hz(), 10_000);
    }

    #[test]
    fn sb16_rate_command_programs_hz_directly() {
        let mut dsp = SbDsp::default();
        write_cmd(&mut dsp, &[0x41, 0x2B, 0x11]); // 0x2B11 = 11025 Hz, high byte then low byte
        assert_eq!(dsp.rate_hz(), 11_025);
    }

    #[test]
    fn dma_single_output_arms_with_inline_length() {
        let mut dsp = SbDsp::default();
        write_cmd(&mut dsp, &[0x14, 0xFF, 0x00]); // length 0x00FF -> 256 samples
        assert!(dsp.is_playing());
        assert!(!dsp.is_auto_init());
        assert_eq!(dsp.block_remaining(), 256);
    }

    #[test]
    fn auto_init_command_marks_the_mode() {
        let mut dsp = SbDsp::default();
        write_cmd(&mut dsp, &[0x48, 0x00, 0x01]); // 0x0100 -> 256
        write_cmd(&mut dsp, &[0x1C]); // 8-bit auto-init
        assert!(dsp.is_playing() && dsp.is_auto_init());
    }

    #[test]
    fn render_sample_consumes_dma_bytes_and_edges_half_and_end_irqs() {
        let mut dsp = SbDsp::default();
        write_cmd(&mut dsp, &[0x41, 0x2B, 0x11]); // 11025 Hz
        write_cmd(&mut dsp, &[0x48, 0x07, 0x00]); // block size 8
        write_cmd(&mut dsp, &[0x1C]); // 8-bit auto-init
        let pattern = [0x00u8, 0x40, 0x80, 0xC0, 0x00, 0x40, 0x80, 0xC0];
        let mut irq_at: Vec<usize> = Vec::new();
        for i in 1..=8 {
            let byte = pattern[(i - 1) % pattern.len()];
            let _ = dsp.render_sample(move || Some(byte));
            if dsp.take_irq() {
                irq_at.push(i);
            }
        }
        // Half-buffer IRQ at the midpoint (4 consumed), end IRQ at TC (8 consumed).
        assert_eq!(irq_at, vec![4, 8], "half at 4, end at 8");
        // Auto-init reloads and keeps playing across terminal count.
        assert!(dsp.is_playing(), "auto-init keeps playing past TC");
    }

    #[test]
    fn single_mode_stops_at_end_of_block() {
        let mut dsp = SbDsp::default();
        write_cmd(&mut dsp, &[0x41, 0x2B, 0x11, 0x14, 0x01, 0x00]); // block 2, single
        let _ = dsp.render_sample(|| Some(0x80));
        let _ = dsp.render_sample(|| Some(0x80)); // TC -> single stops
        assert!(!dsp.is_playing(), "single mode halts after the block");
    }

    #[test]
    fn halt_continue_and_exit_auto_init_commands_control_playback() {
        let mut dsp = SbDsp::default();
        write_cmd(&mut dsp, &[0x48, 0x07, 0x00, 0x1C]); // auto-init, block 8
        assert!(dsp.is_playing() && dsp.is_auto_init());
        write_cmd(&mut dsp, &[0xD0]); // halt
        assert!(!dsp.is_playing());
        write_cmd(&mut dsp, &[0xD4]); // continue
        assert!(dsp.is_playing());
        write_cmd(&mut dsp, &[0xDA]); // exit auto-init
        assert!(!dsp.is_auto_init(), "exit-auto-init clears the mode");
    }

    #[test]
    fn sb16_16bit_auto_init_command_arms_with_mode_and_count() {
        let mut dsp = SbDsp::default();
        write_cmd(&mut dsp, &[0x41, 0x2B, 0x11]); // 11025 Hz
        // 0xB6 = 16-bit auto-init output; mode 0x30 = signed, stereo; count 7 -> 8 samples.
        write_cmd(&mut dsp, &[0xB6, 0x30, 0x07, 0x00]);
        assert!(dsp.is_playing() && dsp.is_auto_init());
        assert!(dsp.is_16bit());
        assert!(dsp.is_stereo());
        assert_eq!(dsp.block_remaining(), 8);
    }

    #[test]
    fn sb16_16bit_single_command_arms_non_auto_init() {
        let mut dsp = SbDsp::default();
        write_cmd(&mut dsp, &[0xB0, 0x00, 0x01, 0x00]); // single, mono, unsigned, count 2
        assert!(dsp.is_16bit());
        assert!(!dsp.is_stereo());
        assert!(!dsp.is_auto_init());
        assert_eq!(dsp.block_remaining(), 2);
    }

    #[test]
    fn sb16_16bit_input_command_arms_nothing() {
        // 0xB8 is the 16-bit A/D (input) command; ADC is out of scope, so it must
        // not arm playback even with well-formed arguments.
        let mut dsp = SbDsp::default();
        write_cmd(&mut dsp, &[0xB8, 0x30, 0x07, 0x00]);
        assert!(!dsp.is_playing());
        assert!(!dsp.is_16bit());
    }

    #[test]
    fn render_frame_16bit_signed_stereo_consumes_two_words() {
        let mut dsp = SbDsp::default();
        write_cmd(&mut dsp, &[0x41, 0x2B, 0x11]); // 11025 Hz
        // auto-init, signed, stereo, count 7 -> 8 samples = 4 stereo frames.
        write_cmd(&mut dsp, &[0xB6, 0x30, 0x07, 0x00]);
        let words = [
            0x0001u16, 0xFFFE, 0x7FFF, 0x8000, 0x0001, 0xFFFE, 0x7FFF, 0x8000,
        ];
        let mut i = 0;
        let mut frames = Vec::new();
        for _ in 0..4 {
            let f = dsp.render_frame(
                || panic!("8-bit fetch unused in 16-bit mode"),
                || {
                    let w = words[i % words.len()];
                    i += 1;
                    Some(w)
                },
            );
            frames.push(f);
        }
        assert_eq!(frames[0], Some((1, -2)), "signed little-endian L,R");
        assert!(dsp.is_playing(), "auto-init continues past TC");
    }

    #[test]
    fn render_frame_16bit_mono_duplicates_one_word_to_both_channels() {
        let mut dsp = SbDsp::default();
        // single, mono, signed: 0xB0 with mode 0x10 (bit4 = signed, bit5 clear = mono).
        write_cmd(&mut dsp, &[0xB0, 0x10, 0x01, 0x00]); // count 2 words
        let f = dsp.render_frame(
            || panic!("8-bit fetch unused in 16-bit mode"),
            || Some(0x7FFF),
        );
        assert_eq!(f, Some((32_767, 32_767)), "mono duplicates the word L/R");
    }

    #[test]
    fn render_frame_8bit_mono_duplicated_to_both_channels() {
        let mut dsp = SbDsp::default();
        write_cmd(&mut dsp, &[0x41, 0x2B, 0x11, 0x14, 0x01, 0x00]); // 8-bit mono single
        let f = dsp.render_frame(|| Some(0x80), || panic!("word fetch unused in 8-bit mode"));
        assert_eq!(f, Some((0, 0)), "0x80 -> silence on both channels");
    }

    #[test]
    fn high_speed_auto_init_command_0x90_arms_auto_init() {
        let mut dsp = SbDsp::default();
        write_cmd(&mut dsp, &[0x48, 0x07, 0x00]); // block size 8
        write_cmd(&mut dsp, &[0x90]); // SB Pro high-speed 8-bit auto-init
        assert!(dsp.is_playing() && dsp.is_auto_init());
        assert!(!dsp.is_16bit(), "high-speed 0x90 is an 8-bit mode");
    }

    #[test]
    fn high_speed_single_command_0x91_arms_single() {
        let mut dsp = SbDsp::default();
        write_cmd(&mut dsp, &[0x48, 0x07, 0x00]); // block size 8
        write_cmd(&mut dsp, &[0x91]); // SB Pro high-speed 8-bit single
        assert!(dsp.is_playing());
        assert!(!dsp.is_auto_init(), "high-speed 0x91 is single-cycle");
    }

    #[test]
    fn command_f2_raises_the_8bit_irq_immediately() {
        let mut dsp = SbDsp::default();
        assert!(!dsp.take_irq(), "no IRQ pending before the command");
        dsp.write_command_byte(0xF2);
        assert!(dsp.take_irq(), "F2 requests the 8-bit interrupt");
        assert!(!dsp.take_irq(), "take_irq clears the pending state");
    }

    #[test]
    fn reset_during_active_playback_clears_playing_and_auto_init() {
        let mut dsp = SbDsp::default();
        write_cmd(&mut dsp, &[0x48, 0x07, 0x00, 0x90]); // block 8, high-speed auto-init
        assert!(dsp.is_playing() && dsp.is_auto_init());
        // Render past the block midpoint so half_reached latches, and leave a
        // half-byte command partially assembled so `pending` is non-empty.
        for _ in 0..5 {
            let _ = dsp.render_sample(|| Some(0x80));
        }
        assert!(dsp.half_reached, "midpoint crossed before reset");
        let _ = dsp.take_irq(); // drop any IRQ raised by the half-buffer edge
        // Re-establish a pending IRQ and a partial command to prove reset clears them.
        dsp.irq_pending = true;
        dsp.write_command_byte(0x48); // arity-2 command, no args yet -> pending set
        assert!(dsp.pending.is_some(), "partial command queued before reset");
        // A DSP reset (write 0 to 0x226) halts playback, the way a game exits
        // high-speed mode.
        dsp.write_port(0x226, 0x00);
        assert!(!dsp.is_playing(), "reset halts playback");
        assert!(!dsp.is_auto_init(), "reset clears the auto-init latch");
        assert_eq!(dsp.block_remaining(), 0);
        assert!(!dsp.half_reached, "reset clears the half-buffer latch");
        assert!(dsp.pending.is_none(), "reset drops the partial command");
        // Real hardware clears the interrupt latch on reset, so a pre-reset
        // pending IRQ does not fire on the next re-armed playback.
        assert!(!dsp.take_irq(), "reset clears the pending IRQ latch");
        // rate/block-size are intentionally preserved across the halt-on-reset.
        assert_eq!(
            dsp.block_size, 8,
            "reset preserves the programmed block size"
        );
    }

    #[test]
    fn sbpro_8bit_stereo_consumes_two_bytes_and_yields_distinct_l_r() {
        let mut dsp = SbDsp::default();
        write_cmd(&mut dsp, &[0x14, 0x03, 0x00]); // block 4 bytes, 8-bit single
        assert!(!dsp.is_sbpro_stereo(), "SB Pro stereo off by default");
        dsp.set_sbpro_stereo(true);
        assert!(dsp.is_sbpro_stereo(), "set_sbpro_stereo(true) latches");
        // Left 0xFF (near full positive), right 0x00 (full negative) interleaved.
        let pattern = [0xFFu8, 0x00];
        let mut i = 0;
        let f = dsp.render_frame(
            || {
                let b = pattern[i % pattern.len()];
                i += 1;
                Some(b)
            },
            || panic!("word fetch unused in 8-bit stereo"),
        );
        assert_eq!(i, 2, "two bytes consumed per stereo frame");
        let (l, r) = f.expect("a stereo frame");
        assert!(
            l > 0 && r < 0,
            "distinct L/R from the byte pattern: {l},{r}"
        );
        assert_eq!(dsp.block_remaining(), 2, "block advanced by both bytes");
    }

    #[test]
    fn sbpro_8bit_stereo_edges_half_and_end_irqs_consuming_two_bytes_per_frame() {
        let mut dsp = SbDsp::default();
        // Block 4 bytes, 8-bit single, SB Pro stereo: advance_block(2) per frame,
        // so the block drains in 2 frames. Half fires when remaining <= 2 (after
        // frame 1), end fires when remaining == 0 (after frame 2), then single
        // mode stops.
        write_cmd(&mut dsp, &[0x14, 0x03, 0x00]); // block 4
        dsp.set_sbpro_stereo(true);
        let mut feed = || Some(0x80u8);
        // Frame 1: remaining 4 -> 2, half IRQ.
        assert!(dsp.render_frame(&mut feed, || panic!("no words")).is_some());
        assert_eq!(dsp.block_remaining(), 2);
        assert!(dsp.take_irq(), "half-buffer IRQ after frame 1");
        // Frame 2: remaining 2 -> 0, end IRQ, single mode stops.
        assert!(dsp.render_frame(&mut feed, || panic!("no words")).is_some());
        assert!(dsp.take_irq(), "end-buffer IRQ after frame 2");
        assert!(!dsp.is_playing(), "single mode stops at end of block");
    }

    #[test]
    fn high_speed_0x90_clears_stale_16bit_stereo_signed_latches() {
        let mut dsp = SbDsp::default();
        // First arm a 16-bit signed stereo auto-init mode (0xB6, mode 0x30).
        write_cmd(&mut dsp, &[0xB6, 0x30, 0x07, 0x00]);
        assert!(dsp.is_16bit() && dsp.is_stereo() && dsp.sample_signed);
        // A high-speed 8-bit command must reset those latches to the 8-bit
        // defaults; arm_dma clears them. The render path then pulls bytes.
        write_cmd(&mut dsp, &[0x48, 0x03, 0x00]); // block 4
        write_cmd(&mut dsp, &[0x90]); // high-speed auto-init 8-bit
        assert!(!dsp.is_16bit(), "0x90 clears the 16-bit latch");
        assert!(!dsp.is_stereo(), "0x90 clears the 16-bit stereo latch");
        assert!(!dsp.sample_signed, "0x90 clears the signed latch");
        // The 8-bit render path must run (pull a byte, never a word).
        let f = dsp.render_frame(|| Some(0x80), || panic!("word fetch unused in 8-bit mode"));
        assert_eq!(f, Some((0, 0)), "8-bit render path taken after 0x90");
    }

    #[test]
    fn output_frame_rate_halves_for_8bit_stereo_only() {
        let mut dsp = SbDsp::default();
        // 0x40 time constant programs the interleaved BYTE rate. tc for ~22.05k
        // byte rate: 256 - 1_000_000/22_050 = 256 - 45 = 211 (0xD3), giving
        // 1_000_000 / 45 = 22_222.
        write_cmd(&mut dsp, &[0x40, 0xD3]);
        let byte_rate = dsp.rate_hz();
        // 8-bit mono: per-channel rate is the programmed rate.
        write_cmd(&mut dsp, &[0x14, 0x00, 0x00]);
        assert_eq!(dsp.output_frame_rate(), byte_rate, "8-bit mono is unhalved");
        // 8-bit stereo: the time constant is the byte rate, so each channel halves.
        dsp.set_sbpro_stereo(true);
        assert_eq!(
            dsp.output_frame_rate(),
            byte_rate / 2,
            "8-bit stereo halves a time-constant (byte) rate"
        );
        // 16-bit stereo: the rate command programs the per-channel rate already,
        // so the SB Pro byte-interleave halving must not apply.
        write_cmd(&mut dsp, &[0xB6, 0x30, 0x07, 0x00]); // 16-bit signed stereo
        assert_eq!(dsp.rate_hz(), byte_rate, "16-bit stereo unchanged");
        assert_eq!(
            dsp.output_frame_rate(),
            byte_rate,
            "16-bit stereo unchanged"
        );
    }

    #[test]
    fn output_frame_rate_does_not_halve_a_0x41_rate_for_8bit_stereo() {
        // Per the SB16 guide, 0x41 programs the per-channel rate directly (no
        // channel-count pre-multiply), so SB Pro stereo must NOT halve it.
        let mut dsp = SbDsp::default();
        write_cmd(&mut dsp, &[0x41, 0x2B, 0x11]); // 0x2B11 = 11025 Hz, per-channel
        write_cmd(&mut dsp, &[0x14, 0x00, 0x00]); // 8-bit single
        dsp.set_sbpro_stereo(true);
        assert_eq!(
            dsp.output_frame_rate(),
            11_025,
            "a 0x41 per-channel rate is not halved for SB Pro stereo"
        );
    }

    #[test]
    fn reading_0x22f_acks_the_16bit_irq() {
        let mut dsp = SbDsp::default();
        write_cmd(&mut dsp, &[0x41, 0x2B, 0x11, 0xB6, 0x30, 0x00, 0x00]); // count 1
        let mut w = 0u16;
        let _ = dsp.render_frame(
            || None,
            || {
                w = w.wrapping_add(1);
                Some(w)
            },
        );
        // end-of-buffer IRQ pending; 0x22F acks it.
        dsp.read_port(0x22F);
        assert!(!dsp.take_irq(), "0x22F cleared the pending IRQ");
    }

    // ---- Creative ADPCM ----------------------------------------------------

    #[test]
    fn creative_adpcm4_silence_holds_the_reference() {
        // Reference 0x80 with all-zero 4-bit codes stays at 0x80 (centered
        // silence): code 0 adds scaleMap[0]=0 and adjustMap[0]=0, so neither the
        // reference nor the step index moves.
        let mut st = AdpcmState::new(AdpcmMode::Bits4, false);
        st.reference = 0x80;
        for _ in 0..4 {
            st.decode_byte(0x00);
        }
        assert!(
            st.buf.iter().all(|&s| s == 0x80),
            "0x80 + zero codes stays silent: {:?}",
            st.buf
        );
    }

    #[test]
    fn creative_adpcm4_decode_matches_reference_arithmetic() {
        // Byte 0x50 after reference 0x80 (step 0): hi nibble 5, lo nibble 0.
        //  code 5, step 0: samp 5, scaleMap[5]=5  -> ref 0x85 (133); step += 16.
        //  code 0, step 16: samp 16, scaleMap[16]=1 -> ref 134; adjustMap[16]=240
        //   wraps the step index (16+240)&0xff back to 0.
        let mut st = AdpcmState::new(AdpcmMode::Bits4, false);
        st.reference = 0x80;
        st.decode_byte(0x50);
        assert_eq!(st.buf.pop_front(), Some(133));
        assert_eq!(st.buf.pop_front(), Some(134));
        assert_eq!(st.step, 0, "adjustMap[16]=240 wraps step 16 to 0");
    }

    #[test]
    fn adpcm4_reference_command_seeds_predictor_and_counts_encoded_bytes() {
        // 0x75 = 4-bit ADPCM with reference, inline length. Length 0x0002 means
        // 3 DMA bytes: the reference seed plus two encoded bytes.
        let mut dsp = SbDsp::default();
        write_cmd(&mut dsp, &[0x75, 0x02, 0x00]);
        assert!(dsp.is_playing());
        assert_eq!(dsp.block_remaining(), 3, "block counts encoded DMA bytes");
        // Stream: reference 0x80 then two silence bytes.
        let stream = [0x80u8, 0x00, 0x00];
        let mut i = 0;
        let mut fetch = || {
            let b = stream.get(i).copied();
            i += 1;
            b
        };
        // First frame consumes the reference seed (no sample) then decodes the
        // first encoded byte: 0x80 -> centered silence, so both channels are 0.
        let f = dsp.render_frame(&mut fetch, || None);
        assert_eq!(f, Some((0, 0)), "0x80 decodes to centered silence");
        assert_eq!(
            dsp.block_remaining(),
            1,
            "reference seed + one encoded byte both drained the counter"
        );
    }

    #[test]
    fn adpcm4_auto_init_uses_the_0x48_block_size() {
        // 0x7D = 4-bit auto-init ADPCM: no inline length, block size from 0x48.
        let mut dsp = SbDsp::default();
        write_cmd(&mut dsp, &[0x48, 0x03, 0x00]); // 4 encoded bytes
        write_cmd(&mut dsp, &[0x7D]);
        assert!(dsp.is_playing() && dsp.is_auto_init());
        assert_eq!(dsp.block_remaining(), 4);
    }

    #[test]
    fn pcm_arm_clears_a_prior_adpcm_transfer() {
        // Arming a plain PCM transfer after an ADPCM one must drop the decode
        // state, or the raw-byte path stays stuck decoding.
        let mut dsp = SbDsp::default();
        write_cmd(&mut dsp, &[0x75, 0x02, 0x00]);
        assert!(dsp.adpcm.is_some(), "ADPCM armed");
        write_cmd(&mut dsp, &[0x14, 0x01, 0x00]); // 8-bit PCM single
        assert!(dsp.adpcm.is_none(), "PCM arm dropped ADPCM state");
        let f = dsp.render_frame(|| Some(0x80), || None);
        assert_eq!(f, Some((0, 0)), "raw 0x80 PCM byte -> silence");
    }

    #[test]
    fn dsp_reset_clears_adpcm_state() {
        let mut dsp = SbDsp::default();
        write_cmd(&mut dsp, &[0x75, 0x02, 0x00]);
        assert!(dsp.adpcm.is_some());
        dsp.write_port(0x226, 0x00); // reset settle
        assert!(dsp.adpcm.is_none(), "reset drops ADPCM state");
    }
}
