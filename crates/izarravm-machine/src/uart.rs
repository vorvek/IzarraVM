// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! 16450/16550 UART as software sees it. Models a COM port (COM1 at base 0x3F8
//! on IRQ4, COM2 at 0x2F8 on IRQ3): the eight register offsets, the DLAB divisor
//! latches, the IIR priority encoder, loopback (MCR bit4), and the scratch
//! register a 16450-vs-16550 probe reads back. Transmit and loopback receive run
//! at the programmed baud against the machine master clock.

use std::collections::VecDeque;

use izarravm_core::MASTER_CLOCK_HZ;

const COM1_BASE: u16 = 0x03f8;
const COM2_BASE: u16 = 0x02f8;

// Line status register (offset 5) bits.
const LSR_DR: u8 = 0x01; // data ready in RBR
const LSR_OE: u8 = 0x02; // overrun error
const LSR_THRE: u8 = 0x20; // transmit holding register empty
const LSR_TEMT: u8 = 0x40; // transmitter empty

// Modem status register (offset 6) bits.
const MSR_DCTS: u8 = 0x01; // delta CTS
const MSR_DDSR: u8 = 0x02; // delta DSR
const MSR_TERI: u8 = 0x04; // trailing edge ring indicator
const MSR_DDCD: u8 = 0x08; // delta DCD
const MSR_CTS: u8 = 0x10;
const MSR_DSR: u8 = 0x20;
const MSR_RI: u8 = 0x40;
const MSR_DCD: u8 = 0x80;

// Modem control register (offset 4) bits.
const MCR_DTR: u8 = 0x01;
const MCR_RTS: u8 = 0x02;
const MCR_OUT1: u8 = 0x04;
const MCR_OUT2: u8 = 0x08; // conventional global interrupt enable gate
const MCR_LOOP: u8 = 0x10; // diagnostic loopback

// Interrupt enable register (offset 1) bits.
const IER_RDA: u8 = 0x01; // received data available
const IER_THRE: u8 = 0x02; // transmit holding register empty
const IER_RLS: u8 = 0x04; // receiver line status
const IER_MS: u8 = 0x08; // modem status

// IIR (offset 2 read) interrupt identification codes, low nibble.
const IIR_NONE: u8 = 0x01; // bit0 set means no interrupt pending
const IIR_RLS: u8 = 0x06; // receiver line status (highest priority)
const IIR_RDA: u8 = 0x04; // received data available
const IIR_TIMEOUT: u8 = 0x0c; // FIFO character timeout
const IIR_THRE: u8 = 0x02; // transmit holding register empty
const IIR_MS: u8 = 0x00; // modem status (lowest priority)

// FCR (offset 2 write) bits.
const FCR_FIFO_ENABLE: u8 = 0x01;
const FCR_CLEAR_RX: u8 = 0x02;
const FCR_CLEAR_TX: u8 = 0x04;
const FIFO_SIZE: usize = 16;

/// One COM port. Named registers, not a raw array, so each one carries its own
/// reset value and read/write side effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uart16450 {
    advance_credit_ticks: u64,
    base: u16,    // first I/O port (0x3F8 for COM1, 0x2F8 for COM2)
    ier: u8,      // interrupt enable
    fcr: u8,      // FIFO control (write side of offset 2)
    lcr: u8,      // line control; bit7 is DLAB
    mcr: u8,      // modem control
    lsr: u8,      // line status
    msr: u8,      // modem status
    scr: u8,      // scratch
    divisor: u16, // 16-bit baud divisor from DLL/DLM

    thre_pending: bool, // a THR-empty interrupt source is latched until IIR read
    irq_armed: bool,    // edge: the interrupt line just asserted
    irq_asserted: bool,

    rx_fifo: VecDeque<u8>,
    tx_fifo: VecDeque<u8>,
    tx_shift: Option<u8>,
    tx_ticks_remaining: u64,
    rx_timeout_remaining: Option<u64>,
    rx_timeout_pending: bool,

    output: Vec<u8>, // captured transmit bytes (the POST log sink)
}

