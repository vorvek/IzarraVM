// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Sound Blaster 16-class DSP (CT1747) clean-room core: reset handshake,
//! command/data protocol, 8-bit plus 16-bit single/auto-init DMA playback, and
//! Creative ADPCM (4-bit, 2.6-bit, 2-bit) decode on the 8-bit DMA path. The
//! CT1745 mixer lives next to this in the machine crate. Input/ADC is not
//! modeled. MIDI and MPU-401 support lives in the sibling modules.

use crate::pcm::{push_frame_capped, sample_i8, sample_i16, sample_u8, sample_u16};
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
    reset_micros: Option<u64>,
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
    // 8-bit DMA playback state.
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
    // 16-bit DMA playback state (SB16 0xBx family). dma_16bit selects the word
    // fetch and sample-depth path; stereo selects one vs. two words per frame;
    // sample_signed selects signed vs. unsigned 16-bit conversion.
    dma_16bit: bool,
    stereo: bool,
    sample_signed: bool,
    // A stereo left sample whose matching right DMA unit was not available yet.
    pending_stereo_left: Option<i16>,
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
    /// Frames evicted from `rendered` because the host path did not drain it
    /// fast enough. Diagnostic only; never gates behavior.
    dropped_frames: u64,
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
            dma_16bit: false,
            stereo: false,
            sample_signed: false,
            pending_stereo_left: None,
            sbpro_stereo: false,
            adpcm: None,
            rendered: VecDeque::new(),
            dropped_frames: 0,
        }
    }
}

impl SbDsp {
    /// Advance the DSP's reset-settle countdown by `micros` microseconds. When
    /// the countdown elapses the DSP queues 0xAA on read-data.
    pub fn advance_micros(&mut self, micros: u64) {
        if let Some(remaining) = self.reset_micros.as_mut() {
            *remaining = remaining.saturating_sub(micros);
            if *remaining == 0 {
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
            // The SB16 0xBx/0xCx families (16-bit/8-bit DMA output/input, single
            // + auto-init) take a mode byte plus a 2-byte transfer count.
            0xB0..=0xBF => 3,
            0xC0..=0xCF => 3,
            _ => 0,
        }
    }

    /// Push a command/data byte into the interpreter; dispatches when complete.
    /// Log every fully-assembled DSP command (`IZARRAVM_SB_CMD_TRACE`), with the
    /// playback state it leaves behind.
    ///
    /// The per-second `[SB]` trace shows what the DSP is DOING; this shows what
    /// the guest ASKED for, which is the half you need when a driver streams by
    /// re-arming single-cycle blocks and simply stops. The read-port histogram
    /// cannot answer it either, because commands are port WRITES.
    ///
    /// Commands arrive at a handful per block, so the cost here is irrelevant;
    /// the gate is still read once rather than per call.
    fn trace_command(&mut self, command: u8, args: &[u8]) {
        use std::sync::OnceLock;
        use std::sync::atomic::{AtomicU32, Ordering};
        static ENABLED: OnceLock<bool> = OnceLock::new();
        static SEEN: AtomicU32 = AtomicU32::new(0);
        const LIMIT: u32 = 400;
        if !*ENABLED.get_or_init(|| std::env::var_os("IZARRAVM_SB_CMD_TRACE").is_some()) {
            return;
        }
        let n = SEEN.fetch_add(1, Ordering::Relaxed);
        if n >= LIMIT {
            return;
        }
        eprintln!(
            "[SBCMD {n}] cmd={command:#04x} args={args:02x?} -> playing={} auto_init={} \
             bits={} stereo={} rate={} block={} remaining={} irq_pending={}",
            self.playing,
            self.auto_init,
            if self.dma_16bit { 16 } else { 8 },
            self.stereo,
            self.rate_hz,
            self.block_size,
            self.block_remaining,
            self.irq_pending,
        );
    }

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
        self.trace_command(command, args);
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
            0x41 if args.len() >= 2 => {
                // Set sample rate in Hz, high byte then low byte (SB16). Unlike
                // the time constant, this is already the per-channel rate for
                // stereo (no channel-count pre-multiply), so it is not a byte
                // rate and must not be halved for SB Pro stereo.
                self.rate_hz = (u32::from(args[0]) << 8) | u32::from(args[1]);
                self.rate_is_byte_rate = false;
            }
            0x41 => {}
            0x48 if args.len() >= 2 => {
                // Set DSP block transfer size, low byte then high byte (n+1 bytes).
                let count = (u32::from(args[0]) | (u32::from(args[1]) << 8)) + 1;
                self.block_size = count;
            }
            0x48 => {}
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
            0xC0..=0xCF => self.arm_8bit_sb16(command, args),
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
        self.arm_8bit(auto_init, false, false);
    }

