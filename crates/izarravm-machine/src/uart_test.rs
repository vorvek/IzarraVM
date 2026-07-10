// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

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
    let deadline = uart.ticks_until_event().unwrap();
    let lsr = uart.read_port(LSR).unwrap();
    assert_eq!(lsr & LSR_DR, 0, "shift register has not completed");
    uart.advance_master_ticks(deadline - 1);
    assert_eq!(uart.read_port(LSR).unwrap() & LSR_DR, 0);
    uart.advance_master_ticks(1);
    let lsr = uart.read_port(LSR).unwrap();
    assert_ne!(lsr & LSR_DR, 0, "data ready at the baud deadline");
    assert_eq!(uart.read_port(RBR), Some(b'Z'), "looped byte readable");
    // SOUT is disconnected in loopback, so nothing reaches the capture sink.
    assert!(uart.output().is_empty(), "loopback does not capture");
    // Data ready clears once RBR is read.
    let lsr = uart.read_port(LSR).unwrap();
    assert_eq!(lsr & LSR_DR, 0, "DR cleared after RBR read");
}

#[test]
fn non_loopback_tx_captures_only_after_baud_deadlines() {
    let mut uart = Uart16450::default();
    uart.write_port(THR, b'H');
    uart.write_port(THR, b'i');
    assert!(uart.output().is_empty());
    let lsr = uart.read_port(LSR).unwrap();
    assert_eq!(lsr & LSR_THRE, 0, "one byte remains in THR");
    assert_eq!(lsr & LSR_TEMT, 0, "shift register is active");
    let deadline = uart.ticks_until_idle();
    uart.advance_master_ticks(deadline - 1);
    assert_eq!(uart.output(), b"H");
    uart.advance_master_ticks(1);
    assert_eq!(uart.output(), b"Hi");
    let lsr = uart.read_port(LSR).unwrap();
    assert_ne!(lsr & LSR_THRE, 0, "THRE set after drain");
    assert_ne!(lsr & LSR_TEMT, 0, "TEMT set after drain");
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
    uart.write_port(THR, b'A');
    assert!(!uart.take_irq(), "no receive edge before baud deadline");
    uart.advance_master_ticks(uart.ticks_until_event().unwrap());
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
    assert!(uart.output().is_empty());
    uart.advance_master_ticks(uart.ticks_until_idle());
    assert_eq!(uart.output(), b"Ok");
    let lsr = uart.read_port(COM2_BASE + 5).unwrap();
    assert_ne!(lsr & LSR_THRE, 0, "THRE set");
}

#[test]
fn divisor_controls_the_exact_character_deadline() {
    let mut fast = Uart16450::default();
    fast.write_port(LCR, 0x80);
    fast.write_port(THR, 1);
    fast.write_port(IER, 0);
    fast.write_port(LCR, 0x03); // 8 data bits, no parity, 1 stop bit
    fast.write_port(THR, b'F');

    let mut slow = Uart16450::default();
    slow.write_port(LCR, 0x80);
    slow.write_port(THR, 2);
    slow.write_port(IER, 0);
    slow.write_port(LCR, 0x03);
    slow.write_port(THR, b'S');

    let fast_deadline = fast.ticks_until_idle();
    let slow_deadline = slow.ticks_until_idle();
    assert_eq!(slow_deadline, fast_deadline * 2);
    slow.advance_master_ticks(slow_deadline - 1);
    assert!(slow.output().is_empty());
    slow.advance_master_ticks(1);
    assert_eq!(slow.output(), b"S");
}

#[test]
fn fifo_character_timeout_raises_receive_irq() {
    let mut uart = Uart16450::default();
    uart.write_port(FCR, FCR_FIFO_ENABLE | 0x40); // four-byte RX trigger
    uart.write_port(IER, IER_RDA);
    uart.write_port(MCR, MCR_LOOP | MCR_OUT2);
    uart.write_port(THR, b'T');

    uart.advance_master_ticks(uart.ticks_until_event().unwrap());
    assert_eq!(uart.read_port(IIR).unwrap() & 0x0f, IIR_NONE);
    let timeout = uart.ticks_until_event().unwrap();
    uart.advance_master_ticks(timeout - 1);
    assert!(!uart.take_irq());
    uart.advance_master_ticks(1);
    assert!(uart.take_irq());
    assert_eq!(uart.read_port(IIR).unwrap() & 0x0f, IIR_TIMEOUT);
    assert_eq!(uart.read_port(RBR), Some(b'T'));
    assert_eq!(uart.read_port(IIR).unwrap() & 0x0f, IIR_NONE);
}

#[test]
fn fifo_receive_threshold_raises_irq_without_waiting_for_timeout() {
    let mut uart = Uart16450::default();
    uart.write_port(FCR, FCR_FIFO_ENABLE | 0x40); // four-byte RX trigger
    uart.write_port(IER, IER_RDA);
    uart.write_port(MCR, MCR_LOOP | MCR_OUT2);
    for byte in b"ABCD" {
        uart.write_port(THR, *byte);
    }
    let character = uart.character_ticks();
    assert_eq!(uart.ticks_until_irq(), Some(character * 4));
    uart.advance_master_ticks(character * 4 - 1);
    assert!(!uart.take_irq());
    uart.advance_master_ticks(1);
    assert!(uart.take_irq());
    assert_eq!(uart.read_port(IIR).unwrap() & 0x0f, IIR_RDA);
}

#[test]
fn queued_transmit_empty_irq_deadline_includes_each_holding_byte() {
    let mut uart = Uart16450::default();
    uart.write_port(FCR, FCR_FIFO_ENABLE);
    uart.write_port(MCR, MCR_OUT2);
    uart.write_port(THR, b'A');
    let _ = uart.read_port(IIR); // acknowledge the first immediate THRE source
    uart.write_port(THR, b'B');
    uart.write_port(THR, b'C');
    uart.write_port(IER, IER_THRE);
    let character = uart.character_ticks();
    assert_eq!(uart.ticks_until_irq(), Some(character * 2));
    uart.advance_master_ticks(character * 2 - 1);
    assert!(!uart.take_irq());
    uart.advance_master_ticks(1);
    assert!(uart.take_irq());
    assert_eq!(uart.read_port(IIR).unwrap() & 0x0f, IIR_THRE);
}

#[test]
fn transmit_and_timeout_state_are_batch_invariant() {
    let mut whole = Uart16450::default();
    whole.write_port(FCR, FCR_FIFO_ENABLE | 0x40);
    whole.write_port(MCR, MCR_LOOP | MCR_OUT2);
    whole.write_port(IER, IER_RDA);
    whole.write_port(THR, b'B');
    let mut split = whole.clone();

    let span = whole.ticks_until_idle() + whole.character_ticks() * 4;
    whole.advance_master_ticks(span);
    split.advance_master_ticks(span / 3);
    split.advance_master_ticks(span - span / 3);
    assert_eq!(whole, split);
    assert_eq!(whole.pending_code(), IIR_TIMEOUT);
}
