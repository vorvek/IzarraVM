//! Sound Blaster 16-class DSP (CT1747) clean-room core: reset handshake,
//! command/data protocol, and 8-bit single/auto-init DMA playback. 16-bit,
//! ADPCM, input/ADC, MIDI and the CT1745 mixer are out of scope for this slice.

use std::collections::VecDeque;

pub const DSP_VERSION_HI: u8 = 4;
pub const DSP_VERSION_LO: u8 = 5;

/// One DSP. The reset port (0x226) drives a microsecond countdown; when it
/// elapses the DSP queues 0xAA on read-data and asserts data-available.
#[derive(Debug, Clone, PartialEq)]
pub struct SbDsp {
    reset_micros: Option<f64>,
    read_data: VecDeque<u8>,
    data_available: bool,
    // Command interpreter: bytes written to 0x22C stream in here.
    pending: Option<PendingCommand>,
    // Immediate-command state.
    direct_dac_byte: Option<u8>,
    test_reg: u8,
    speaker_on: bool,
    // 8-bit DMA playback state (Tasks 5-6).
    rate_hz: u32,
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
            pending: None,
            direct_dac_byte: None,
            test_reg: 0,
            speaker_on: false,
            rate_hz: 22_050,
            block_size: 0,
            block_remaining: 0,
            auto_init: false,
            playing: false,
            irq_pending: false,
            half_reached: false,
            dma_16bit: false,
            stereo: false,
            sample_signed: false,
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
            0x40 => 1,        // set time constant (Task 5)
            0x41 => 2,        // set sample rate (Task 5)
            0x48 => 2,        // set block size (Task 5)
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
                // Set time constant: rate = 1_000_000 / (256 - tc).
                if let Some(&tc) = args.first() {
                    let divisor = 256u32.wrapping_sub(u32::from(tc));
                    if let Some(rate) = 1_000_000u32.checked_div(divisor) {
                        self.rate_hz = rate;
                    }
                }
            }
            0x41 => {
                // Set sample rate in Hz, high byte then low byte (SB16).
                if args.len() >= 2 {
                    self.rate_hz = (u32::from(args[0]) << 8) | u32::from(args[1]);
                }
            }
            0x48 => {
                // Set DSP block transfer size, low byte then high byte (n+1 bytes).
                if args.len() >= 2 {
                    let count = (u32::from(args[0]) | (u32::from(args[1]) << 8)) + 1;
                    self.block_size = count;
                }
            }
            0x14 | 0x90 => self.arm_dma(false),
            0x1C => self.arm_dma(true),
            0xB0..=0xBF => self.arm_16bit(command, args),
            0xD0 => self.playing = false,   // halt DMA (position kept)
            0xD4 => self.playing = true,    // continue DMA
            0xDA => self.auto_init = false, // exit auto-init: stop at next TC
            _ => {}
        }
    }

    fn arm_dma(&mut self, auto_init: bool) {
        // 8-bit DMA is mono unsigned PCM: clear the 16-bit/stereo/signed latches.
        self.dma_16bit = false;
        self.stereo = false;
        self.sample_signed = false;
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

    pub fn block_remaining(&self) -> u32 {
        self.block_remaining
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
        } else {
            let s = sample_u8(byte_fetch()?);
            self.advance_block(1);
            Some((s, s))
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

    pub fn read_port(&mut self, port: u16) -> Option<u8> {
        match port {
            0x22A => {
                let byte = self.read_data.pop_front().unwrap_or(0xAA);
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

/// Convert one 8-bit Sound Blaster PCM sample (unsigned) to a centered signed
/// 16-bit value for the mixer: (byte - 128) * 256.
fn sample_u8(byte: u8) -> i16 {
    (i32::from(byte) - 128).clamp(-128, 127) as i16 * 256
}

/// Convert one signed 16-bit DMA sample directly (no centering): the SB16 16-bit
/// path is already signed PCM, so the bit pattern maps straight to i16.
fn sample_i16(word: u16) -> i16 {
    word as i16
}

/// Convert one unsigned 16-bit DMA sample (rare, mode-byte-selected) by
/// re-centering around 0x8000: the upper half (>= 0x8000) maps to 0..=32767 and
/// the lower half wraps to -32768..=-1.
fn sample_u16(word: u16) -> i16 {
    word.wrapping_sub(0x8000) as i16
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
    fn dma_single_output_arms_with_block_size() {
        let mut dsp = SbDsp::default();
        write_cmd(&mut dsp, &[0x48, 0xFF, 0x00]); // block size 0x00FF -> 256
        write_cmd(&mut dsp, &[0x14]); // 8-bit DMA single output
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
        write_cmd(&mut dsp, &[0x41, 0x2B, 0x11, 0x48, 0x01, 0x00, 0x14]); // block 2, single
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
    fn sample_u8_centers_unsigned_bytes() {
        assert_eq!(sample_u8(0x00), -32_768, "0x00 -> full negative");
        assert_eq!(sample_u8(0x80), 0, "0x80 -> silence");
        assert_eq!(sample_u8(0xFF), 32_512, "0xFF -> near full positive");
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
        write_cmd(&mut dsp, &[0x41, 0x2B, 0x11, 0x48, 0x01, 0x00, 0x14]); // 8-bit mono single
        let f = dsp.render_frame(|| Some(0x80), || panic!("word fetch unused in 8-bit mode"));
        assert_eq!(f, Some((0, 0)), "0x80 -> silence on both channels");
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
}