    fn arm_8bit(&mut self, auto_init: bool, stereo: bool, signed: bool) {
        self.dma_16bit = false;
        self.stereo = stereo;
        self.sample_signed = signed;
        self.pending_stereo_left = None;
        self.adpcm = None;
        self.auto_init = auto_init;
        self.playing = true;
        self.block_remaining = self.block_size;
    }

    /// Arm the 8-bit DMA path as a Creative ADPCM transfer. Mono unsigned like
    /// `arm_dma`, but the fetched bytes are decoded through `AdpcmState`. The
    /// block counter still counts encoded DMA bytes (including the reference
    /// seed byte), so the programmed-block IRQ lands exactly as on the raw 8-bit path.
    fn arm_adpcm(&mut self, mode: AdpcmMode, haveref: bool, auto_init: bool) {
        self.dma_16bit = false;
        self.stereo = false;
        self.sample_signed = false;
        self.pending_stereo_left = None;
        self.adpcm = Some(AdpcmState::new(mode, haveref));
        self.auto_init = auto_init;
        self.playing = true;
        self.block_remaining = self.block_size;
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
        self.pending_stereo_left = None;
        self.auto_init = auto_init;
        self.block_size = count;
        self.block_remaining = count;
        self.playing = true;
    }

    /// Arm the SB16 8-bit DMA path from a 0xCx command (mode byte + 2-byte
    /// count). Bit2 selects auto-init, bit3 selects input/ADC (ignored here),
    /// and the mode byte selects signed/stereo playback.
    fn arm_8bit_sb16(&mut self, command: u8, args: &[u8]) {
        if command & 0x08 != 0 {
            return;
        }
        let auto_init = command & 0x04 != 0;
        let mode = args.first().copied().unwrap_or(0);
        let stereo = mode & 0x20 != 0;
        let signed = mode & 0x10 != 0;
        let count_lo = u32::from(args.get(1).copied().unwrap_or(0));
        let count_hi = u32::from(args.get(2).copied().unwrap_or(0));
        self.block_size = (count_lo | (count_hi << 8)) + 1;
        self.arm_8bit(auto_init, stereo, signed);
    }

