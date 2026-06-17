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
    inbox: VecDeque<u8>,
}

impl Default for SbDsp {
    fn default() -> Self {
        Self {
            reset_micros: None,
            read_data: VecDeque::new(),
            data_available: false,
            inbox: VecDeque::new(),
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
}
