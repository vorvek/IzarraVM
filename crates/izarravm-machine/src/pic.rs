//! Intel 8259A programmable interrupt controller, a master/slave cascade pair.
//!
//! Built clean-room from the Intel 8259A datasheet cached at
//! dev_docs/reference/8259a/. Fixed priority, edge latched, 8086 vector mode.
//! Rotating priority, special mask mode, the poll command, special fully nested
//! mode, and buffered mode are not modeled; the PC BIOS and DOS do not use them.

/// One 8259A. The pair owns two of these plus the cascade routing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Pic {
    irr: u8,           // interrupt request register (latched requests)
    isr: u8,           // in-service register
    imr: u8,           // interrupt mask register (1 = masked)
    icw2: u8,          // vector base; vector(irq) = (icw2 & 0xF8) | irq
    init: InitStage,   // odd-port initialization sequence position
    expect_icw4: bool, // ICW1 bit0 (IC4)
    single: bool,      // ICW1 bit1 (SNGL): skip ICW3
    auto_eoi: bool,    // ICW4 bit1 (AEOI)
    read_isr: bool,    // OCW3 read select: false = IRR, true = ISR
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum InitStage {
    #[default]
    Ready,
    ExpectIcw2,
    ExpectIcw3,
    ExpectIcw4,
}

impl Pic {
    fn write_port(&mut self, port: u16, value: u8) {
        if port & 1 == 0 {
            // Command port (A0=0).
            if value & 0x10 != 0 {
                // ICW1: master clear, then start the init sequence.
                self.irr = 0;
                self.isr = 0;
                self.imr = 0;
                self.read_isr = false;
                self.expect_icw4 = value & 0x01 != 0;
                self.single = value & 0x02 != 0;
                self.init = InitStage::ExpectIcw2;
            } else if value & 0x08 != 0 {
                // OCW3: only the read-register select is modeled. Poll and the
                // special mask mode are not used by the PC BIOS or DOS.
                if value & 0x02 != 0 {
                    self.read_isr = value & 0x01 != 0;
                }
            } else {
                // OCW2: end of interrupt.
                self.end_of_interrupt(value);
            }
        } else {
            // Data port (A0=1).
            match self.init {
                InitStage::ExpectIcw2 => {
                    self.icw2 = value;
                    self.init = if !self.single {
                        InitStage::ExpectIcw3
                    } else if self.expect_icw4 {
                        InitStage::ExpectIcw4
                    } else {
                        InitStage::Ready
                    };
                }
                InitStage::ExpectIcw3 => {
                    // Cascade wiring is fixed in the pair; the ICW3 value is consumed.
                    self.init = if self.expect_icw4 {
                        InitStage::ExpectIcw4
                    } else {
                        InitStage::Ready
                    };
                }
                InitStage::ExpectIcw4 => {
                    self.auto_eoi = value & 0x02 != 0;
                    self.init = InitStage::Ready;
                }
                InitStage::Ready => {
                    // OCW1: interrupt mask register.
                    self.imr = value;
                }
            }
        }
    }

    fn read_port(&self, port: u16) -> u8 {
        if port & 1 == 0 {
            if self.read_isr { self.isr } else { self.irr }
        } else {
            self.imr
        }
    }

    fn end_of_interrupt(&mut self, ocw2: u8) {
        if ocw2 & 0x20 == 0 {
            // No EOI bit: a rotate-only or set-priority command. Priority rotation
            // is not modeled, so there is nothing to do.
            return;
        }
        if ocw2 & 0x40 != 0 {
            // Specific EOI: clear the named level.
            self.isr &= !(1 << (ocw2 & 0x07));
        } else if let Some(level) = self.highest_in_service() {
            // Non-specific EOI: clear the highest-priority in-service level.
            self.isr &= !(1 << level);
        }
    }

    fn highest_in_service(&self) -> Option<u8> {
        (0..8u8).find(|&irq| self.isr & (1 << irq) != 0)
    }

    /// Highest-priority deliverable request, or None. A request outranks the
    /// in-service set only if no equal-or-higher ISR bit is set (fully nested).
    fn highest_pending(&self) -> Option<u8> {
        let requests = self.irr & !self.imr;
        for irq in 0..8u8 {
            let bit = 1 << irq;
            if self.isr & bit != 0 {
                return None;
            }
            if requests & bit != 0 {
                return Some(irq);
            }
        }
        None
    }

    fn vector(&self, irq: u8) -> u8 {
        (self.icw2 & 0xf8) | irq
    }

    fn set_in_service(&mut self, irq: u8) {
        let bit = 1 << irq;
        self.isr |= bit;
        self.irr &= !bit;
        if self.auto_eoi {
            self.isr &= !bit;
        }
    }
}

/// The master/slave 8259A pair. The slave's INT output drives master IR2, modeled
/// by mirroring any slave request onto master IR2 so the single-chip resolver
/// handles both levels.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Pic8259Pair {
    master: Pic,
    slave: Pic,
}

impl Pic8259Pair {
    pub(crate) fn write_port(&mut self, port: u16, value: u8) -> bool {
        match port {
            0x20 | 0x21 => self.master.write_port(port, value),
            0xa0 | 0xa1 => self.slave.write_port(port, value),
            _ => return false,
        }
        true
    }

    pub(crate) fn read_port(&self, port: u16) -> Option<u8> {
        match port {
            0x20 | 0x21 => Some(self.master.read_port(port)),
            0xa0 | 0xa1 => Some(self.slave.read_port(port)),
            _ => None,
        }
    }