impl Default for Uart16450 {
    fn default() -> Self {
        Self {
            advance_credit_ticks: 0,
            base: COM1_BASE,
            ier: 0,
            fcr: 0,
            lcr: 0,
            mcr: 0,
            lsr: LSR_THRE | LSR_TEMT, // reset 0x60: transmitter always empty
            msr: 0,
            scr: 0,
            divisor: 0,
            thre_pending: false,
            irq_armed: false,
            irq_asserted: false,
            rx_fifo: VecDeque::new(),
            tx_fifo: VecDeque::new(),
            tx_shift: None,
            tx_ticks_remaining: 0,
            rx_timeout_remaining: None,
            rx_timeout_pending: false,
            output: Vec::new(),
        }
    }
}

impl Uart16450 {
    /// A second UART decoded at the COM2 base (0x2F8). Same register model as
    /// COM1; only the port window differs. The machine pulses IRQ3 for it.
    pub fn com2() -> Self {
        Self {
            base: COM2_BASE,
            ..Self::default()
        }
    }

    /// Captured transmit bytes. serial_output()/serial_text() and the POST log
    /// boot-suite test read this.
    pub fn output(&self) -> &[u8] {
        &self.output
    }

    fn dlab(&self) -> bool {
        self.lcr & 0x80 != 0
    }

    fn map_offset(&self, port: u16) -> Option<u8> {
        if (self.base..=self.base + 7).contains(&port) {
            Some((port - self.base) as u8)
        } else {
            None
        }
    }

    /// Read a UART register, applying read side effects. None if not our port.
    pub(crate) fn advance_credit_ticks(&self) -> u64 {
        self.advance_credit_ticks
    }

    fn catch_up(&mut self, prefix_ticks: u64) {
        let elapsed = prefix_ticks
            .checked_sub(self.advance_credit_ticks)
            .expect("peripheral access cannot precede its last access");
        self.advance_elapsed_ticks(elapsed);
        self.advance_credit_ticks = prefix_ticks;
    }

    pub(crate) fn read_port_at(&mut self, port: u16, prefix_ticks: u64) -> Option<u8> {
        if !(self.map_offset(port).is_some()) {
            return None;
        }
        self.catch_up(prefix_ticks);
        self.read_port(port)
    }

    pub(crate) fn write_port_at(&mut self, port: u16, value: u8, prefix_ticks: u64) -> bool {
        if !(self.map_offset(port).is_some()) {
            return false;
        }
        self.catch_up(prefix_ticks);
        self.write_port(port, value)
    }

    pub fn read_port(&mut self, port: u16) -> Option<u8> {
        let offset = self.map_offset(port)?;
        let value = match offset {
            0 => {
                if self.dlab() {
                    (self.divisor & 0x00ff) as u8 // DLL
                } else {
                    self.read_receive_byte()
                }
            }
            1 => {
                if self.dlab() {
                    (self.divisor >> 8) as u8 // DLM
                } else {
                    self.ier
                }
            }
            2 => self.read_iir(),
            3 => self.lcr,
            4 => self.mcr, // reserved bits 5-7 already held 0 on write
            5 => {
                let value = self.lsr;
                self.lsr &= !LSR_OE;
                value
            }
            6 => {
                // Reading MSR clears the four delta bits.
                let value = self.msr;
                self.msr &= !(MSR_DCTS | MSR_DDSR | MSR_TERI | MSR_DDCD);
                value
            }
            7 => self.scr,
            _ => unreachable!(),
        };
        self.refresh_irq();
        Some(value)
    }

    /// Write a UART register, applying write side effects. false if not ours.
    pub fn write_port(&mut self, port: u16, value: u8) -> bool {
        let Some(offset) = self.map_offset(port) else {
            return false;
        };
        match offset {
            0 => {
                if self.dlab() {
                    self.divisor = (self.divisor & 0xff00) | u16::from(value); // DLL
                } else {
                    self.transmit(value); // THR
                }
            }
            1 => {
                if self.dlab() {
                    self.divisor = (self.divisor & 0x00ff) | (u16::from(value) << 8); // DLM
                } else {
                    let old = self.ier;
                    self.ier = value & 0x0f;
                    if old & IER_THRE == 0 && self.ier & IER_THRE != 0 && self.lsr & LSR_THRE != 0 {
                        self.thre_pending = true;
                    }
                }
            }
            2 => self.write_fcr(value),
            3 => self.lcr = value,
            4 => {
                let old = self.mcr;
                self.mcr = value & 0x1f; // reserved bits 5-7 read back 0
                if self.mcr & MCR_LOOP != 0 {
                    self.update_loopback_msr(old);
                } else if old & MCR_LOOP != 0 {
                    // Leaving loopback reconnects MSR to the real modem inputs,
                    // which read low with no device attached.
                    self.clear_loopback_msr();
                }
            }
            5 => {} // LSR is read-only in hardware; ignore writes
            6 => {} // MSR is read-only; the modem inputs drive it
            7 => self.scr = value,
            _ => unreachable!(),
        }
        self.refresh_irq();
        true
    }

