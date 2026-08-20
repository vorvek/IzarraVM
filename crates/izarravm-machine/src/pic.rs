// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Intel 8259A programmable interrupt controller, a master/slave cascade pair.
//!
//! Built clean-room from the Intel 8259A datasheet. Edge and level triggering
//! are supported in 8086 vector mode. Priority order is rotatable through OCW2 (a per-controller
//! lowest-priority pointer), and ICW4 special fully nested mode is decoded and
//! honored in the cascade decision.

use izarravm_core::{CanonicalFieldWriter, CanonicalStateError};

/// One 8259A. The pair owns two of these plus the cascade routing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Pic {
    irr: u8,               // interrupt request register (latched requests)
    asserted: u8,          // current electrical level on each IR input
    isr: u8,               // in-service register
    imr: u8,               // interrupt mask register (1 = masked)
    icw2: u8,              // vector base; vector(irq) = (icw2 & 0xF8) | irq
    icw3: u8,              // cascade wiring: master IR pin bitmask, or slave id
    init: InitStage,       // odd-port initialization sequence position
    expect_icw4: bool,     // ICW1 bit0 (IC4)
    single: bool,          // ICW1 bit1 (SNGL): skip ICW3
    level_triggered: bool, // ICW1 bit3 (LTIM)
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
                self.isr = 0;
                self.imr = 0;
                self.read_isr = false;
                // Reset priority: IR0 highest, IR7 lowest. The pointer names the
                // level that currently holds lowest priority.
                self.lowest = 7;
                self.auto_rotate = false;
                self.expect_icw4 = value & 0x01 != 0;
                self.single = value & 0x02 != 0;
                self.level_triggered = value & 0x08 != 0;
                self.irr = if self.level_triggered {
                    self.asserted
                } else {
                    0
                };
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

    fn request(&mut self, irq: u8) {
        self.irr |= 1 << irq;
    }

    fn set_input_level(&mut self, irq: u8, asserted: bool) {
        let bit = 1 << irq;
        let was_asserted = self.asserted & bit != 0;
        if asserted {
            self.asserted |= bit;
            if self.level_triggered || !was_asserted {
                self.irr |= bit;
            }
        } else {
            self.asserted &= !bit;
            if self.level_triggered {
                self.irr &= !bit;
            }
        }
    }

    fn clear_in_service(&mut self, irq: u8) {
        let bit = 1 << irq;
        self.isr &= !bit;
        if self.level_triggered && self.asserted & bit != 0 {
            self.irr |= bit;
        }
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
                    self.clear_in_service(level);
                }
            }
            // 011: specific EOI, clear the named level.
            0b011 => self.clear_in_service(level),
            // 101: rotate on non-specific EOI. Clear the highest in-service level
            // and move it to lowest priority.
            0b101 => {
                if let Some(level) = self.highest_in_service() {
                    self.clear_in_service(level);
                    self.lowest = level;
                }
            }
            // 110: set priority, no EOI. The named level becomes lowest priority.
            0b110 => self.lowest = level,
            // 111: rotate on specific EOI. Clear the named level and move it to
            // lowest priority.
            0b111 => {
                self.clear_in_service(level);
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
            self.clear_in_service(irq);
            if self.auto_rotate {
                // Rotate-in-automatic-EOI: the acknowledged level drops to lowest.
                self.lowest = irq;
            }
        }
    }
}

/// The master/slave 8259A pair. The slave's INT output drives one master IR pin,
/// the one selected by the slave's ICW3 id, modeled by mirroring any slave request
/// onto that master pin so the single-chip resolver handles both levels. Pulsed
/// requests retain the edge-latched path. Held inputs also drive the slave INT
/// level through the cascade pin.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Pic8259Pair {
    master: Pic,
    slave: Pic,
}

/// Borrowed, behaviorally effective PIC state for canonical comparison.
///
/// The buffered-controller role bits are stored by ICW4 but never affect this
/// model. The ignored ICW2 and slave ICW3 bits are projected out for the same
/// reason. Everything retained here can affect a later port read, interrupt
/// decision, acknowledge, EOI, or edge transition.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CanonicalPic8259Pair<'a> {
    pair: &'a Pic8259Pair,
}

const fn init_stage_tag(stage: InitStage) -> u8 {
    match stage {
        InitStage::Ready => 0,
        InitStage::ExpectIcw2 => 1,
        InitStage::ExpectIcw3 => 2,
        InitStage::ExpectIcw4 => 3,
    }
}

