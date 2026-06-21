//! Intel 8259A programmable interrupt controller, a master/slave cascade pair.
//!
//! Built clean-room from the Intel 8259A datasheet cached at
//! dev_docs/reference/8259a/. Edge latched, 8086 vector mode. Priority order is
//! rotatable through OCW2 (a per-controller lowest-priority pointer), and ICW4
//! special fully nested mode is decoded and honored in the cascade decision.

/// One 8259A. The pair owns two of these plus the cascade routing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Pic {
    irr: u8,               // interrupt request register (latched requests)
    isr: u8,               // in-service register
    imr: u8,               // interrupt mask register (1 = masked)
    icw2: u8,              // vector base; vector(irq) = (icw2 & 0xF8) | irq
    icw3: u8,              // cascade wiring: master IR pin bitmask, or slave id
    init: InitStage,       // odd-port initialization sequence position
    expect_icw4: bool,     // ICW1 bit0 (IC4)
    single: bool,          // ICW1 bit1 (SNGL): skip ICW3
    level_triggered: bool, // ICW1 bit3 (LTIM): level mode, stored only
    auto_eoi: bool,        // ICW4 bit1 (AEOI)
    buffered: bool,        // ICW4 bit3 (BUF), stored only
    is_master: bool,       // ICW4 bit2 (M/S) when buffered, stored only
    read_isr: bool,        // OCW3 read select: false = IRR, true = ISR
    poll_pending: bool,    // OCW3 P=1: the next data read is a poll command
    special_mask: bool,    // OCW3 SMM: special mask mode active
    sfnm: bool,            // ICW4 bit4 (SFNM): special fully nested mode
    lowest: u8,            // OCW2 rotation: the level holding lowest priority
    auto_rotate: bool,     // OCW2 R=1 with EOI bit 0: rotate in automatic EOI mode
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
                // Reset priority: IR0 highest, IR7 lowest. The pointer names the
                // level that currently holds lowest priority.
                self.lowest = 7;
                self.auto_rotate = false;
                self.expect_icw4 = value & 0x01 != 0;
                self.single = value & 0x02 != 0;
                // ponytail: LTIM is decoded and stored, but the request path stays
                // edge-pulsed; level-triggered re-assertion is not modeled.
                self.level_triggered = value & 0x08 != 0;
                self.init = InitStage::ExpectIcw2;
            } else if value & 0x08 != 0 {
                // OCW3: read-register select, poll command, and special mask mode.
                if value & 0x02 != 0 {
                    self.read_isr = value & 0x01 != 0;
                }
                if value & 0x04 != 0 {
                    // P=1: the next data-port read is serviced as a poll.
                    self.poll_pending = true;
                }
                if value & 0x40 != 0 {
                    // ESMM=1 (D6): the SMM bit (D5) then sets or resets special mask mode.
                    // ESMM=1/SMM=0 reverts to normal mask mode; ESMM=0 leaves it unchanged.
                    self.special_mask = value & 0x20 != 0;
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
                    // Cascade wiring. On the master this is a bitmask of IR pins
                    // that carry a slave; on a slave it is the slave id in bits 2-0.
                    self.icw3 = value;
                    self.init = if self.expect_icw4 {
                        InitStage::ExpectIcw4
                    } else {
                        InitStage::Ready
                    };
                }
                InitStage::ExpectIcw4 => {
                    self.auto_eoi = value & 0x02 != 0;
                    self.buffered = value & 0x08 != 0;
                    self.is_master = value & 0x04 != 0;
                    self.sfnm = value & 0x10 != 0;
                    self.init = InitStage::Ready;
                }
                InitStage::Ready => {
                    // OCW1: interrupt mask register.
                    self.imr = value;
                }
            }
        }
    }

    /// `cascade_exempt` carries the same special-fully-nested-mode exemption as
    /// `highest_pending`: the pair passes the master's exempt cascade pin so a
    /// poll resolves the same level acknowledge() would, and `None` for the slave.
    fn read_port(&mut self, port: u16, cascade_exempt: Option<u8>) -> u8 {
        if self.poll_pending {
            // A poll command armed by OCW3 P=1 overrides the register read on the
            // next access to either port and behaves like an INTA pulse.
            self.poll_pending = false;
            return self.poll(cascade_exempt);
        }
        if port & 1 == 0 {
            if self.read_isr { self.isr } else { self.irr }
        } else {
            self.imr
        }
    }

    /// Poll command: acknowledge the highest-priority deliverable request in
    /// software. Sets its IS bit and returns `I 0 0 0 0 W2 W1 W0` where bit 7 is
    /// interrupt-present and bits 2-0 are the level; 0x00 when nothing is pending.
    /// A software poll is an INTA in software, so it resolves through the same
    /// special-fully-nested-mode rule as acknowledge(): `cascade_exempt` relaxes
    /// the master's busy cascade pin so a higher slave line can win the poll.
    fn poll(&mut self, cascade_exempt: Option<u8>) -> u8 {
        match self.highest_pending(cascade_exempt) {
            Some(level) => {
                self.set_in_service(level);
                0x80 | level
            }
            None => 0x00,
        }
    }

    /// The eight levels in priority order, highest first. The level just below
    /// `lowest` is highest, so `(lowest + 1) % 8` leads and `lowest` trails. With
    /// the reset pointer of 7 this is the fixed 0..7 order.
    fn priority_order(&self) -> [u8; 8] {
        let mut order = [0u8; 8];
        for (slot, item) in order.iter_mut().enumerate() {
            *item = (self.lowest + 1 + slot as u8) % 8;
        }
        order
    }

    /// OCW2: end of interrupt and priority rotation. Bits 7-5 select the command,
    /// bits 2-0 name a level for the specific variants.
    fn end_of_interrupt(&mut self, ocw2: u8) {
        let level = ocw2 & 0x07;
        match ocw2 >> 5 {
            // 000 / 100: clear or set rotate-in-automatic-EOI mode, no EOI.
            0b000 | 0b100 => self.auto_rotate = ocw2 & 0x80 != 0,
            // 001: non-specific EOI, clear the highest-priority in-service level.
            0b001 => {
                if let Some(level) = self.highest_in_service() {
                    self.isr &= !(1 << level);
                }
            }
            // 011: specific EOI, clear the named level.
            0b011 => self.isr &= !(1 << level),
            // 101: rotate on non-specific EOI. Clear the highest in-service level
            // and move it to lowest priority.
            0b101 => {
                if let Some(level) = self.highest_in_service() {
                    self.isr &= !(1 << level);
                    self.lowest = level;
                }
            }
            // 110: set priority, no EOI. The named level becomes lowest priority.
            0b110 => self.lowest = level,
            // 111: rotate on specific EOI. Clear the named level and move it to
            // lowest priority.
            0b111 => {
                self.isr &= !(1 << level);
                self.lowest = level;
            }
            // 010: no-op.
            _ => {}
        }
    }

    fn highest_in_service(&self) -> Option<u8> {
        self.priority_order()
            .into_iter()
            .find(|&irq| self.isr & (1 << irq) != 0)
    }

    /// Highest-priority deliverable request, or None. In fully nested mode a
    /// request outranks the in-service set only if no equal-or-higher ISR bit is
    /// set. In special mask mode a level is skipped only when its own ISR bit is
    /// set, so a lower unmasked request can still be delivered. Levels are walked
    /// in the current rotated priority order, not a fixed 0..7.
    ///
    /// `cascade_exempt` names a master cascade pin running special fully nested
    /// mode: that pin's in-service bit does not inhibit a fresh request on the
    /// same pin, so a higher-priority slave line can preempt one already being
    /// serviced. Every other level keeps the fully nested rule, and the slave's
    /// own internal priority orders the two slave requests. Pass `None` for the
    /// plain fully nested resolution used by a slave or a non-SFNM master.
    fn highest_pending(&self, cascade_exempt: Option<u8>) -> Option<u8> {
        let requests = self.irr & !self.imr;
        for irq in self.priority_order() {
            let bit = 1 << irq;
            if self.isr & bit != 0 && cascade_exempt != Some(irq) {
                if self.special_mask {
                    continue;
                }
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
            if self.auto_rotate {
                // Rotate-in-automatic-EOI: the acknowledged level drops to lowest.
                self.lowest = irq;
            }
        }
    }
}

