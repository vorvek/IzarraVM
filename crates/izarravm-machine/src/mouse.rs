//! PS/2 auxiliary (mouse) device, as the 8042 controller multiplexes it.
//! Models a Microsoft IntelliMouse: it powers up as a standard three-byte mouse
//! (id 0x00) and switches to four-byte wheel mode (id 0x03) once the driver
//! plays the 200/100/80 sample-rate "magic knock". A reset or set-defaults drops
//! it back to three bytes. Tracks the reporting enable, the queued data bytes,
//! and the sample-rate/resolution/scaling state the driver sets up during
//! detection.

use std::collections::VecDeque;

/// The PS/2 mouse device state. Movement and button changes queue a packet
/// (three bytes, or four with a Z wheel byte in IntelliMouse mode) and, when
/// reporting is on, raise IRQ12 through the controller. The sample-rate history
/// detects the IntelliMouse knock, which flips `intellimouse`/`device_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ps2Mouse {
    pub(crate) queue: VecDeque<u8>, // bytes waiting to be moved into the aux output buffer
    pub(crate) reporting: bool,     // data-reporting enabled (command 0xF4 on / 0xF5 off)
    sample_rate: u8,                // last value set by 0xF3 (sample rate, Hz)
    resolution: u8,                 // last value set by 0xE8 (counts per mm code 0..3)
    scaling_2to1: bool,             // 2:1 scaling (0xE7 on / 0xE6 off)
    buttons: u8,                    // current button bitmask (bit0 left, bit1 right, bit2 middle)
    expecting_data: Option<u8>,     // a mouse command awaiting its parameter byte
    device_id: u8,                  // 0x00 standard, 0x03 IntelliMouse (set by the knock)
    intellimouse: bool,             // four-byte wheel mode enabled
    rate_history: [u8; 3],          // last three 0xF3 sample rates (for the magic knock)
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
            device_id: 0x00,
            intellimouse: false,
            rate_history: [0; 3],
        }
    }
}

impl Ps2Mouse {
    /// Put the device into IntelliMouse (4-byte / wheel) mode. The platform enables
    /// this at mouse-enable; the magic knock also reaches it via write_byte.
    pub(crate) fn enable_wheel(&mut self) {
        self.device_id = 0x03;
        self.intellimouse = true;
    }

    pub(crate) fn set_sample_rate_code(&mut self, code: u8) -> bool {
        let rate = match code {
            0 => 10,
            1 => 20,
            2 => 40,
            3 => 60,
            4 => 80,
            5 => 100,
            6 => 200,
            _ => return false,
        };
        self.set_sample_rate(rate);
        true
    }

    fn set_sample_rate(&mut self, rate: u8) {
        self.sample_rate = rate;
        self.rate_history = [self.rate_history[1], self.rate_history[2], rate];
        if self.rate_history == [200, 100, 80] {
            self.enable_wheel();
        }
    }

    pub(crate) fn sample_rate(&self) -> u8 {
        self.sample_rate
    }

    /// Test seam: whether the device is in IntelliMouse 4-byte (wheel) mode.
    #[cfg(test)]
    pub(crate) fn is_intellimouse(&self) -> bool {
        self.intellimouse
    }

    /// Handle a byte the guest wrote to the mouse (via the controller's 0xD4
    /// path). Most commands queue an ACK (0xFA); a parameter-taking command
    /// (set sample rate / resolution) records the next byte as its parameter.
    pub(crate) fn write_byte(&mut self, value: u8) {
        if let Some(cmd) = self.expecting_data.take() {
            match cmd {
                // 0xF3 set sample rate, 0xE8 set resolution: record the parameter.
                0xF3 => {
                    self.set_sample_rate(value);
                }
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
                self.device_id = 0x00;
                self.intellimouse = false;
                self.rate_history = [0; 3];
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
                self.device_id = 0x00;
                self.intellimouse = false;
                self.rate_history = [0; 3];
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
                // Get device id: ACK then 0x00 (standard) or 0x03 (IntelliMouse).
                self.queue.push_back(0xFA);
                self.queue.push_back(self.device_id);
            }
            _ => self.queue.push_back(0xFA), // ack anything else
        }
    }

    /// The current button bitmask (bit0 left, bit1 right, bit2 middle), so a
    /// wheel-only injection can reuse it instead of clearing the buttons.
    pub(crate) fn current_buttons(&self) -> u8 {
        self.buttons
    }

    /// Queue a movement packet for `dx`/`dy` (host pixels, y down positive), the
    /// button mask, and `dz` (wheel detents). The packet is three bytes for a
    /// standard mouse, or four (with a signed Z byte) in IntelliMouse mode.
    /// Returns true if reporting is enabled so the controller can raise IRQ12.
    /// Movement while reporting is off is dropped, matching a real mouse that
    /// holds its line idle until enabled.
    pub(crate) fn queue_movement(&mut self, dx: i32, dy: i32, buttons: u8, dz: i32) -> bool {
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
        if self.intellimouse {
            let cz = dz.clamp(-8, 7) as i8; // signed wheel detent
            self.queue.push_back(cz as u8);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_knock_switches_to_intellimouse() {
        let mut m = Ps2Mouse::default();
        for rate in [200u8, 100, 80] {
            m.write_byte(0xF3); // set sample rate
            m.queue.clear(); // drop the ACK
            m.write_byte(rate); // the parameter
            m.queue.clear();
        }
        m.write_byte(0xF2); // get device id
        assert_eq!(m.queue.pop_front(), Some(0xFA)); // ACK
        assert_eq!(m.queue.pop_front(), Some(0x03)); // IntelliMouse id
    }

    #[test]
    fn intellimouse_packet_is_four_bytes_with_z() {
        let mut m = Ps2Mouse::default();
        for rate in [200u8, 100, 80] {
            m.write_byte(0xF3);
            m.write_byte(rate);
        }
        m.queue.clear();
        m.reporting = true;
        assert!(m.queue_movement(0, 0, 0, -1)); // dz = -1
        assert_eq!(m.queue.len(), 4);
        let _b0 = m.queue.pop_front().unwrap();
        let _x = m.queue.pop_front().unwrap();
        let _y = m.queue.pop_front().unwrap();
        assert_eq!(m.queue.pop_front().unwrap() as i8, -1); // Z byte
    }

    #[test]
    fn wrong_sample_rate_sequence_does_not_knock() {
        let mut m = Ps2Mouse::default();
        for rate in [200u8, 100, 81] {
            // 81, not 80 -> not the magic knock
            m.write_byte(0xF3);
            m.write_byte(rate);
        }
        m.queue.clear();
        m.write_byte(0xF2);
        assert_eq!(m.queue.pop_front(), Some(0xFA));
        assert_eq!(m.queue.pop_front(), Some(0x00)); // still standard PS/2 id
        m.reporting = true;
        assert!(m.queue_movement(1, 1, 0, 0));
        assert_eq!(m.queue.len(), 3); // still a 3-byte packet (no wheel)
    }

    #[test]
    fn reset_drops_back_to_three_byte() {
        let mut m = Ps2Mouse::default();
        for rate in [200u8, 100, 80] {
            m.write_byte(0xF3);
            m.write_byte(rate);
        }
        m.write_byte(0xFF); // reset
        m.queue.clear();
        m.write_byte(0xF2);
        assert_eq!(m.queue.pop_front(), Some(0xFA));
        assert_eq!(m.queue.pop_front(), Some(0x00)); // back to standard id
        m.reporting = true;
        assert!(m.queue_movement(1, 1, 0, 0));
        assert_eq!(m.queue.len(), 3); // 3-byte packet again
    }
}
