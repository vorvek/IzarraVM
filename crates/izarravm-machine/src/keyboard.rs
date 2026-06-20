//! AT keyboard controller (8042-class) as software sees it: status/data ports,
//! a host-fed scancode FIFO, and the command subset the BIOS uses at boot. The
//! controller also multiplexes a PS/2 auxiliary (mouse) device, reachable
//! through the 0xD4 "write to aux" command and reporting on IRQ12.
//! Command set is boot-minimal; extend when the BIOS needs more.

use std::collections::VecDeque;

const STATUS_OBF: u8 = 0x01; // output buffer full (data waiting on 0x60)
const STATUS_SYS: u8 = 0x04; // system flag, set after a passed self-test
const STATUS_AUX: u8 = 0x20; // the byte in the output buffer came from the mouse

/// A standard PS/2 (three-byte) mouse. Tracks the reporting enable, the queued
/// data bytes, and the sample-rate/resolution/scaling state the driver sets up
/// during detection. Movement and button changes queue a three-byte packet and
/// (when reporting is on) raise IRQ12 through the controller.
#[derive(Debug, Clone, PartialEq)]
struct Ps2Mouse {
    queue: VecDeque<u8>, // bytes waiting to be moved into the aux output buffer
    reporting: bool,     // data-reporting enabled (command 0xF4 on / 0xF5 off)
    sample_rate: u8,     // last value set by 0xF3 (sample rate, Hz)
    resolution: u8,      // last value set by 0xE8 (counts per mm code 0..3)
    scaling_2to1: bool,  // 2:1 scaling (0xE7 on / 0xE6 off)
    buttons: u8,         // current button bitmask (bit0 left, bit1 right, bit2 middle)
    expecting_data: Option<u8>, // a mouse command awaiting its parameter byte
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
    fn write_byte(&mut self, value: u8) {
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
    fn queue_movement(&mut self, dx: i32, dy: i32, buttons: u8) -> bool {
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

#[derive(Debug, Clone, PartialEq)]
pub struct Keyboard8042 {
    queue: VecDeque<u8>, // host-injected scancodes waiting to be latched
    output: Option<u8>,  // the byte currently readable on 0x60
    output_is_aux: bool, // the latched byte came from the mouse (status bit 5)
    status: u8,
    command_byte: u8,                   // 8042 command byte (bit 0 = IRQ1 enable)
    expecting_command_data: Option<u8>, // a 0x64 command awaiting its 0x60 data
    irq_armed: bool,                    // a freshly latched keyboard byte to pulse IRQ1
    irq12_armed: bool,                  // a freshly latched mouse byte to pulse IRQ12
    mouse: Ps2Mouse,
}

impl Default for Keyboard8042 {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            output: None,
            output_is_aux: false,
            status: STATUS_SYS,
            command_byte: 0x01, // IRQ1 enabled, translation on (as a PC BIOS leaves it)
            expecting_command_data: None,
            irq_armed: false,
            irq12_armed: false,
            mouse: Ps2Mouse::default(),
        }
    }
}

impl Keyboard8042 {
    /// Queue host scancodes (Set 1, make on press / break = 0x80|make on release).
    pub fn push_scancodes(&mut self, codes: &[u8]) {
        self.queue.extend(codes.iter().copied());
        self.latch_next();
    }

    /// Feed host mouse movement to the aux device: a relative delta plus the
    /// button mask. Queues a PS/2 packet and latches the first byte when data
    /// reporting is enabled. Returns true if that should pulse IRQ12.
    pub fn inject_mouse(&mut self, dx: i32, dy: i32, buttons: u8) -> bool {
        let reporting = self.mouse.queue_movement(dx, dy, buttons);
        self.latch_next();
        reporting && self.irq12_armed
    }

    /// Put a controller command response (self-test 0x55, interface test 0x00)
    /// into the output buffer ahead of keyboard scancodes. A real 8042 holds the
    /// keyboard while it processes a command and returns the answer immediately, so
    /// any scancode already latched is pushed back to the front of the queue rather
    /// than dropped. This keeps a self-test from eating host keystrokes.
    fn respond_immediately(&mut self, response: u8) {
        if let Some(latched) = self.output.take() {
            if self.output_is_aux {
                self.mouse.queue.push_front(latched);
            } else {
                self.queue.push_front(latched);
            }
        }
        self.output = Some(response);
        self.output_is_aux = false;
        self.status |= STATUS_OBF;
        self.status &= !STATUS_AUX;
        if self.command_byte & 0x01 != 0 {
            self.irq_armed = true;
        }
    }