/// The master/slave 8259A pair. The slave's INT output drives one master IR pin,
/// the one selected by the slave's ICW3 id, modeled by mirroring any slave request
/// onto that master pin so the single-chip resolver handles both levels. The mirror
/// is edge latched: a second slave request latched while another slave level is in
/// service must be re-raised through `request`, because the master cascade bit is
/// not held across its EOI. The PIT (IRQ0, master) does not exercise this; add a
/// held slave INT line if a cascaded device needs it.
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

    pub(crate) fn read_port(&mut self, port: u16) -> Option<u8> {
        match port {
            0x20 | 0x21 => {
                // A master poll is an INTA in software, so it must resolve under
                // the same SFNM exemption acknowledge() uses, or poll and ack
                // would disagree on which level wins the master cascade pin.
                let cascade_exempt = self.master_cascade_exempt();
                Some(self.master.read_port(port, cascade_exempt))
            }
            // The slave never owns a cascade pin of its own, so the plain fully
            // nested poll resolution applies.
            0xa0 | 0xa1 => Some(self.slave.read_port(port, None)),
            _ => None,
        }
    }

    /// True when IRQ `irq` (0..15) is not masked. IRQ 0..7 are on the master,
    /// IRQ 8..15 on the slave. IRQ10 (slave IR2) is gated by the slave IMR: the
    /// master IR2 cascade line is normally unmasked, so the slave mask is the
    /// meaningful gate.
    pub(crate) fn irq_unmasked(&self, irq: u8) -> bool {
        if irq < 8 {
            self.master.imr & (1 << irq) == 0
        } else {
            self.slave.imr & (1 << (irq - 8)) == 0
        }
    }

    /// True when IRQ0 (master IR0) is not masked in the master IMR.
    pub(crate) fn irq0_unmasked(&self) -> bool {
        self.irq_unmasked(0)
    }

    /// True when IRQ `irq` (0..15) has a latched request in the IRR. IRQ 0..7 are
    /// on the master, IRQ 8..15 on the slave. Test-only inspector.
    #[cfg(test)]
    pub(crate) fn irr_bit(&self, irq: u8) -> bool {
        if irq < 8 {
            self.master.irr & (1 << irq) != 0
        } else {
            self.slave.irr & (1 << (irq - 8)) != 0
        }
    }

    pub(crate) fn request(&mut self, irq: u8) {
        debug_assert!(irq < 16, "the PIC pair has 16 IRQ lines, got {irq}");
        if irq < 8 {
            self.master.irr |= 1 << irq;
        } else if irq < 16 {
            self.slave.irr |= 1 << (irq - 8);
            // The slave INT line is wired to the master IR pin named by the
            // slave's ICW3 id (bits 2-0); the AT default is master IR2.
            let cascade_pin = self.slave.icw3 & 0x07;
            self.master.irr |= 1 << cascade_pin;
        }
        // irq >= 16 is not a PC interrupt line; ignore it in release builds.
    }

    /// The master cascade pin exempt from the fully nested block, or `None`. When
    /// the master runs special fully nested mode and its wired cascade pin carries
    /// the slave, an in-service bit on that pin no longer blocks a fresh request on
    /// it, so a higher slave line can preempt the one being serviced. Both the
    /// interrupt resolution (acknowledge) and the software poll consult this so the
    /// two paths agree.
    fn master_cascade_exempt(&self) -> Option<u8> {
        let cascade_pin = self.slave.icw3 & 0x07;
        let pin_has_slave = self.master.icw3 & (1 << cascade_pin) != 0;
        (self.master.sfnm && pin_has_slave).then_some(cascade_pin)
    }

    /// The master's highest-priority deliverable level, resolved under the same
    /// special-fully-nested-mode rule the poll path uses.
    //
    // ponytail: SFNM here is just the master-side block relaxation. The datasheet
    // also asks software to poll the slave's ISR after a slave EOI and skip the
    // master EOI while the slave still has work in service. That slave-EOI dance
    // is left to the guest; this models the request-resolution half only.
    fn master_pending(&self) -> Option<u8> {
        self.master.highest_pending(self.master_cascade_exempt())
    }

    pub(crate) fn interrupt_pending(&self) -> bool {
        self.master_pending().is_some()
    }

    pub(crate) fn acknowledge(&mut self) -> Option<u8> {
        let master_irq = self.master_pending()?;
        self.master.set_in_service(master_irq);
        // A pin is a cascade only if the master ICW3 bitmask flags it and the
        // slave's ICW3 id names the same pin (AT default: master IR2, slave id 2).
        let pin_has_slave = self.master.icw3 & (1 << master_irq) != 0;
        let cascade_pin = self.slave.icw3 & 0x07;
        if !pin_has_slave || master_irq != cascade_pin {
            return Some(self.master.vector(master_irq));
        }
        // Cascade: the master selected the slave. A non-AEOI EOI is later owed to
        // both chips (the slave then the master); under AEOI each ISR self-clears.
        // The slave resolves under the plain fully nested rule (no exempt pin).
        match self.slave.highest_pending(None) {
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
        assert_eq!(pic.master.isr, 0x04); // master IR2 is in service, owes a master EOI
        assert_eq!(pic.slave.isr, 0x00); // no slave ISR set on a spurious IR7
    }

    #[test]
    fn cascade_routing_follows_stored_icw3_id() {
        // Wire the slave onto master IR5 instead of the AT default IR2. Both chips
        // must agree: master ICW3 flags pin 5, slave ICW3 id is 5.
        let mut pic = Pic8259Pair::default();
        pic.write_port(0x20, 0x11); // master ICW1
        pic.write_port(0x21, 0x08); // master ICW2 base 0x08
        pic.write_port(0x21, 0x20); // master ICW3 slave on IR5
        pic.write_port(0x21, 0x01); // master ICW4 8086
        pic.write_port(0xa0, 0x11); // slave ICW1
        pic.write_port(0xa1, 0x70); // slave ICW2 base 0x70
        pic.write_port(0xa1, 0x05); // slave ICW3 id 5
        pic.write_port(0xa1, 0x01); // slave ICW4 8086
        pic.request(9); // slave line 1
        assert_eq!(pic.master.irr, 0x20); // mirrored onto master IR5, not IR2
        assert_eq!(pic.acknowledge(), Some(0x71)); // slave base 0x70 | 1
        assert_eq!(pic.master.isr, 0x20); // master IR5 in service
        assert_eq!(pic.slave.isr, 0x02);
    }

    #[test]
    fn poll_command_returns_level_and_sets_isr() {
        let mut pic = master_initialized();
        pic.request(3);
        pic.write_port(0x20, 0x0c); // OCW3 with P=1 (D3=1, P=1)
        assert_eq!(pic.read_port(0x20), Some(0x83)); // present, level 3
        assert_eq!(pic.master.isr, 0x08); // poll set IR3 in service
        // The poll is consumed: a following read returns the selected register (IRR).
        let irr = pic.master.irr;
        assert_eq!(pic.read_port(0x20), Some(irr));
    }

    #[test]
    fn poll_command_with_no_request_returns_zero() {
        let mut pic = master_initialized();
        pic.write_port(0x20, 0x0c); // OCW3 with P=1
        assert_eq!(pic.read_port(0x20), Some(0x00));
        assert_eq!(pic.master.isr, 0x00);
    }

    #[test]
    fn special_mask_mode_delivers_lower_unmasked_request() {
        let mut pic = master_initialized();
        pic.request(2);
        pic.acknowledge(); // IR2 in service
        assert_eq!(pic.master.isr, 0x04);
        pic.request(4);
        // Fully nested: IR4 stays blocked behind the in-service IR2.
        assert!(!pic.interrupt_pending());
        pic.write_port(0x20, 0x68); // OCW3 ESMM=1, SMM=1
        pic.write_port(0x21, 0x04); // OCW1 mask IR2
        // Special mask mode now lets the lower unmasked IR4 through.
        assert!(pic.interrupt_pending());
        assert_eq!(pic.acknowledge(), Some(0x0c)); // IR4 vector
    }

    #[test]
    fn special_mask_mode_reverts_to_normal_on_esmm_clear() {
        let mut pic = master_initialized();
        pic.request(2);
        pic.acknowledge(); // IR2 in service
        pic.request(4);
        pic.write_port(0x20, 0x68); // ESMM=1, SMM=1: enable special mask mode
        pic.write_port(0x21, 0x04); // mask IR2 so SMM lets the lower IR4 through
        assert!(pic.interrupt_pending());
        // ESMM=1, SMM=0 reverts to normal mask mode: the in-service IR2 blocks IR4 again.
        pic.write_port(0x20, 0x48);
        assert!(!pic.interrupt_pending());
    }

    #[test]
    fn without_special_mask_lower_request_stays_blocked() {
        let mut pic = master_initialized();
        pic.request(2);
        pic.acknowledge(); // IR2 in service
        pic.request(4);
        pic.write_port(0x21, 0x04); // OCW1 mask IR2, but no special mask mode
        assert!(!pic.interrupt_pending()); // IR4 still blocked by IR2 in service
    }

    #[test]
    fn icw1_ltim_and_icw4_buffered_bits_are_stored() {
        let mut pic = Pic8259Pair::default();
        pic.write_port(0x20, 0x19); // ICW1 with LTIM (bit3) and IC4
        pic.write_port(0x21, 0x08); // ICW2
        pic.write_port(0x21, 0x04); // ICW3
        pic.write_port(0x21, 0x0d); // ICW4 8086, buffered master (BUF + M/S)
        assert!(pic.master.level_triggered);
        assert!(pic.master.buffered);
        assert!(pic.master.is_master);
    }

    #[test]
    fn icw1_resets_priority_pointer_to_seven() {
        let pic = master_initialized();
        // Reset order is IR0 highest, IR7 lowest.
        assert_eq!(pic.master.lowest, 7);
        assert_eq!(pic.master.priority_order(), [0, 1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn icw4_sfnm_bit_is_decoded() {
        let mut pic = Pic8259Pair::default();
        pic.write_port(0x20, 0x11); // ICW1 cascade, ICW4 follows
        pic.write_port(0x21, 0x08); // ICW2 base 0x08
        pic.write_port(0x21, 0x04); // ICW3 slave on IR2
        pic.write_port(0x21, 0x11); // ICW4 8086 + SFNM (bit4)
        assert!(pic.master.sfnm);

        // The same sequence without bit4 leaves SFNM clear.
        let plain = master_initialized();
        assert!(!plain.master.sfnm);
    }

    #[test]
    fn set_priority_command_moves_rotation_pointer() {
        let mut pic = master_initialized();
        // OCW2 110 (set priority), level 4: IR4 becomes lowest, so IR5 leads.
        pic.write_port(0x20, 0xc4);
        assert_eq!(pic.master.lowest, 4);
        assert_eq!(pic.master.priority_order(), [5, 6, 7, 0, 1, 2, 3, 4]);
        // No EOI bit, so a clear ISR stays clear.
        assert_eq!(pic.master.isr, 0x00);
    }

    #[test]
    fn rotate_on_non_specific_eoi_demotes_serviced_level() {
        let mut pic = master_initialized();
        pic.request(2);
        pic.acknowledge(); // IR2 in service, highest priority by reset order
        assert_eq!(pic.master.isr, 0x04);
        // OCW2 101 (rotate on non-specific EOI): clear IR2 and make it lowest.
        pic.write_port(0x20, 0xa0);
        assert_eq!(pic.master.isr, 0x00);
        assert_eq!(pic.master.lowest, 2);
        // IR3 now leads the order: an equal-priority contest with IR4 favors IR3,
        // and the just-serviced IR2 trails everything.
        assert_eq!(pic.master.priority_order(), [3, 4, 5, 6, 7, 0, 1, 2]);
        pic.request(2);
        pic.request(4);
        // After rotation IR4 outranks the demoted IR2.
        assert_eq!(pic.acknowledge(), Some(0x0c)); // IR4 vector
    }

    #[test]
    fn rotate_on_specific_eoi_clears_and_demotes_named_level() {
        let mut pic = master_initialized();
        pic.request(1);
        pic.request(5);
        pic.acknowledge(); // IR1 in service (higher than IR5)
        pic.master.set_in_service(5); // force IR5 in service too for the test
        assert_eq!(pic.master.isr, 0x22);
        // OCW2 111 (rotate on specific EOI), level 1: clear IR1, make it lowest.
        pic.write_port(0x20, 0xe1);
        assert_eq!(pic.master.isr & 0x02, 0x00); // IR1 cleared
        assert_eq!(pic.master.lowest, 1);
        assert_eq!(pic.master.priority_order(), [2, 3, 4, 5, 6, 7, 0, 1]);
    }

    #[test]
    fn non_specific_eoi_clears_highest_in_rotated_order() {
        let mut pic = master_initialized();
        // Rotate so IR4 is lowest priority and IR5 leads.
        pic.write_port(0x20, 0xc4); // set priority, level 4 lowest
        pic.master.set_in_service(0);
        pic.master.set_in_service(5);
        assert_eq!(pic.master.isr, 0x21);
        // Non-specific EOI clears the highest by the rotated order. IR5 leads IR0,
        // so IR5 is cleared first, not IR0.
        pic.write_port(0x20, 0x20);
        assert_eq!(pic.master.isr, 0x01); // IR0 still in service, IR5 cleared
    }

    #[test]
    fn rotate_in_auto_eoi_demotes_acknowledged_level() {
        let mut pic = master_initialized();
        // Re-init with AEOI set (ICW4 bit1) on top of the 8086 mode.
        pic.write_port(0x20, 0x11);
        pic.write_port(0x21, 0x08);
        pic.write_port(0x21, 0x04);
        pic.write_port(0x21, 0x03); // ICW4 8086 + AEOI
        // OCW2 100: set rotate-in-automatic-EOI mode.
        pic.write_port(0x20, 0x80);
        assert!(pic.master.auto_rotate);
        pic.request(3);
        pic.acknowledge(); // AEOI self-clears IR3 and rotation demotes it
        assert_eq!(pic.master.isr, 0x00);
        assert_eq!(pic.master.lowest, 3);
        // OCW2 000 clears rotate-in-automatic-EOI mode again.
        pic.write_port(0x20, 0x00);
        assert!(!pic.master.auto_rotate);
    }

    #[test]
    fn sfnm_master_lets_higher_slave_line_preempt() {
        let mut pic = master_initialized_sfnm();
        slave_initialized(&mut pic);
        pic.request(9); // slave line 1
        assert_eq!(pic.acknowledge(), Some(0x71)); // slave base 0x70 | 1
        assert_eq!(pic.master.isr, 0x04); // master IR2 cascade in service
        assert_eq!(pic.slave.isr, 0x02);
        // A higher-priority slave line (IR8 = slave line 0) requests while the
        // master cascade pin is still in service. SFNM does not block it.
        pic.request(8);
        assert!(pic.interrupt_pending());
        assert_eq!(pic.acknowledge(), Some(0x70)); // slave base 0x70 | 0
    }

    #[test]
    fn without_sfnm_master_blocks_second_slave_line() {
        let mut pic = master_initialized();
        slave_initialized(&mut pic);
        pic.request(9); // slave line 1
        pic.acknowledge(); // master IR2 + slave line 1 in service
        assert!(!pic.master.sfnm);
        // The fully nested master treats its busy IR2 as a hard block, so a higher
        // slave line cannot get through until the master EOIs IR2.
        pic.request(8);
        assert!(!pic.interrupt_pending());
    }

    #[test]
    fn sfnm_master_poll_agrees_with_acknowledge() {
        // A software poll is an INTA in software, so it must apply the same SFNM
        // block relaxation acknowledge() does. With the busy cascade pin in
        // service for the slave, a master poll has to report the pin as present,
        // not blocked, exactly as an interrupt acknowledge would.
        let mut pic = master_initialized_sfnm();
        slave_initialized(&mut pic);
        pic.request(9); // slave line 1
        assert_eq!(pic.acknowledge(), Some(0x71)); // master IR2 + slave line 1
        assert_eq!(pic.master.isr, 0x04); // master IR2 cascade in service
        // A higher slave line requests, mirroring onto the in-service master IR2.
        pic.request(8);
        // Poll the master. Under the plain fully nested rule the in-service IR2
        // would block the poll and return 0x00; the SFNM-aware poll instead
        // reports IR2 present at level 2, agreeing with interrupt_pending().
        assert!(pic.interrupt_pending());
        pic.write_port(0x20, 0x0c); // OCW3 P=1 on the master
        assert_eq!(pic.read_port(0x20), Some(0x82)); // present, level 2 (cascade pin)
        assert_eq!(pic.master.isr, 0x04); // poll set (kept) IR2 in service
    }

    #[test]
    fn sfnm_slave_eoi_protocol_defers_master_eoi() {
        // The full special-fully-nested-mode slave-EOI dance, the guest software
        // sequence the datasheet prescribes: after EOIing the slave, software
        // reads the slave ISR and only EOIs the master once the slave ISR clears.
        let mut pic = master_initialized_sfnm();
        slave_initialized(&mut pic);

        // A lower slave line goes into service through the cascade.
        pic.request(9); // slave line 1
        assert_eq!(pic.acknowledge(), Some(0x71));
        assert_eq!(pic.master.isr, 0x04); // master IR2 cascade
        assert_eq!(pic.slave.isr, 0x02); // slave line 1

        // A higher slave line preempts it. SFNM relaxes the master cascade pin,
        // and the slave's own nesting lets line 0 outrank the in-service line 1.
        pic.request(8); // slave line 0, higher priority
        assert!(pic.interrupt_pending());
        assert_eq!(pic.acknowledge(), Some(0x70));
        assert_eq!(pic.slave.isr, 0x03); // both slave lines now in service

        // The higher handler finishes. Non-specific EOI to the slave clears its
        // top in-service line (line 0), leaving line 1 still in service.
        pic.write_port(0xa0, 0x20);
        assert_eq!(pic.slave.isr, 0x02);

        // Software reads the slave ISR via OCW3 (read-ISR select) to decide
        // whether to EOI the master. The remaining in-service bit is visible.
        pic.write_port(0xa0, 0x0b); // OCW3: read ISR (D3=1, RR=1, RIS=1)
        assert_eq!(pic.read_port(0xa0), Some(0x02));
        // The slave ISR is non-zero, so the guest correctly skips the master EOI:
        // the master cascade pin must stay in service while the slave is busy.
        assert_eq!(pic.master.isr, 0x04);

        // The lower handler finishes. Non-specific EOI to the slave clears the
        // last in-service line, so the slave ISR read now shows it empty.
        pic.write_port(0xa0, 0x20);
        assert_eq!(pic.read_port(0xa0), Some(0x00)); // OCW3 read-ISR still latched
        // Now, and only now, the guest issues the deferred master EOI.
        pic.write_port(0x20, 0x20);
        assert_eq!(pic.master.isr, 0x00);
    }

    fn master_initialized_sfnm() -> Pic8259Pair {
        let mut pic = Pic8259Pair::default();
        // Same as master_initialized but ICW4 sets SFNM (bit4) alongside 8086 mode.
        pic.write_port(0x20, 0x11);
        pic.write_port(0x21, 0x08);
        pic.write_port(0x21, 0x04);
        pic.write_port(0x21, 0x11);
        pic
    }
}
