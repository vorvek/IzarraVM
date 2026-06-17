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
            0x10 | 0xE4 => 1,                         // direct DAC / test-register write
            0x40 => 1,                                // set time constant (Task 5)
            0x41 => 2,                                // set sample rate (Task 5)
            0x48 => 2,                                // set block size (Task 5)
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
            // Rate / block / DMA commands are wired in Tasks 5 and 6.
            _ => {}
        }
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
        assert!(!dsp.write_port(0x224, 0x00), "mixer (0x224) stays out of scope");
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
}