    pub fn rate_hz(&self) -> u32 {
        self.rate_hz
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Whether the output clock still has PCM to produce. Creative ADPCM can
    /// retain decoded samples after its final encoded DMA byte stops playback.
    pub fn needs_output_tick(&self) -> bool {
        self.playing
            || self
                .adpcm
                .as_ref()
                .is_some_and(|state| !state.buf.is_empty())
    }

    pub fn is_auto_init(&self) -> bool {
        self.auto_init
    }

    /// Whether the armed DMA mode is the SB16 16-bit (0xBx) path.
    pub fn is_16bit(&self) -> bool {
        self.dma_16bit
    }

    /// Whether the armed DMA mode is stereo.
    pub fn is_stereo(&self) -> bool {
        self.stereo
    }

    /// Set the SB Pro 8-bit stereo flag from the mixer (register 0x0E bit1).
    pub fn set_sbpro_stereo(&mut self, on: bool) {
        if !on && !self.dma_16bit && !self.stereo {
            self.pending_stereo_left = None;
        }
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

    pub fn block_size(&self) -> u32 {
        self.block_size
    }

    /// Advance the DMA playback by exactly one stereo frame and push the result
    /// onto the rendered-frame ring. This is the per-CPU-clock producer entry
    /// point: it wraps the existing [`render_frame`] (which advances the block
    /// counter and edges the programmed-block IRQ) and buffers the frame for the host
    /// drainer. A `None` frame (channel idle or DMA exhausted) is not pushed.
    /// The IRQ raised inside `render_frame` is left pending for the caller to
    /// forward to the PIC via [`take_irq`]. Returns whether a frame was produced.
    pub fn tick_sample<B, W>(&mut self, byte_fetch: B, word_fetch: W) -> bool
    where
        B: FnMut() -> Option<u8>,
        W: FnMut() -> Option<u16>,
    {
        if let Some(frame) = self.render_frame(byte_fetch, word_fetch) {
            if push_frame_capped(&mut self.rendered, frame) {
                self.dropped_frames = self.dropped_frames.saturating_add(1);
            }
            true
        } else {
            false
        }
    }

    /// Batch tick for HLE: produce up to `n` frames, stopping on a dry source and
    /// returning the number produced. Used by the machine's phase accounting.
    pub fn tick_n_samples<B, W>(&mut self, n: usize, mut byte_fetch: B, mut word_fetch: W) -> usize
    where
        B: FnMut() -> Option<u8>,
        W: FnMut() -> Option<u16>,
    {
        let mut produced = 0;
        while produced < n && self.tick_sample(&mut byte_fetch, &mut word_fetch) {
            produced += 1;
        }
        produced
    }

    /// Pop the oldest rendered stereo frame for the host audio path, or None
    /// when the ring is empty (silent DSP = OPL passthrough).
    pub fn drain_frame(&mut self) -> Option<(i16, i16)> {
        self.rendered.pop_front()
    }

    /// Frames evicted from the render ring since power-on. Diagnostic only.
    pub fn dropped_frames(&self) -> u64 {
        self.dropped_frames
    }

    /// Output frames until the next block-completion IRQ edge. PCM stereo drains two block units
    /// per frame; mono drains one. Creative ADPCM fetch cadence
    /// depends on its decoded-sample FIFO, so it returns the earliest causal
    /// deadline of one frame rather than risking a late interrupt.
    pub fn frames_until_next_irq(&self) -> Option<u64> {
        if !self.playing {
            return None;
        }
        if self.adpcm.is_some() {
            return Some(1);
        }
        let units = u64::from(self.block_remaining.max(1));
        if self.stereo || (!self.dma_16bit && self.sbpro_stereo) {
            let pending = u64::from(self.pending_stereo_left.is_some());
            Some((units + pending).div_ceil(2))
        } else {
            Some(units)
        }
    }

    /// Produce one stereo output frame for the current DMA mode, or None if the
    /// channel is idle. `byte_fetch` feeds the 8-bit DMA path and `word_fetch`
    /// the 16-bit path; only the one matching the armed mode is pulled. Mono
    /// modes duplicate their single sample to both channels. `block_remaining`
    /// advances after each successful DMA unit: a byte on the 8-bit path or a
    /// word on the 16-bit path. The programmed-block IRQ follows those units.
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
            if !self.stereo {
                let sample = self.sample_word(word_fetch()?);
                self.advance_block(1);
                return Some((sample, sample));
            }
            let left = if let Some(left) = self.pending_stereo_left.take() {
                left
            } else {
                let left = self.sample_word(word_fetch()?);
                self.advance_block(1);
                if !self.playing {
                    return None;
                }
                left
            };
            let Some(word) = word_fetch() else {
                if self.playing {
                    self.pending_stereo_left = Some(left);
                }
                return None;
            };
            let right = self.sample_word(word);
            self.advance_block(1);
            Some((left, right))
        } else if self.stereo || self.sbpro_stereo {
            // SB Pro 8-bit stereo: two interleaved bytes per frame, left then
            // right, advancing the block counter by both bytes consumed.
            // The SB Pro silent-byte priming / L<->R channel-swap alignment quirk
            // is not modeled, so the first byte of each frame is always Left.
            let left = if let Some(left) = self.pending_stereo_left.take() {
                left
            } else {
                let left = self.sample_byte(byte_fetch()?);
                self.advance_block(1);
                if !self.playing {
                    return None;
                }
                left
            };
            let Some(byte) = byte_fetch() else {
                if self.playing {
                    self.pending_stereo_left = Some(left);
                }
                return None;
            };
            let right = self.sample_byte(byte);
            self.advance_block(1);
            Some((left, right))
        } else {
            let s = self.sample_byte(byte_fetch()?);
            self.advance_block(1);
            Some((s, s))
        }
    }

