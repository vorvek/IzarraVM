//! AT keyboard controller (8042-class) as software sees it: status/data ports,
//! a host-fed scancode FIFO, and the command subset the BIOS uses at boot.
//! Command set is boot-minimal; extend when the BIOS needs more.

use std::collections::VecDeque;

const STATUS_OBF: u8 = 0x01; // output buffer full (data waiting on 0x60)
const STATUS_IBF: u8 = 0x02; // input buffer full (unread command/data)
const STATUS_SYS: u8 = 0x04; // system flag, set after a passed self-test

#[derive(Debug, Clone, PartialEq)]
pub struct Keyboard8042 {
    queue: VecDeque<u8>, // host-injected scancodes waiting to be latched
    output: Option<u8>,  // the byte currently readable on 0x60
    status: u8,
    command_byte: u8,                   // 8042 command byte (bit 0 = IRQ1 enable)
    expecting_command_data: Option<u8>, // a 0x64 command awaiting its 0x60 data
    irq_armed: bool,                    // a freshly latched byte that should pulse IRQ1
}

impl Default for Keyboard8042 {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            output: None,
            status: STATUS_SYS,
            command_byte: 0x01, // IRQ1 enabled, translation on (as a PC BIOS leaves it)
            expecting_command_data: None,
            irq_armed: false,
        }
    }
}

impl Keyboard8042 {
    /// Queue host scancodes (Set 1, make on press / break = 0x80|make on release).
    pub fn push_scancodes(&mut self, codes: &[u8]) {
        self.queue.extend(codes.iter().copied());
        self.latch_next();
    }

    /// Move the next queued scancode into the output buffer if it is free.
    fn latch_next(&mut self) {
        if self.output.is_none() {
            if let Some(code) = self.queue.pop_front() {
                self.output = Some(code);
                self.status |= STATUS_OBF;
                if self.command_byte & 0x01 != 0 {
                    self.irq_armed = true;
                }
            }
        }
    }

    /// Take the pending "announce a key" edge; the caller pulses IRQ1.
    pub fn take_irq(&mut self) -> bool {
        let armed = self.irq_armed;
        self.irq_armed = false;
        armed
    }

    pub fn read_port(&mut self, port: u16) -> Option<u8> {
        match port {
            0x60 => {
                let value = self.output.take().unwrap_or(0x00);
                self.status &= !STATUS_OBF;
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
                    if cmd == 0x60 {
                        self.command_byte = value;
                    }
                    // Other command-data writes ignored until needed.
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
                    0xAA => self.push_scancodes(&[0x55]), // controller self-test OK
                    0xAB => self.push_scancodes(&[0x00]), // interface test OK
                    0x20 => {
                        // read command byte -> output buffer
                        self.queue.push_front(self.command_byte);
                        self.latch_next();
                    }
                    0x60 => self.expecting_command_data = Some(0x60), // write command byte
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
}