    /// THR write. One byte may sit in the 16450 holding register, or sixteen in
    /// the enabled 16550 FIFO, while the shift register transmits independently.
    fn transmit(&mut self, value: u8) {
        let capacity = if self.fifo_enabled() { FIFO_SIZE } else { 1 };
        if self.tx_fifo.len() >= capacity {
            return;
        }
        self.thre_pending = false;
        self.lsr &= !(LSR_THRE | LSR_TEMT);
        self.refresh_irq();
        self.tx_fifo.push_back(value);
        self.start_next_transmit();
    }

    fn start_next_transmit(&mut self) {
        if self.tx_shift.is_none()
            && let Some(byte) = self.tx_fifo.pop_front()
        {
            self.tx_shift = Some(byte);
            self.tx_ticks_remaining = self.character_ticks();
        }
        if self.tx_fifo.is_empty() {
            if self.lsr & LSR_THRE == 0 {
                self.thre_pending = true;
            }
            self.lsr |= LSR_THRE;
        } else {
            self.lsr &= !LSR_THRE;
        }
        if self.tx_shift.is_none() {
            self.lsr |= LSR_TEMT;
        } else {
            self.lsr &= !LSR_TEMT;
        }
    }

    /// Advance baud and FIFO deadlines. Splitting a master-tick span does not
    /// change the bytes delivered or the interrupt state.
    pub fn advance_master_ticks(&mut self, ticks: u64) {
        let credit = ticks.min(self.advance_credit_ticks);
        self.advance_credit_ticks -= credit;
        self.advance_elapsed_ticks(ticks - credit);
    }

    fn advance_elapsed_ticks(&mut self, mut ticks: u64) {
        while ticks > 0 {
            let Some(next) = self.ticks_until_event() else {
                break;
            };
            let step = ticks.min(next);
            if self.tx_shift.is_some() {
                self.tx_ticks_remaining -= step;
            }
            if let Some(timeout) = self.rx_timeout_remaining.as_mut() {
                *timeout -= step;
            }
            ticks -= step;

            if self.tx_shift.is_some() && self.tx_ticks_remaining == 0 {
                let byte = self.tx_shift.take().unwrap();
                if self.mcr & MCR_LOOP != 0 {
                    self.receive(byte);
                } else {
                    self.output.push(byte);
                }
                self.start_next_transmit();
            }
            if self.rx_timeout_remaining == Some(0) {
                self.rx_timeout_remaining = None;
                self.rx_timeout_pending = !self.rx_fifo.is_empty();
            }
            self.refresh_irq();
        }
    }

    pub fn ticks_until_event(&self) -> Option<u64> {
        (self.tx_shift.is_some())
            .then_some(self.tx_ticks_remaining)
            .into_iter()
            .chain(self.rx_timeout_remaining)
            .min()
    }

    pub fn ticks_until_idle(&self) -> u64 {
        let queued = self.tx_fifo.len() as u128 * u128::from(self.character_ticks());
        (u128::from(self.tx_ticks_remaining) + queued).min(u128::from(u64::MAX)) as u64
    }

    pub fn ticks_until_irq(&self) -> Option<u64> {
        if self.irq_asserted || self.mcr & MCR_OUT2 == 0 {
            return None;
        }
        let receiver = self.receiver_irq_deadline();
        let transmitter = self.transmitter_irq_deadline();
        receiver.into_iter().chain(transmitter).min()
    }

