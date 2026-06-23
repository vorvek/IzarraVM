//! 16450/16550 UART as software sees it. Models a COM port (COM1 at base 0x3F8
//! on IRQ4, COM2 at 0x2F8 on IRQ3): the eight register offsets, the DLAB divisor
//! latches, the IIR priority encoder, loopback (MCR bit4), and the scratch
//! register a 16450-vs-16550 probe reads back. Transmit drains instantly into a
//! capture sink the POST log reads, so there is no host backpressure and no baud
//! timing.

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
const IIR_THRE: u8 = 0x02; // transmit holding register empty
const IIR_MS: u8 = 0x00; // modem status (lowest priority)

// FCR (offset 2 write) bits.
const FCR_FIFO_ENABLE: u8 = 0x01;

/// One COM port. Named registers, not a raw array, so each one carries its own
/// reset value and read/write side effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uart16450 {
    base: u16,    // first I/O port (0x3F8 for COM1, 0x2F8 for COM2)
    rbr: u8,      // receive buffer (read side of offset 0)
    ier: u8,      // interrupt enable
    fcr: u8,      // FIFO control (write side of offset 2)
    lcr: u8,      // line control; bit7 is DLAB
    mcr: u8,      // modem control
    lsr: u8,      // line status
    msr: u8,      // modem status
    scr: u8,      // scratch
    divisor: u16, // 16-bit baud divisor from DLL/DLM

    rx_ready: bool,     // a byte sits in rbr waiting to be read
    thre_pending: bool, // a THR-empty interrupt source is latched until IIR read
    irq_armed: bool,    // edge: the interrupt line just asserted

    output: Vec<u8>, // captured transmit bytes (the POST log sink)
}

