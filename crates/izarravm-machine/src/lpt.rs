// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Parallel printer port as software sees it (LPT1 at base 0x378 on IRQ7, LPT2
//! at 0x278 on IRQ5): the data latch, BUSY and -ACK phases, and a control
//! register with the strobe/init/IRQ-enable bits. A strobe starts a short timed
//! transfer. The byte and optional interrupt become visible on the -ACK edge.

use izarravm_core::MASTER_CLOCK_HZ;

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

const BUSY_TICKS: u64 = MASTER_CLOCK_HZ / 100_000; // 10 us
const ACK_TICKS: u64 = MASTER_CLOCK_HZ / 200_000; // 5 us

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrinterPhase {
    Idle,
    Busy { ticks: u64, byte: u8 },
    Ack { ticks: u64 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Lpt {
    base: u16,             // first I/O port (0x378 for LPT1, 0x278 for LPT2)
    data: u8,              // data latch (base+0)
    control: u8,           // control latch (base+2), software view
    strobe_asserted: bool, // last seen strobe state, to capture once per pulse
    output: Vec<u8>,       // captured printed bytes
    irq_armed: bool,       // a strobed byte armed the -ACK IRQ edge
    phase: PrinterPhase,
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
            phase: PrinterPhase::Idle,
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
            Some(1) => Some(self.status()),
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
                // Latch once on the de-asserted -> asserted edge of -Strobe.
                if strobe_now && !self.strobe_asserted && self.phase == PrinterPhase::Idle {
                    self.phase = PrinterPhase::Busy {
                        ticks: BUSY_TICKS,
                        byte: self.data,
                    };
                }
                self.strobe_asserted = strobe_now;
                true
            }
            Some(1) => true, // status register is read-only; swallow writes
            _ => false,
        }
    }

    fn status(&self) -> u8 {
        match self.phase {
            PrinterPhase::Idle => STATUS_IDLE,
            PrinterPhase::Busy { .. } => STATUS_IDLE & !STATUS_NOT_BUSY,
            PrinterPhase::Ack { .. } => STATUS_IDLE & !STATUS_NOT_ACK,
        }
    }

    pub fn advance_master_ticks(&mut self, mut ticks: u64) {
        while ticks > 0 {
            let Some(deadline) = self.ticks_until_event() else {
                break;
            };
            let step = ticks.min(deadline);
            ticks -= step;
            match &mut self.phase {
                PrinterPhase::Busy { ticks, byte } => {
                    *ticks -= step;
                    if *ticks == 0 {
                        self.output.push(*byte);
                        if self.control & CONTROL_IRQ_ENABLE != 0 {
                            self.irq_armed = true;
                        }
                        self.phase = PrinterPhase::Ack { ticks: ACK_TICKS };
                    }
                }
                PrinterPhase::Ack { ticks } => {
                    *ticks -= step;
                    if *ticks == 0 {
                        self.phase = PrinterPhase::Idle;
                    }
                }
                PrinterPhase::Idle => break,
            }
        }
    }

    pub fn ticks_until_event(&self) -> Option<u64> {
        match self.phase {
            PrinterPhase::Idle => None,
            PrinterPhase::Busy { ticks, .. } | PrinterPhase::Ack { ticks } => Some(ticks),
        }
    }

    pub fn ticks_until_idle(&self) -> u64 {
        match self.phase {
            PrinterPhase::Idle => 0,
            PrinterPhase::Busy { ticks, .. } => ticks.saturating_add(ACK_TICKS),
            PrinterPhase::Ack { ticks } => ticks,
        }
    }

    pub fn ticks_until_irq(&self) -> Option<u64> {
        match self.phase {
            PrinterPhase::Busy { ticks, .. } if self.control & CONTROL_IRQ_ENABLE != 0 => {
                Some(ticks)
            }
            PrinterPhase::Idle | PrinterPhase::Busy { .. } | PrinterPhase::Ack { .. } => None,
        }
    }
}

#[cfg(test)]
#[path = "lpt_test.rs"]
mod tests;