fn write_canonical_pic(
    out: &mut CanonicalFieldWriter<'_>,
    pic: &Pic,
    cascade: u8,
    master: bool,
) -> Result<(), CanonicalStateError> {
    let expect_icw4 =
        matches!(pic.init, InitStage::ExpectIcw2 | InitStage::ExpectIcw3) && pic.expect_icw4;
    let single = pic.init == InitStage::ExpectIcw2 && pic.single;
    out.write_u8(pic.irr)?;
    out.write_u8(pic.asserted)?;
    out.write_u8(pic.isr)?;
    out.write_u8(pic.imr)?;
    out.write_u8(pic.icw2 & 0xf8)?;
    out.write_u8(cascade)?;
    out.write_u8(init_stage_tag(pic.init))?;
    out.write_bool(expect_icw4)?;
    out.write_bool(single)?;
    out.write_bool(pic.level_triggered)?;
    out.write_bool(pic.auto_eoi)?;
    out.write_bool(pic.read_isr)?;
    out.write_bool(pic.poll_pending)?;
    out.write_bool(pic.special_mask)?;
    out.write_bool(master && pic.sfnm)?;
    out.write_u8(pic.lowest)?;
    out.write_bool(pic.auto_eoi && pic.auto_rotate)
}

impl CanonicalPic8259Pair<'_> {
    /// Writes version 1 of the fixed 34-byte PIC-pair payload.
    pub(crate) fn write_payload(
        &self,
        out: &mut CanonicalFieldWriter<'_>,
    ) -> Result<(), CanonicalStateError> {
        write_canonical_pic(out, &self.pair.master, self.pair.master.icw3, true)?;
        write_canonical_pic(out, &self.pair.slave, self.pair.slave.icw3 & 0x07, false)
    }
}

impl Pic8259Pair {
    pub(crate) fn canonical_projection(&self) -> CanonicalPic8259Pair<'_> {
        CanonicalPic8259Pair { pair: self }
    }

    pub(crate) fn write_port(&mut self, port: u16, value: u8) -> bool {
        match port {
            0x20 | 0x21 => self.master.write_port(port, value),
            0xa0 | 0xa1 => {
                let old_pin = self.slave.icw3 & 0x07;
                self.slave.write_port(port, value);
                let new_pin = self.slave.icw3 & 0x07;
                if old_pin != new_pin {
                    self.master.set_input_level(old_pin, false);
                }
                self.sync_cascade();
            }
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
            0xa0 | 0xa1 => {
                let value = self.slave.read_port(port, None);
                self.sync_cascade();
                Some(value)
            }
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

    /// True when an edge on IRQ `irq` (0..15) can actually reach the CPU, i.e. the
    /// line is unmasked *and*, for a slave line (8..15), the master cascade pin the
    /// slave INT is wired to (slave ICW3 id, AT default IR2) is also unmasked.
    /// `irq_unmasked` alone only checks the slave IMR bit; if a guest masks the
    /// cascade line but leaves the slave bit open, the slave latches the IRR but no
    /// edge forwards to the master, so the CPU never wakes. Wake estimators must use
    /// this -- not `irq_unmasked` -- so a halted CPU is not fast-forwarded to a wake
    /// that delivery can never produce.
    pub(crate) fn deliverable(&self, irq: u8) -> bool {
        if irq < 8 {
            self.irq_unmasked(irq)
        } else {
            let cascade_pin = self.slave.icw3 & 0x07;
            self.irq_unmasked(irq) && self.master.imr & (1 << cascade_pin) == 0
        }
    }

    /// True when the input pin for IRQ `irq` (0..15) is currently being driven
    /// by a device. Distinct from [`Pic8259Pair::irr_bit`]: on the AT's
    /// edge-triggered 8259 the IRR latches a rising edge and stays set after the
    /// line falls, so only this tells a test whether a device is still holding
    /// the line. Test-only inspector.
    #[cfg(test)]
    pub(crate) fn input_asserted(&self, irq: u8) -> bool {
        if irq < 8 {
            self.master.asserted & (1 << irq) != 0
        } else {
            self.slave.asserted & (1 << (irq - 8)) != 0
        }
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
            self.master.request(irq);
        } else if irq < 16 {
            self.slave.request(irq - 8);
            // The slave INT line is wired to the master IR pin named by the
            // slave's ICW3 id (bits 2-0); the AT default is master IR2.
            let cascade_pin = self.slave.icw3 & 0x07;
            self.master.request(cascade_pin);
        }
        // irq >= 16 is not a PC interrupt line; ignore it in release builds.
    }

    pub(crate) fn set_irq_level(&mut self, irq: u8, asserted: bool) {
        debug_assert!(irq < 16, "the PIC pair has 16 IRQ lines, got {irq}");
        if irq < 8 {
            self.master.set_input_level(irq, asserted);
        } else if irq < 16 {
            self.slave.set_input_level(irq - 8, asserted);
            self.sync_cascade();
        }
    }

    fn sync_cascade(&mut self) {
        let cascade_pin = self.slave.icw3 & 0x07;
        let asserted = self.slave.highest_pending(None).is_some();
        self.master.set_input_level(cascade_pin, asserted);
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
    // Limit: SFNM here is just the master-side block relaxation. The datasheet
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
                self.sync_cascade();
                Some(self.slave.vector(slave_irq))
            }
            // The slave line dropped before INTA: spurious IR7, no slave ISR set.
            None => {
                self.sync_cascade();
                Some(self.slave.vector(7))
            }
        }
    }
}

#[cfg(test)]
#[path = "pic_test.rs"]
mod tests;
