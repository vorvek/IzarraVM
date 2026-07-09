//! Parallel printer port as software sees it (LPT1 at base 0x378 on IRQ7, LPT2
//! at 0x278 on IRQ5): the data latch, a status register that reports a printer
//! that is always ready, and a control register with the strobe/init/IRQ-enable
//! bits. Printed bytes are captured into an output sink on the strobe pulse, so a
//! polled or interrupt INT 17h driver runs without ever blocking on a real
//! printer.

const LPT1_BASE: u16 = 0x0378;
const LPT2_BASE: u16 = 0x0278;

// Status register (0x379) bits. The data lines from the printer are active-low
// for Busy/Ack/Error, so a "good idle" state reads them as 1. PaperEnd is
// active-high and Select is active-high.
const STATUS_NOT_BUSY: u8 = 0x80; // bit7 -Busy: 1 = printer not busy
const STATUS_NOT_ACK: u8 = 0x40; // bit6 -ACK: 1 = no acknowledge pulse
// bit5 PaperEnd (0x20): 1 = out of paper; left clear in the idle state below.
const STATUS_SELECT: u8 = 0x10; // bit4 Select: 1 = printer online
const STATUS_NOT_ERROR: u8 = 0x08; // bit3 -Error: 1 = no error
const STATUS_RESERVED: u8 = 0x07; // bits0-2 read as 1

// A printer that is always idle and ready: not busy, no ack pulse, paper in,
// online, no error. Polled drivers spin on -Busy/-ACK, so this never hangs.
const STATUS_IDLE: u8 =
    STATUS_NOT_BUSY | STATUS_NOT_ACK | STATUS_SELECT | STATUS_NOT_ERROR | STATUS_RESERVED; // = 0xDF, PaperEnd clear

// Control register (0x37A) bits. Strobe/AutoLF/Init/SelectIn are active-low at
// the connector: software writes the latch and the hardware inverts. Bits 4-5
// (IRQ enable, direction) are not inverted. Bits 6-7 read back as 1.
const CONTROL_STROBE: u8 = 0x01; // bit0 -Strobe
const CONTROL_IRQ_ENABLE: u8 = 0x10; // bit4 ACK interrupt enable
const CONTROL_RESERVED: u8 = 0xC0; // bits6-7 read as 1

#[derive(Debug, Clone, PartialEq)]
pub struct Lpt {
    base: u16,             // first I/O port (0x378 for LPT1, 0x278 for LPT2)
    data: u8,              // data latch (base+0)
    control: u8,           // control latch (base+2), software view
    strobe_asserted: bool, // last seen strobe state, to capture once per pulse
    output: Vec<u8>,       // captured printed bytes
    irq_armed: bool,       // a strobed byte armed the -ACK IRQ edge
}

impl Default for Lpt {
    fn default() -> Self {
        Self {
            base: LPT1_BASE,
            data: 0,
            control: 0,
            strobe_asserted: false,
            output: Vec::new(),
            irq_armed: false,
        }
    }
}

impl Lpt {
    /// A second printer port decoded at the LPT2 base (0x278). Same model as
    /// LPT1; only the port window differs. The machine pulses IRQ5 for it.
    pub fn lpt2() -> Self {
        Self {
            base: LPT2_BASE,
            ..Self::default()
        }
    }

    /// The bytes captured from strobed prints, in order.
    pub fn output(&self) -> &[u8] {
        &self.output
    }

    /// Take the pending -ACK edge; the caller pulses the port's IRQ (IRQ7 for
    /// LPT1, IRQ5 for LPT2). Only armed when the control register had IRQ-enable
    /// (bit4) set at the strobe.
    pub fn take_irq(&mut self) -> bool {
        let armed = self.irq_armed;
        self.irq_armed = false;
        armed
    }

    pub fn read_port(&self, port: u16) -> Option<u8> {
        match port.checked_sub(self.base) {
            Some(0) => Some(self.data),
            Some(1) => Some(STATUS_IDLE),
            Some(2) => Some(self.control | CONTROL_RESERVED),
            _ => None,
        }
    }

    pub fn write_port(&mut self, port: u16, value: u8) -> bool {
        match port.checked_sub(self.base) {
            Some(0) => {
                self.data = value;
                true
            }
            Some(2) => {
                self.control = value;
                let strobe_now = value & CONTROL_STROBE != 0;
                // Capture once on the de-asserted -> asserted edge of -Strobe.
                if strobe_now && !self.strobe_asserted {
                    self.output.push(self.data);
                    // The -ACK pulse after the latched byte raises the port's IRQ
                    // when the control register has IRQ-enable set.
                    // Limit: instant printer, no real busy/ack timing window.
                    if value & CONTROL_IRQ_ENABLE != 0 {
                        self.irq_armed = true;
                    }
                }
                self.strobe_asserted = strobe_now;
                true
            }
            Some(1) => true, // status register is read-only; swallow writes
            _ => false,
        }
    }
}

#[cfg(test)]
#[path = "lpt_test.rs"]
mod tests;