    /// Produce up to `n` HLE frames from one elapsed-time batch.
    pub fn render_n_frames<B, W>(
        &mut self,
        n: usize,
        mut byte_fetch: B,
        mut word_fetch: W,
    ) -> Vec<(i16, i16)>
    where
        B: FnMut() -> Option<u8>,
        W: FnMut() -> Option<u16>,
    {
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            if let Some(f) = self.render_frame(&mut byte_fetch, &mut word_fetch) {
                out.push(f);
            } else {
                break;
            }
        }
        out
    }

    /// Pop the next decoded Creative ADPCM sample, fetching and decoding encoded
    /// DMA bytes as needed. Each fetched byte advances the block counter by one
    /// (bytes, not decoded samples, are what the DMA length counts) and edges the
    /// programmed-block IRQ. The reference-init byte seeds the predictor and yields no
    /// sample, so the loop fetches again. Returns None when the FIFO is empty and
    /// the channel is stopped or the DMA source is starved; buffered samples from
    /// the final byte still drain even after the block ends.
    fn pop_adpcm_sample<B>(&mut self, byte_fetch: &mut B) -> Option<u8>
    where
        B: FnMut() -> Option<u8>,
    {
        loop {
            {
                let state = self.adpcm.as_mut()?;
                if let Some(sample) = state.buf.pop_front() {
                    return Some(sample);
                }
            }
            if !self.playing {
                return None;
            }
            let byte = byte_fetch()?;
            self.advance_block(1);
            {
                let state = self.adpcm.as_mut()?;
                if state.haveref {
                    state.haveref = false;
                    state.reference = byte;
                    state.step = 0;
                } else {
                    state.decode_byte(byte);
                }
            }
        }
    }

    /// Per-channel output frame rate. The SB Pro time constant (0x40) programs
    /// the interleaved BYTE rate, so in 8-bit stereo each channel runs at half
    /// that. The 0x41 set-sample-rate command instead programs the per-channel
    /// rate directly (no channel-count pre-multiply), so it must not be halved.
    /// Every other mode (mono, or any 16-bit) frames at the programmed rate.
    pub fn output_frame_rate(&self) -> u32 {
        if (self.sbpro_stereo || self.stereo) && !self.dma_16bit && self.rate_is_byte_rate {
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

    fn sample_byte(&self, byte: u8) -> i16 {
        if self.sample_signed {
            sample_i8(byte)
        } else {
            sample_u8(byte)
        }
    }

    /// Consume DMA units and edge the block-completion IRQ. A stereo frame may cross an
    /// odd-sized auto-init block, so units left after the reload belong to the
    /// next block instead of being lost at the first programmed block boundary.
    fn advance_block(&mut self, mut consumed: u32) {
        while consumed > 0 && self.playing {
            if self.block_remaining == 0 {
                self.irq_pending = true;
                if self.auto_init && self.block_size > 0 {
                    self.block_remaining = self.block_size;
                } else {
                    self.playing = false;
                    break;
                }
            }

            let step = consumed.min(self.block_remaining);
            self.block_remaining -= step;
            consumed -= step;

            if self.block_remaining == 0 {
                self.irq_pending = true;
                if self.auto_init && self.block_size > 0 {
                    self.block_remaining = self.block_size;
                } else {
                    self.playing = false;
                }
            }
        }
    }

    /// Mono wrapper over [`render_frame`] for the 8-bit path (kept so the 8-bit
    /// unit tests stay green). Returns the single channel duplicated L/R as one
    /// i16.
    pub fn render_sample<F: FnMut() -> Option<u8>>(&mut self, mut fetch: F) -> Option<i16> {
        self.render_frame(&mut fetch, || None).map(|(l, _)| l)
    }

    /// Take and clear a pending block-completion IRQ (cleared when the host reads 0x22E).
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
            // 0x22C reads the write-buffer status; bit 7 clear means ready.
            // Commands dispatch synchronously in this model, so it is never busy.
            0x22C => Some(0x00),
            // 0x22E is the 8-bit read-buffer status port and the 8-bit DMA
            // interrupt-acknowledge port; 0x22F is its 16-bit counterpart. Only
            // one DMA mode runs at a time, so a read of either status port clears
            // the pending block-completion IRQ.
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
                    self.reset_micros = Some(0);
                } else {
                    self.reset_micros = Some(100);
                    self.read_data.clear();
                    self.data_available = false;
                    // Real hardware halts playback and clears the interrupt
                    // latch on reset; clear the DMA state so a high-speed game's
                    // reset stops the channel cleanly. Clearing irq_pending here
                    // (and never re-arming it in arm_dma/arm_16bit) prevents a
                    // block IRQ that went pending before the reset from firing
                    // spuriously on the next re-armed playback. rate_hz and
                    // block_size are intentionally preserved: this is the
                    // halt-on-reset behavior, not a power-on parameter wipe.
                    self.playing = false;
                    self.auto_init = false;
                    self.block_remaining = 0;
                    self.irq_pending = false;
                    self.pending = None;
                    // The mode latches must clear too, or the idle 16-bit
                    // guard in render_frame keeps direct DAC dead after any
                    // 16-bit session.
                    self.dma_16bit = false;
                    self.stereo = false;
                    self.sample_signed = false;
                    self.pending_stereo_left = None;
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
#[path = "dsp_test.rs"]
mod tests;