    /// Move the next queued byte into the output buffer if it is free. A waiting
    /// keyboard byte is preferred; a mouse byte is drained only when no scancode
    /// is pending. A mouse byte sets the AUX status bit and arms IRQ12 instead of
    /// IRQ1.
    fn latch_next(&mut self) {
        if self.output.is_some() {
            return;
        }
        if let Some(code) = self.queue.pop_front() {
            self.output = Some(code);
            self.output_is_aux = false;
            self.status |= STATUS_OBF;
            self.status &= !STATUS_AUX;
            if self.command_byte & 0x01 != 0 {
                self.irq_armed = true;
            }
        } else if let Some(code) = self.mouse.queue.pop_front() {
            self.output = Some(code);
            self.output_is_aux = true;
            self.status |= STATUS_OBF | STATUS_AUX;
            // Command byte bit 1 enables the mouse interrupt (IRQ12).
            if self.command_byte & 0x02 != 0 {
                self.irq12_armed = true;
            }
        }
    }

    /// Take the pending "announce a keyboard byte" edge; the caller pulses IRQ1.
    pub fn take_irq(&mut self) -> bool {
        let armed = self.irq_armed;
        self.irq_armed = false;
        armed
    }

    /// Take the pending "announce a mouse byte" edge; the caller pulses IRQ12.
    pub fn take_irq12(&mut self) -> bool {
        let armed = self.irq12_armed;
        self.irq12_armed = false;
        armed
    }

    pub fn read_port(&mut self, port: u16) -> Option<u8> {
        match port {
            0x60 => {
                let value = self.output.take().unwrap_or(0x00);
                self.output_is_aux = false;
                self.status &= !(STATUS_OBF | STATUS_AUX);
                self.latch_next(); // re-arms IRQ if more is queued
                Some(value)
            }
            0x64 => Some(self.status),
            _ => None,
        }
    }

