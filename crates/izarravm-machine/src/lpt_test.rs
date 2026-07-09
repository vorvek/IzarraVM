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
    assert_eq!(lpt.output(), b"A");
}

#[test]
fn two_chars_print_in_order() {
    let mut lpt = Lpt::default();
    for ch in b"Hi" {
        lpt.write_port(0x0378, *ch);
        lpt.write_port(0x037A, 0x01);
        lpt.write_port(0x037A, 0x00);
    }
    assert_eq!(lpt.output(), b"Hi");
}

#[test]
fn control_write_without_fresh_strobe_edge_does_not_double_capture() {
    let mut lpt = Lpt::default();
    lpt.write_port(0x0378, b'Z');
    lpt.write_port(0x037A, 0x01); // edge: captures once
    lpt.write_port(0x037A, 0x09); // strobe still asserted (bit0 set): no recapture
    assert_eq!(lpt.output(), b"Z");
}

#[test]
fn strobe_with_irq_enable_arms_irq7_once() {
    let mut lpt = Lpt::default();
    lpt.write_port(0x0378, b'Q');
    lpt.write_port(0x037A, 0x11); // -Strobe + IRQ-enable (bit4)
    assert!(lpt.take_irq(), "strobed byte arms IRQ7");
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
    assert_eq!(lpt.output(), b"P");
}
