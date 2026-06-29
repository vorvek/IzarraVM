//! PS/2 auxiliary (mouse) device, as the 8042 controller multiplexes it.
//! Tracks the reporting enable, the queued data bytes, and the
//! sample-rate/resolution/scaling state the driver sets up during detection.

use std::collections::VecDeque;

/// A standard PS/2 (three-byte) mouse. Tracks the reporting enable, the queued
/// data bytes, and the sample-rate/resolution/scaling state the driver sets up
/// during detection. Movement and button changes queue a three-byte packet and
/// (when reporting is on) raise IRQ12 through the controller.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Ps2Mouse {
    pub(crate) queue: VecDeque<u8>, // bytes waiting to be moved into the aux output buffer
    pub(crate) reporting: bool,     // data-reporting enabled (command 0xF4 on / 0xF5 off)
    sample_rate: u8,                // last value set by 0xF3 (sample rate, Hz)
    resolution: u8,                 // last value set by 0xE8 (counts per mm code 0..3)
    scaling_2to1: bool,             // 2:1 scaling (0xE7 on / 0xE6 off)
    buttons: u8,                    // current button bitmask (bit0 left, bit1 right, bit2 middle)
    expecting_data: Option<u8>,     // a mouse command awaiting its parameter byte
}

impl Default for Ps2Mouse {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            reporting: false,
            sample_rate: 100,
            resolution: 2,
            scaling_2to1: false,
            buttons: 0,
            expecting_data: None,
        }
    }
}

impl Ps2Mouse {
    /// Handle a byte the guest wrote to the mouse (via the controller's 0xD4
    /// path). Most commands queue an ACK (0xFA); a parameter-taking command
    /// (set sample rate / resolution) records the next byte as its parameter.
    pub(crate) fn write_byte(&mut self, value: u8) {
        if let Some(cmd) = self.expecting_data.take() {
            match cmd {
                // 0xF3 set sample rate, 0xE8 set resolution: record the parameter.
                0xF3 => self.sample_rate = value,
                0xE8 => self.resolution = value,
                _ => {}
            }
            self.queue.push_back(0xFA);
            return;
        }
        match value {
            0xFF => {
                // Reset: ACK, then self-test pass (0xAA) and the device id (0x00).
                self.reporting = false;
                self.sample_rate = 100;
                self.resolution = 2;
                self.scaling_2to1 = false;
                self.queue.push_back(0xFA);
                self.queue.push_back(0xAA);
                self.queue.push_back(0x00);
            }
            0xF6 => {
                // Set defaults.
                self.reporting = false;
                self.sample_rate = 100;
                self.resolution = 2;
                self.scaling_2to1 = false;
                self.queue.push_back(0xFA);
            }
            0xF4 => {
                self.reporting = true;
                self.queue.push_back(0xFA);
            }
            0xF5 => {
                self.reporting = false;
                self.queue.push_back(0xFA);
            }
            0xF3 | 0xE8 => {
                // Set sample rate / resolution: ACK now, value arrives next byte.
                self.expecting_data = Some(value);
                self.queue.push_back(0xFA);
            }
            0xE7 => {
                self.scaling_2to1 = true;
                self.queue.push_back(0xFA);
            }
            0xE6 => {
                self.scaling_2to1 = false;
                self.queue.push_back(0xFA);
            }
            0xE9 => {
                // Status request: ACK then a three-byte status packet.
                self.queue.push_back(0xFA);
                let mut byte0 = 0u8;
                if self.scaling_2to1 {
                    byte0 |= 0x10;
                }
                if self.reporting {
                    byte0 |= 0x20;
                }
                byte0 |= self.buttons & 0x07;
                self.queue.push_back(byte0);
                self.queue.push_back(self.resolution);
                self.queue.push_back(self.sample_rate);
            }
            0xF2 => {
                // Get device id: ACK then 0x00 (standard PS/2 mouse).
                self.queue.push_back(0xFA);
                self.queue.push_back(0x00);
            }
            _ => self.queue.push_back(0xFA), // ack anything else
        }
    }

    /// Queue a standard three-byte movement packet for `dx`/`dy` (host pixels,
    /// y down positive) and the button mask. Returns true if reporting is enabled
    /// so the controller can raise IRQ12. Movement while reporting is off is
    /// dropped, matching a real mouse that holds its line idle until enabled.
    pub(crate) fn queue_movement(&mut self, dx: i32, dy: i32, buttons: u8) -> bool {
        self.buttons = buttons & 0x07;
        if !self.reporting {
            return false;
        }
        // Clamp to the 9-bit two's-complement range the packet carries.
        let cx = dx.clamp(-256, 255);
        // PS/2 reports +y as up; screen-space dy is +down, so negate.
        let cy = (-dy).clamp(-256, 255);
        let mut byte0 = 0x08 | (buttons & 0x07); // bit3 always set
        if cx < 0 {
            byte0 |= 0x10; // X sign
        }
        if cy < 0 {
            byte0 |= 0x20; // Y sign
        }
        self.queue.push_back(byte0);
        self.queue.push_back((cx & 0xff) as u8);
        self.queue.push_back((cy & 0xff) as u8);
        true
    }
}