    pub fn write_port(&mut self, port: u16, value: u8) -> bool {
        match port {
            0x60 => {
                if let Some(cmd) = self.expecting_command_data.take() {
                    match cmd {
                        0x60 => self.command_byte = value,
                        0xD4 => {
                            // Byte destined for the mouse: hand it to the aux
                            // device, then latch whatever it queued in reply.
                            self.mouse.write_byte(value);
                            self.latch_next();
                        }
                        _ => {} // other command-data writes ignored until needed
                    }
                } else {
                    // Keyboard device commands. Ack everything; reset/enable also
                    // queue 0xFA (ACK) and, for reset, 0xAA (self-test passed).
                    match value {
                        0xFF => self.push_scancodes(&[0xFA, 0xAA]),
                        _ => self.push_scancodes(&[0xFA]),
                    }
                }
                true
            }
            0x64 => {
                match value {
                    0xAA => self.respond_immediately(0x55), // controller self-test OK
                    0xAB => self.respond_immediately(0x00), // interface test OK
                    0x20 => {
                        // read command byte -> output buffer
                        self.queue.push_front(self.command_byte);
                        self.latch_next();
                    }
                    0x60 => self.expecting_command_data = Some(0x60), // write command byte
                    0xA8 | 0xA7 => {} // enable / disable aux (mouse): accepted
                    0xA9 => self.respond_immediately(0x00), // aux interface test OK
                    0xD4 => self.expecting_command_data = Some(0xD4), // write next byte to aux
                    0xAE | 0xAD => {} // enable / disable keyboard: accepted
                    0xD1 => self.expecting_command_data = Some(0xD1), // output port (A20)
                    _ => {}           // Rest accepted and ignored
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
    fn injected_scancode_reads_back_with_obf_and_irq() {
        let mut kbd = Keyboard8042::default();
        kbd.push_scancodes(&[0x1e]); // 'A' make
        assert_eq!(kbd.read_port(0x64).unwrap() & STATUS_OBF, STATUS_OBF);
        assert!(kbd.take_irq(), "a latched key arms IRQ1");
        assert_eq!(kbd.read_port(0x60), Some(0x1e));
        assert_eq!(
            kbd.read_port(0x64).unwrap() & STATUS_OBF,
            0,
            "OBF clears after read"
        );
    }

    #[test]
    fn second_scancode_re_arms_irq_after_read() {
        let mut kbd = Keyboard8042::default();
        kbd.push_scancodes(&[0x1e, 0x9e]); // make + break
        assert!(kbd.take_irq());
        assert_eq!(kbd.read_port(0x60), Some(0x1e));
        assert!(
            kbd.take_irq(),
            "reading latches the next byte and re-arms IRQ1"
        );
        assert_eq!(kbd.read_port(0x60), Some(0x9e));
    }

    #[test]
    fn controller_self_test_returns_0x55() {
        let mut kbd = Keyboard8042::default();
        kbd.write_port(0x64, 0xaa);
        assert_eq!(kbd.read_port(0x60), Some(0x55));
    }

    #[test]
    fn irq_disabled_in_command_byte_does_not_arm() {
        let mut kbd = Keyboard8042::default();
        kbd.write_port(0x64, 0x60); // write command byte
        kbd.write_port(0x60, 0x00); // IRQ1 disabled
        kbd.push_scancodes(&[0x1e]);
        assert!(!kbd.take_irq());
    }

    /// Enable the mouse the way a driver does: command byte bit 1 set so IRQ12
    /// fires, then a 0xD4-routed 0xF4 (enable data reporting) acked.
    fn enable_mouse(kbd: &mut Keyboard8042) {
        kbd.write_port(0x64, 0x60); // write command byte
        kbd.write_port(0x60, 0x03); // IRQ1 + IRQ12 (mouse) enabled
        kbd.write_port(0x64, 0xD4); // next 0x60 byte goes to the mouse
        kbd.write_port(0x60, 0xF4); // enable data reporting
        assert_eq!(kbd.read_port(0x60), Some(0xFA), "mouse acks enable");
    }

    #[test]
    fn aux_write_path_routes_to_mouse_and_acks() {
        let mut kbd = Keyboard8042::default();
        enable_mouse(&mut kbd);
        assert!(kbd.mouse.reporting, "0xF4 enabled data reporting");
    }

    #[test]
    fn movement_queues_three_byte_packet_and_arms_irq12() {
        let mut kbd = Keyboard8042::default();
        enable_mouse(&mut kbd);
        // Move right 5, up 3 (screen dy = -3), left button held.
        let irq = kbd.inject_mouse(5, -3, 0x01);
        assert!(irq, "movement with reporting on raises IRQ12");
        // First byte: AUX bit set in status.
        let status = kbd.read_port(0x64).unwrap();
        assert_eq!(status & STATUS_OBF, STATUS_OBF);
        assert_eq!(status & STATUS_AUX, STATUS_AUX, "byte is from the mouse");
        // Flags byte: bit3 set, left button (bit0); +x and (screen up -> +y)
        // both positive, so neither sign bit is set.
        let b0 = kbd.read_port(0x60).unwrap();
        assert_eq!(b0 & 0x08, 0x08, "always-one bit");
        assert_eq!(b0 & 0x01, 0x01, "left button");
        assert_eq!(b0 & 0x10, 0x00, "X positive, no sign");
        assert_eq!(b0 & 0x20, 0x00, "Y positive (up), no sign");
        let bx = kbd.read_port(0x60).unwrap();
        assert_eq!(bx, 5, "dx byte");
        let by = kbd.read_port(0x60).unwrap();
        assert_eq!(by, 3, "dy byte (negated screen delta)");
    }

    #[test]
    fn movement_without_reporting_is_dropped() {
        let mut kbd = Keyboard8042::default();
        // No enable: reporting is off by default.
        let irq = kbd.inject_mouse(10, 10, 0);
        assert!(!irq, "no IRQ12 while reporting is disabled");
        assert_eq!(
            kbd.read_port(0x64).unwrap() & STATUS_OBF,
            0,
            "nothing latched"
        );
    }

    #[test]
    fn negative_delta_sets_sign_bits() {
        let mut kbd = Keyboard8042::default();
        enable_mouse(&mut kbd);
        // Move left 4 (dx -4), down 7 (screen dy +7 -> packet y -7).
        kbd.inject_mouse(-4, 7, 0);
        let b0 = kbd.read_port(0x60).unwrap();
        assert_eq!(b0 & 0x10, 0x10, "X sign set for leftward move");
        assert_eq!(b0 & 0x20, 0x20, "Y sign set for downward move");
        let bx = kbd.read_port(0x60).unwrap();
        assert_eq!(bx as i8 as i32, -4, "dx is -4 two's complement");
        let by = kbd.read_port(0x60).unwrap();
        assert_eq!(by as i8 as i32, -7, "dy is -7 (down)");
    }
}