    fn receiver_irq_deadline(&self) -> Option<u64> {
        if self.ier & (IER_RDA | IER_RLS) == 0 {
            return None;
        }
        let timeout = (self.ier & IER_RDA != 0)
            .then_some(self.rx_timeout_remaining)
            .flatten();
        if self.mcr & MCR_LOOP == 0 || self.tx_shift.is_none() {
            return timeout;
        }

        let first = self.tx_ticks_remaining;
        let character = self.character_ticks();
        let deliveries = self.tx_fifo.len().saturating_add(1);
        let at_delivery = |number: usize| {
            first.saturating_add((number.saturating_sub(1) as u64).saturating_mul(character))
        };
        let mut deadline = timeout.filter(|ticks| *ticks < first);

        if self.ier & IER_RDA != 0 {
            let needed = self.rx_trigger().saturating_sub(self.rx_fifo.len());
            let receive = if needed != 0 && needed <= deliveries {
                Some(at_delivery(needed))
            } else {
                Some(at_delivery(deliveries).saturating_add(4 * character))
            };
            deadline = deadline.into_iter().chain(receive).min();
        }
        if self.ier & IER_RLS != 0 {
            let capacity = if self.fifo_enabled() { FIFO_SIZE } else { 1 };
            let overrun = capacity
                .saturating_sub(self.rx_fifo.len())
                .saturating_add(1);
            if overrun <= deliveries {
                deadline = deadline.into_iter().chain(Some(at_delivery(overrun))).min();
            }
        }
        deadline
    }

    fn transmitter_irq_deadline(&self) -> Option<u64> {
        if self.ier & IER_THRE == 0 || self.tx_shift.is_none() || self.tx_fifo.is_empty() {
            return None;
        }
        Some(self.tx_ticks_remaining.saturating_add(
            (self.tx_fifo.len().saturating_sub(1) as u64).saturating_mul(self.character_ticks()),
        ))
    }

    fn character_ticks(&self) -> u64 {
        let data_bits = 5 + u64::from(self.lcr & 0x03);
        let parity_bits = u64::from(self.lcr & 0x08 != 0);
        let stop_half_bits = if self.lcr & 0x04 == 0 {
            2
        } else if data_bits == 5 {
            3
        } else {
            4
        };
        let half_bits = 2 + data_bits * 2 + parity_bits * 2 + stop_half_bits;
        let divisor = u128::from(self.divisor.max(1));
        (u128::from(MASTER_CLOCK_HZ) * divisor * u128::from(half_bits))
            .div_ceil(230_400)
            .min(u128::from(u64::MAX)) as u64
    }

    fn fifo_enabled(&self) -> bool {
        self.fcr & FCR_FIFO_ENABLE != 0
    }

    fn rx_trigger(&self) -> usize {
        if !self.fifo_enabled() {
            return 1;
        }
        match self.fcr >> 6 {
            0 => 1,
            1 => 4,
            2 => 8,
            _ => 14,
        }
    }

    fn receive(&mut self, byte: u8) {
        let capacity = if self.fifo_enabled() { FIFO_SIZE } else { 1 };
        if self.rx_fifo.len() >= capacity {
            self.lsr |= LSR_OE;
            return;
        }
        self.rx_fifo.push_back(byte);
        self.lsr |= LSR_DR;
        self.rx_timeout_pending = false;
        self.rx_timeout_remaining = (self.fifo_enabled() && self.rx_fifo.len() < self.rx_trigger())
            .then(|| 4 * self.character_ticks());
    }

    fn read_receive_byte(&mut self) -> u8 {
        let byte = self.rx_fifo.pop_front().unwrap_or(0);
        if self.rx_fifo.is_empty() {
            self.lsr &= !LSR_DR;
            self.rx_timeout_remaining = None;
        } else {
            self.rx_timeout_remaining = (self.fifo_enabled()
                && self.rx_fifo.len() < self.rx_trigger())
            .then(|| 4 * self.character_ticks());
        }
        self.rx_timeout_pending = false;
        byte
    }

    fn write_fcr(&mut self, value: u8) {
        let changed_mode = (self.fcr ^ value) & FCR_FIFO_ENABLE != 0;
        self.fcr = value & 0xc1;
        if changed_mode || value & FCR_CLEAR_RX != 0 {
            self.rx_fifo.clear();
            self.rx_timeout_remaining = None;
            self.rx_timeout_pending = false;
            self.lsr &= !(LSR_DR | LSR_OE);
        }
        if changed_mode || value & FCR_CLEAR_TX != 0 {
            self.tx_fifo.clear();
            self.start_next_transmit();
        }
    }