impl Default for Uart16450 {
    fn default() -> Self {
        Self {
            base: COM1_BASE,
            rbr: 0,
            ier: 0,
            fcr: 0,
            lcr: 0,
            mcr: 0,
            lsr: LSR_THRE | LSR_TEMT, // reset 0x60: transmitter always empty
            msr: 0,
            scr: 0,
            divisor: 0,
            rx_ready: false,
            thre_pending: false,
            irq_armed: false,
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
    pub fn read_port(&mut self, port: u16) -> Option<u8> {
        let offset = self.map_offset(port)?;
        let value = match offset {
            0 => {
                if self.dlab() {
                    (self.divisor & 0x00ff) as u8 // DLL
                } else {
                    // Reading RBR clears data-ready and the RX-data source.
                    self.rx_ready = false;
                    self.lsr &= !LSR_DR;
                    self.rbr
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
            5 => self.lsr,
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
                    self.ier = value & 0x0f;
                }
            }
            2 => self.fcr = value, // FCR is the write side of offset 2, distinct from IIR
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

    /// THR write. In loopback the byte returns through RBR and is not captured;
    /// otherwise it drains into the capture sink.
    fn transmit(&mut self, value: u8) {
        if self.mcr & MCR_LOOP != 0 {
            self.rbr = value;
            self.rx_ready = true;
            self.lsr |= LSR_DR;
        } else {
            // Limit: instant-drain tx, no host backpressure
            self.output.push(value);
        }
        // The holding register empties at once, so a THR-empty source latches.
        self.thre_pending = true;
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
        // Limit: FIFO control bits decoded, depth/timeout not modeled (instant-drain tx)
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
        } else if self.ier & IER_RDA != 0 && self.rx_ready {
            IIR_RDA
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

    /// Recompute whether the interrupt line is asserted and arm the edge.
    /// The line drives the port's IRQ (IRQ4 for COM1, IRQ3 for COM2) only when
    /// MCR OUT2 gates it.
    // Limit: edge-armed on register access, not a continuously level-held
    // line; sufficient because the 8259 takes IRQ4 as an edge and every guest
    // interaction touches a UART port.
    fn refresh_irq(&mut self) {
        let asserted = self.mcr & MCR_OUT2 != 0 && self.pending_code() != IIR_NONE;
        if asserted {
            self.irq_armed = true;
        }
    }

    /// Take the pending interrupt edge; the caller pulses IRQ4.
    pub fn take_irq(&mut self) -> bool {
        let armed = self.irq_armed;
        self.irq_armed = false;
        armed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RBR: u16 = COM1_BASE;
    const THR: u16 = COM1_BASE;
    const IER: u16 = COM1_BASE + 1;
    const IIR: u16 = COM1_BASE + 2;
    const FCR: u16 = COM1_BASE + 2;
    const LCR: u16 = COM1_BASE + 3;
    const MCR: u16 = COM1_BASE + 4;
    const LSR: u16 = COM1_BASE + 5;
    const MSR: u16 = COM1_BASE + 6;
    const SCR: u16 = COM1_BASE + 7;

    #[test]
    fn ignores_ports_outside_com1() {
        let mut uart = Uart16450::default();
        assert_eq!(uart.read_port(0x02f8), None);
        assert!(!uart.write_port(0x02f8, 0x55));
    }

    #[test]
    fn dlab_switches_offset_0_and_1() {
        let mut uart = Uart16450::default();
        // DLAB clear: offset 0 is THR/RBR, offset 1 is IER.
        uart.write_port(IER, 0x0f);
        assert_eq!(uart.read_port(IER), Some(0x0f), "IER reads back");
        // Set DLAB and program the divisor latches.
        uart.write_port(LCR, 0x80);
        uart.write_port(THR, 0x01); // DLL
        uart.write_port(IER, 0xc2); // DLM
        assert_eq!(uart.read_port(RBR), Some(0x01), "DLL low byte");
        assert_eq!(uart.read_port(IER), Some(0xc2), "DLM high byte");
        assert_eq!(uart.divisor, 0xc201);
        // Clear DLAB: the IER value survived the baud programming.
        uart.write_port(LCR, 0x00);
        assert_eq!(
            uart.read_port(IER),
            Some(0x0f),
            "IER not corrupted by baud set"
        );
    }

    #[test]
    fn iir_reset_value_is_one() {
        let mut uart = Uart16450::default();
        assert_eq!(uart.read_port(IIR), Some(IIR_NONE));
    }

    #[test]
    fn iir_read_and_fcr_write_are_independent() {
        let mut uart = Uart16450::default();
        // Writing FCR must not change what IIR reads back.
        uart.write_port(FCR, FCR_FIFO_ENABLE);
        let iir = uart.read_port(IIR).unwrap();
        assert_eq!(iir & 0x0f, IIR_NONE, "no source pending");
        assert_eq!(iir & 0xc0, 0xc0, "FIFO-enabled bits report from FCR");
        assert_eq!(uart.fcr, FCR_FIFO_ENABLE, "FCR holds its own value");
    }

    #[test]
    fn mcr_reserved_bits_read_zero() {
        let mut uart = Uart16450::default();
        uart.write_port(MCR, 0xff);
        assert_eq!(uart.read_port(MCR), Some(0x1f), "bits 5-7 read back 0");
    }

    #[test]
    fn scratch_register_round_trips() {
        let mut uart = Uart16450::default();
        uart.write_port(SCR, 0xa5);
        assert_eq!(uart.read_port(SCR), Some(0xa5));
    }

    #[test]
    fn loopback_cross_wires_mcr_into_msr() {
        let mut uart = Uart16450::default();
        // Enter loopback and raise DTR, RTS, OUT1, OUT2.
        uart.write_port(MCR, MCR_LOOP | MCR_DTR | MCR_RTS | MCR_OUT1 | MCR_OUT2);
        let msr = uart.read_port(MSR).unwrap();
        assert_ne!(msr & MSR_DSR, 0, "DTR -> DSR");
        assert_ne!(msr & MSR_CTS, 0, "RTS -> CTS");
        assert_ne!(msr & MSR_RI, 0, "OUT1 -> RI");
        assert_ne!(msr & MSR_DCD, 0, "OUT2 -> DCD");
        // The delta bits were set by the change; reading MSR cleared them.
        let after = uart.read_port(MSR).unwrap();
        assert_eq!(after & 0x0f, 0, "delta bits clear after read");
    }

    #[test]
    fn leaving_loopback_drops_msr_inputs_to_no_modem() {
        let mut uart = Uart16450::default();
        uart.write_port(MCR, MCR_LOOP | MCR_DTR | MCR_RTS);
        uart.read_port(MSR); // consume the entering-loopback deltas
        // Leave loopback: the cross-wired inputs disconnect and read low again.
        uart.write_port(MCR, MCR_DTR | MCR_RTS);
        let msr = uart.read_port(MSR).unwrap();
        assert_eq!(
            msr & (MSR_CTS | MSR_DSR | MSR_RI | MSR_DCD),
            0,
            "inputs low"
        );
        // The two that were high (CTS from RTS, DSR from DTR) flagged a delta.
        assert_ne!(msr & MSR_DCTS, 0, "CTS dropped");
        assert_ne!(msr & MSR_DDSR, 0, "DSR dropped");
    }

    #[test]
    fn loopback_byte_returns_through_rbr() {
        let mut uart = Uart16450::default();
        uart.write_port(MCR, MCR_LOOP | MCR_OUT2);
        uart.write_port(THR, b'Z');
        let lsr = uart.read_port(LSR).unwrap();
        assert_ne!(lsr & LSR_DR, 0, "data ready after looped write");
        assert_eq!(uart.read_port(RBR), Some(b'Z'), "looped byte readable");
        // SOUT is disconnected in loopback, so nothing reaches the capture sink.
        assert!(uart.output().is_empty(), "loopback does not capture");
        // Data ready clears once RBR is read.
        let lsr = uart.read_port(LSR).unwrap();
        assert_eq!(lsr & LSR_DR, 0, "DR cleared after RBR read");
    }

    #[test]
    fn non_loopback_tx_captures_and_lsr_stays_empty() {
        let mut uart = Uart16450::default();
        uart.write_port(THR, b'H');
        uart.write_port(THR, b'i');
        assert_eq!(uart.output(), b"Hi");
        let lsr = uart.read_port(LSR).unwrap();
        assert_ne!(lsr & LSR_THRE, 0, "THRE set");
        assert_ne!(lsr & LSR_TEMT, 0, "TEMT set");
    }

    #[test]
    fn thre_interrupt_asserts_irq_and_iir_reports_it() {
        let mut uart = Uart16450::default();
        // Enable the THRE interrupt and gate the line with OUT2.
        uart.write_port(IER, IER_THRE);
        uart.write_port(MCR, MCR_OUT2);
        // A transmit latches the THR-empty source.
        uart.write_port(THR, b'x');
        assert!(uart.take_irq(), "IRQ4 edge armed");
        let iir = uart.read_port(IIR).unwrap();
        assert_eq!(iir & 0x0f, IIR_THRE, "IIR reports THR empty");
        // Reading IIR cleared the THRE source.
        let iir = uart.read_port(IIR).unwrap();
        assert_eq!(iir & 0x0f, IIR_NONE, "THRE source cleared by IIR read");
    }

    #[test]
    fn irq_does_not_assert_without_out2() {
        let mut uart = Uart16450::default();
        // THRE enabled but OUT2 (the global gate) is clear.
        uart.write_port(IER, IER_THRE);
        uart.write_port(THR, b'x');
        assert!(!uart.take_irq(), "no IRQ without OUT2 gate");
    }

    #[test]
    fn loopback_rx_data_raises_rda_interrupt() {
        let mut uart = Uart16450::default();
        // Enable received-data interrupt, gate with OUT2, enter loopback.
        uart.write_port(IER, IER_RDA);
        uart.write_port(MCR, MCR_LOOP | MCR_OUT2);
        uart.write_port(THR, b'A'); // loops into RBR, sets DR
        assert!(uart.take_irq(), "RDA edge armed");
        let iir = uart.read_port(IIR).unwrap();
        assert_eq!(iir & 0x0f, IIR_RDA, "IIR reports received data available");
    }

    #[test]
    fn com2_decodes_its_own_window_and_ignores_com1() {
        let mut uart = Uart16450::com2();
        // The scratch register at the COM2 base round-trips like COM1's does.
        uart.write_port(COM2_BASE + 7, 0x5a);
        assert_eq!(uart.read_port(COM2_BASE + 7), Some(0x5a), "COM2 scratch");
        // COM2 ignores the COM1 window, and a COM1 instance ignores COM2's.
        assert_eq!(uart.read_port(COM1_BASE + 7), None, "COM2 skips COM1 ports");
        let mut com1 = Uart16450::default();
        assert_eq!(com1.read_port(COM2_BASE + 7), None, "COM1 skips COM2 ports");
    }

    #[test]
    fn com2_transmit_captures_like_com1() {
        let mut uart = Uart16450::com2();
        // THR at the COM2 base (DLAB clear) drains into the capture sink.
        uart.write_port(COM2_BASE, b'O');
        uart.write_port(COM2_BASE, b'k');
        assert_eq!(uart.output(), b"Ok");
        let lsr = uart.read_port(COM2_BASE + 5).unwrap();
        assert_ne!(lsr & LSR_THRE, 0, "THRE set");
    }
}