    pub(crate) fn request(&mut self, irq: u8) {
        if irq < 8 {
            self.master.irr |= 1 << irq;
        } else {
            self.slave.irr |= 1 << (irq - 8);
            self.master.irr |= 1 << 2; // the slave INT line is wired to master IR2
        }
    }

    pub(crate) fn interrupt_pending(&self) -> bool {
        self.master.highest_pending().is_some()
    }

    pub(crate) fn acknowledge(&mut self) -> Option<u8> {
        let master_irq = self.master.highest_pending()?;
        self.master.set_in_service(master_irq);
        if master_irq != 2 {
            return Some(self.master.vector(master_irq));
        }
        // Cascade: the master selected the slave. EOI is later owed to both chips.
        match self.slave.highest_pending() {
            Some(slave_irq) => {
                self.slave.set_in_service(slave_irq);
                Some(self.slave.vector(slave_irq))
            }
            // The slave line dropped before INTA: spurious IR7, no slave ISR set.
            None => Some(self.slave.vector(7)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn master_initialized() -> Pic8259Pair {
        let mut pic = Pic8259Pair::default();
        // ICW1 (edge, cascade, ICW4 follows), ICW2 base 0x08, ICW3 slave on IR2, ICW4 8086.
        pic.write_port(0x20, 0x11);
        pic.write_port(0x21, 0x08);
        pic.write_port(0x21, 0x04);
        pic.write_port(0x21, 0x01);
        pic
    }

    fn slave_initialized(pic: &mut Pic8259Pair) {
        // ICW1, ICW2 base 0x70, ICW3 slave id 2, ICW4 8086.
        pic.write_port(0xa0, 0x11);
        pic.write_port(0xa1, 0x70);
        pic.write_port(0xa1, 0x02);
        pic.write_port(0xa1, 0x01);
    }

    #[test]
    fn icw1_clears_mask_and_sets_ready() {
        let pic = master_initialized();
        assert_eq!(pic.master.init, InitStage::Ready);
        assert_eq!(pic.master.imr, 0);
        assert_eq!(pic.master.icw2, 0x08);
    }

    #[test]
    fn vector_uses_icw2_offset() {
        let mut pic = master_initialized();
        pic.request(0);
        assert_eq!(pic.acknowledge(), Some(0x08));

        let mut pic = master_initialized();
        pic.request(1);
        assert_eq!(pic.acknowledge(), Some(0x09));
    }

    #[test]
    fn imr_masks_request() {
        let mut pic = master_initialized();
        pic.write_port(0x21, 0x01); // OCW1: mask IR0
        pic.request(0);
        assert!(!pic.interrupt_pending());
    }

    #[test]
    fn request_sets_irr_acknowledge_sets_isr() {
        let mut pic = master_initialized();
        pic.request(0);
        assert_eq!(pic.master.irr, 0x01);
        assert_eq!(pic.acknowledge(), Some(0x08));
        assert_eq!(pic.master.irr, 0x00);
        assert_eq!(pic.master.isr, 0x01);
        pic.write_port(0x20, 0x0b); // OCW3: read ISR (D3=1, RR=1, RIS=1)
        assert_eq!(pic.read_port(0x20), Some(0x01));
    }

    #[test]
    fn fixed_priority_blocks_lower_until_eoi() {
        let mut pic = master_initialized();
        pic.request(1);
        pic.request(3);
        assert_eq!(pic.acknowledge(), Some(0x09)); // IR1 outranks IR3
        assert!(!pic.interrupt_pending()); // IR3 blocked while IR1 is in service
        pic.write_port(0x20, 0x20); // non-specific EOI clears IR1
        assert!(pic.interrupt_pending());
        assert_eq!(pic.acknowledge(), Some(0x0b)); // now IR3
    }

    #[test]
    fn specific_eoi_clears_named_level() {
        let mut pic = master_initialized();
        pic.request(4);
        pic.acknowledge();
        assert_eq!(pic.master.isr, 0x10);
        pic.write_port(0x20, 0x64); // specific EOI, level 4
        assert_eq!(pic.master.isr, 0x00);
    }

    #[test]
    fn cascade_delivers_slave_vector() {
        let mut pic = master_initialized();
        slave_initialized(&mut pic);
        pic.request(9); // slave line 1
        assert_eq!(pic.master.irr, 0x04); // master IR2 mirrors the slave INT
        assert!(pic.interrupt_pending());
        assert_eq!(pic.acknowledge(), Some(0x71)); // slave base 0x70 | 1
        assert_eq!(pic.master.isr, 0x04);
        assert_eq!(pic.slave.isr, 0x02);
        pic.write_port(0xa0, 0x20); // EOI slave
        pic.write_port(0x20, 0x20); // EOI master
        assert_eq!(pic.slave.isr, 0x00);
        assert_eq!(pic.master.isr, 0x00);
    }

    #[test]
    fn slave_line_dropped_before_ack_is_spurious_ir7() {
        let mut pic = master_initialized();
        slave_initialized(&mut pic);
        pic.request(9); // master IR2 + slave line 1
        pic.write_port(0xa1, 0x02); // mask slave line 1 after raising it
        assert_eq!(pic.acknowledge(), Some(0x77)); // slave base 0x70 | 7, spurious
        assert_eq!(pic.slave.isr, 0x00); // no slave ISR set on a spurious IR7
    }
}
