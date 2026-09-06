// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn status_reads_ready_printer() {
    let lpt = Lpt::default();
    assert_eq!(lpt.read_port(0x0379), Some(0xDF), "idle printer status");
}

#[test]
fn data_write_then_strobe_captures_one_byte() {
    let mut lpt = Lpt::default();
    lpt.write_port(0x0378, b'A'); // data
    lpt.write_port(0x037A, 0x01); // assert -Strobe
    lpt.write_port(0x037A, 0x00); // de-assert
    assert!(lpt.output().is_empty());
    assert_eq!(lpt.read_port(0x0379).unwrap() & STATUS_NOT_BUSY, 0);
    lpt.advance_master_ticks(BUSY_TICKS - 1);
    assert!(lpt.output().is_empty());
    lpt.advance_master_ticks(1);
    assert_eq!(lpt.output(), b"A");
    assert_eq!(lpt.read_port(0x0379).unwrap() & STATUS_NOT_ACK, 0);
    lpt.advance_master_ticks(ACK_TICKS);
    assert_eq!(lpt.read_port(0x0379), Some(STATUS_IDLE));
}

#[test]
fn two_chars_print_in_order() {
    let mut lpt = Lpt::default();
    for ch in b"Hi" {
        lpt.write_port(0x0378, *ch);
        lpt.write_port(0x037A, 0x01);
        lpt.write_port(0x037A, 0x00);
        lpt.advance_master_ticks(lpt.ticks_until_idle());
    }
    assert_eq!(lpt.output(), b"Hi");
}

#[test]
fn control_write_without_fresh_strobe_edge_does_not_double_capture() {
    let mut lpt = Lpt::default();
    lpt.write_port(0x0378, b'Z');
    lpt.write_port(0x037A, 0x01); // edge: captures once
    lpt.write_port(0x037A, 0x09); // strobe still asserted (bit0 set): no recapture
    lpt.advance_master_ticks(lpt.ticks_until_idle());
    assert_eq!(lpt.output(), b"Z");
}

#[test]
fn strobe_with_irq_enable_arms_irq7_once() {
    let mut lpt = Lpt::default();
    lpt.write_port(0x0378, b'Q');
    lpt.write_port(0x037A, 0x11); // -Strobe + IRQ-enable (bit4)
    assert!(!lpt.take_irq(), "IRQ waits for -ACK");
    lpt.advance_master_ticks(BUSY_TICKS - 1);
    assert!(!lpt.take_irq(), "IRQ stays clear before -ACK");
    lpt.advance_master_ticks(1);
    assert!(lpt.take_irq(), "-ACK arms IRQ7");
    assert!(!lpt.take_irq(), "edge is consumed once");
}

#[test]
fn ports_outside_the_range_are_not_claimed() {
    let mut lpt = Lpt::default();
    assert_eq!(lpt.read_port(0x0377), None);
    assert_eq!(lpt.read_port(0x037B), None);
    assert!(!lpt.write_port(0x0377, 0));
    assert!(!lpt.write_port(0x037B, 0));
}

#[test]
fn lpt2_decodes_its_own_window_and_captures() {
    let mut lpt = Lpt::lpt2();
    // LPT2's window is 0x278-0x27A; the LPT1 window is not claimed.
    assert_eq!(lpt.read_port(0x0279), Some(0xDF), "LPT2 idle status");
    assert_eq!(lpt.read_port(0x0379), None, "LPT2 skips the LPT1 window");
    // A data write then a strobe edge captures one byte at the LPT2 base.
    lpt.write_port(0x0278, b'P');
    lpt.write_port(0x027A, 0x01);
    lpt.write_port(0x027A, 0x00);
    lpt.advance_master_ticks(lpt.ticks_until_idle());
    assert_eq!(lpt.output(), b"P");
}

#[test]
fn busy_ack_and_output_are_batch_invariant() {
    let mut whole = Lpt::default();
    whole.write_port(0x0378, b'X');
    whole.write_port(0x037A, CONTROL_STROBE | CONTROL_IRQ_ENABLE);
    whole.write_port(0x037A, CONTROL_IRQ_ENABLE);
    let mut split = whole.clone();

    let span = whole.ticks_until_idle();
    whole.advance_master_ticks(span);
    split.advance_master_ticks(BUSY_TICKS / 2);
    split.advance_master_ticks(span - BUSY_TICKS / 2);
    assert_eq!(whole, split);
    assert_eq!(whole.output(), b"X");
    assert!(whole.take_irq());
}

#[test]
fn irq_enable_is_sampled_at_the_ack_edge() {
    let mut lpt = Lpt::default();
    lpt.write_port(0x0378, b'I');
    lpt.write_port(0x037A, CONTROL_STROBE);
    lpt.write_port(0x037A, CONTROL_IRQ_ENABLE);
    assert_eq!(lpt.ticks_until_irq(), Some(BUSY_TICKS));
    lpt.advance_master_ticks(BUSY_TICKS);
    assert!(lpt.take_irq());
}

#[test]
fn access_catchup_matches_settlement_across_printer_events() {
    for base in [LPT1_BASE, LPT2_BASE] {
        for enabled in [false, true] {
            let mut initial = if base == LPT1_BASE {
                Lpt::default()
            } else {
                Lpt::lpt2()
            };
            initial.write_port(base, b'A');
            initial.write_port(
                base + 2,
                CONTROL_STROBE | if enabled { CONTROL_IRQ_ENABLE } else { 0 },
            );
            for offset in [
                BUSY_TICKS - 1,
                BUSY_TICKS,
                BUSY_TICKS + 1,
                BUSY_TICKS + ACK_TICKS - 1,
                BUSY_TICKS + ACK_TICKS,
                BUSY_TICKS + ACK_TICKS + 1,
            ] {
                for value in [0, CONTROL_IRQ_ENABLE] {
                    let mut split = initial.clone();
                    let mut whole = initial.clone();
                    split.advance_master_ticks(offset);
                    assert_eq!(
                        whole.read_port_at(base + 1, offset),
                        split.read_port(base + 1)
                    );
                    for (port, data) in [
                        (base + 2, value),
                        (base, b'B'),
                        (base + 2, value | CONTROL_STROBE),
                        (base, b'C'),
                    ] {
                        split.write_port(port, data);
                        whole.write_port_at(port, data, offset);
                    }
                    let span = offset + BUSY_TICKS + ACK_TICKS;
                    whole.advance_master_ticks(offset / 2);
                    assert_eq!(whole.advance_credit_ticks(), offset - offset / 2);
                    whole.advance_master_ticks(span - offset / 2);
                    split.advance_master_ticks(span - offset);
                    assert_eq!(whole, split);
                    let expected: &[u8] = if offset >= BUSY_TICKS + ACK_TICKS {
                        b"AB"
                    } else {
                        b"A"
                    };
                    assert_eq!(whole.output(), expected);
                    assert_eq!(
                        whole.take_irq(),
                        if offset < BUSY_TICKS {
                            value != 0
                        } else {
                            enabled || (expected.len() == 2 && value != 0)
                        }
                    );
                }
            }
        }
    }
}
