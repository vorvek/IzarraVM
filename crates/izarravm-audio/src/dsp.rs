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
            0xD0 => self.playing = false,   // halt DMA (position kept)
            0xD4 => self.playing = true,    // continue DMA
            0xDA => self.auto_init = false, // exit auto-init: stop at next TC
            _ => {}
        }
    }

    fn arm_dma(&mut self, auto_init: bool) {
        self.auto_init = auto_init;
        self.playing = true;
        self.block_remaining = self.block_size;
        self.half_reached = false;
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

    pub fn block_remaining(&self) -> u32 {
        self.block_remaining
    }

    /// Produce one output sample for the current DMA byte, or None if idle.
    /// `fetch` pulls the next DMA byte (the machine feeds the 8237A through it).
    pub fn render_sample<F: FnMut() -> Option<u8>>(&mut self, mut fetch: F) -> Option<i16> {
        if !self.playing {
            return self.direct_dac_byte.map(sample_u8);
        }
        let byte = fetch()?;
        self.block_remaining = self.block_remaining.saturating_sub(1);
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
        Some(sample_u8(byte))
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
            0x22E => Some(if self.data_available { 0x80 } else { 0x00 }),
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
}