    /// Cross-wire MCR output bits into MSR input bits in loopback and flag the
    /// delta bits for any input that changed. This is the path the standard
    /// 8250/16450 detection routine and IBM POST use to probe a COM port.
    fn update_loopback_msr(&mut self, old_mcr: u8) {
        let mut msr = 0u8;
        if self.mcr & MCR_RTS != 0 {
            msr |= MSR_CTS; // RTS -> CTS
        }
        if self.mcr & MCR_DTR != 0 {
            msr |= MSR_DSR; // DTR -> DSR
        }
        if self.mcr & MCR_OUT1 != 0 {
            msr |= MSR_RI; // OUT1 -> RI
        }
        if self.mcr & MCR_OUT2 != 0 {
            msr |= MSR_DCD; // OUT2 -> DCD
        }
        // Keep any delta bits the guest has not yet read.
        msr |= self.msr & (MSR_DCTS | MSR_DDSR | MSR_TERI | MSR_DDCD);
        let changed = old_mcr ^ self.mcr;
        if changed & MCR_RTS != 0 {
            msr |= MSR_DCTS;
        }
        if changed & MCR_DTR != 0 {
            msr |= MSR_DDSR;
        }
        if changed & MCR_OUT1 != 0 {
            msr |= MSR_TERI;
        }
        if changed & MCR_OUT2 != 0 {
            msr |= MSR_DDCD;
        }
        self.msr = msr;
    }

    /// Leaving loopback: the four MSR input bits (CTS/DSR/RI/DCD) return to the
    /// no-modem low state. Flag a delta for each input that was high so a guest
    /// polling MSR sees the change, and keep any delta bits not yet read.
    fn clear_loopback_msr(&mut self) {
        let mut deltas = self.msr & (MSR_DCTS | MSR_DDSR | MSR_TERI | MSR_DDCD);
        if self.msr & MSR_CTS != 0 {
            deltas |= MSR_DCTS;
        }
        if self.msr & MSR_DSR != 0 {
            deltas |= MSR_DDSR;
        }
        if self.msr & MSR_RI != 0 {
            deltas |= MSR_TERI;
        }
        if self.msr & MSR_DCD != 0 {
            deltas |= MSR_DDCD;
        }
        self.msr = deltas;
    }

    /// Build the IIR byte and clear a serviced THR-empty source. Reading IIR
    /// acknowledges only the THRE source; the others clear on their own register
    /// read (RBR for RX data, LSR for line status, MSR for modem status).
    fn read_iir(&mut self) -> u8 {
        let code = self.pending_code();
        let fifo_bits = if self.fcr & FCR_FIFO_ENABLE != 0 {
            0xc0
        } else {
            0x00
        };
        if code == IIR_THRE {
            self.thre_pending = false;
        }
        code | fifo_bits
    }

    /// Highest-priority pending and enabled interrupt source as an IIR low
    /// nibble, or IIR_NONE when nothing is pending.
    fn pending_code(&self) -> u8 {
        if self.ier & IER_RLS != 0 && self.lsr & LSR_OE != 0 {
            IIR_RLS
        } else if self.ier & IER_RDA != 0 && self.rx_fifo.len() >= self.rx_trigger() {
            IIR_RDA
        } else if self.ier & IER_RDA != 0 && self.rx_timeout_pending {
            IIR_TIMEOUT
        } else if self.ier & IER_THRE != 0 && self.thre_pending {
            IIR_THRE
        } else if self.ier & IER_MS != 0
            && self.msr & (MSR_DCTS | MSR_DDSR | MSR_TERI | MSR_DDCD) != 0
        {
            IIR_MS
        } else {
            IIR_NONE
        }
    }

    /// Recompute whether the interrupt line is asserted and arm its rising edge.
    /// The line drives the port's IRQ (IRQ4 for COM1, IRQ3 for COM2) only when
    /// MCR OUT2 gates it.
    fn refresh_irq(&mut self) {
        let asserted = self.mcr & MCR_OUT2 != 0 && self.pending_code() != IIR_NONE;
        if asserted && !self.irq_asserted {
            self.irq_armed = true;
        }
        self.irq_asserted = asserted;
    }

    /// Take the pending interrupt edge; the caller pulses IRQ4.
    pub fn take_irq(&mut self) -> bool {
        let armed = self.irq_armed;
        self.irq_armed = false;
        armed
    }
}

#[cfg(test)]
#[path = "uart_test.rs"]
mod tests;
