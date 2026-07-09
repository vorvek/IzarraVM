// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn injected_scancode_reads_back_with_obf_and_irq() {
    let mut kbd = Keyboard8042::default();
    kbd.push_scancodes(&[0x1e]); // 'A' make
    assert_eq!(kbd.read_port(0x64).unwrap() & STATUS_OBF, STATUS_OBF);
    assert!(kbd.take_irq(), "a latched key arms IRQ1");
    assert_eq!(kbd.read_port(0x60), Some(0x1e));
    assert_eq!(
        kbd.read_port(0x64).unwrap() & STATUS_OBF,
        0,
        "OBF clears after read"
    );
}

#[test]
fn reread_returns_stale_byte_until_next_arrives() {
    // A real 8042 keeps the last byte in the output register after a read; a
    // re-read (the BIOS handler reading 0x60 after a game's INT 09h already
    // did) returns the same value rather than 0. Prince of Persia's shift
    // state depends on this.
    let mut kbd = Keyboard8042::default();
    kbd.push_scancodes(&[0x2a]); // shift make
    assert_eq!(kbd.read_port(0x60), Some(0x2a));
    assert_eq!(
        kbd.read_port(0x64).unwrap() & STATUS_OBF,
        0,
        "OBF clears after the read"
    );
    assert_eq!(
        kbd.read_port(0x60),
        Some(0x2a),
        "re-read returns the stale byte, not 0"
    );
    kbd.push_scancodes(&[0xaa]); // shift break replaces it
    assert_eq!(kbd.read_port(0x60), Some(0xaa));
    assert_eq!(kbd.read_port(0x60), Some(0xaa), "now stale on 0xaa");
}

#[test]
fn second_scancode_re_arms_irq_after_read() {
    let mut kbd = Keyboard8042::default();
    kbd.push_scancodes(&[0x1e, 0x9e]); // make + break
    assert!(kbd.take_irq());
    assert_eq!(kbd.read_port(0x60), Some(0x1e));
    assert!(
        kbd.take_irq(),
        "reading latches the next byte and re-arms IRQ1"
    );
    assert_eq!(kbd.read_port(0x60), Some(0x9e));
}

#[test]
fn controller_self_test_returns_0x55() {
    let mut kbd = Keyboard8042::default();
    kbd.write_port(0x64, 0xaa);
    assert_eq!(kbd.read_port(0x60), Some(0x55));
}

#[test]
fn irq_disabled_in_command_byte_does_not_arm() {
    let mut kbd = Keyboard8042::default();
    kbd.write_port(0x64, 0x60); // write command byte
    kbd.write_port(0x60, 0x00); // IRQ1 disabled
    kbd.push_scancodes(&[0x1e]);
    assert!(!kbd.take_irq());
}

/// Enable the mouse the way a driver does: command byte bit 1 set so IRQ12
/// fires, then a 0xD4-routed 0xF4 (enable data reporting) acked.
fn enable_mouse(kbd: &mut Keyboard8042) {
    kbd.write_port(0x64, 0x60); // write command byte
    kbd.write_port(0x60, 0x03); // IRQ1 + IRQ12 (mouse) enabled
    kbd.write_port(0x64, 0xD4); // next 0x60 byte goes to the mouse
    kbd.write_port(0x60, 0xF4); // enable data reporting
    assert_eq!(kbd.read_port(0x60), Some(0xFA), "mouse acks enable");
    // The ACK read armed the aux settle window (see AUX_BYTE_SETTLE_US);
    // clear it so callers can inject and immediately read a movement
    // packet, matching a real driver that doesn't move the mouse in the
    // same instant the enable handshake completes.
    kbd.advance_mouse_pacing(AUX_BYTE_SETTLE_US);
}

#[test]
fn aux_write_path_routes_to_mouse_and_acks() {
    let mut kbd = Keyboard8042::default();
    enable_mouse(&mut kbd);
    assert!(kbd.mouse.reporting, "0xF4 enabled data reporting");
}

#[test]
fn keyboard_reread_is_not_hijacked_by_a_pending_mouse_byte() {
    // Regression for the Prince of Persia screen-corruption/freeze bug.
    // PoP's own INT 09h handler reads 0x60, then chains to the BIOS's
    // INT 09h handler, which reads 0x60 again expecting the same stale
    // scancode back (see reread_returns_stale_byte_until_next_arrives).
    // If a mouse packet happens to be queued behind it at that exact
    // moment (e.g. mid-flick), the second read must not see a freshly
    // latched mouse byte instead -- that corrupts the BIOS's
    // shift-state handling and desyncs the mouse driver's own packet
    // framing.
    let mut kbd = Keyboard8042::default();
    enable_mouse(&mut kbd);
    kbd.take_irq12(); // drain the IRQ12 edge the ACK byte itself armed
    kbd.push_scancodes(&[0x1e]); // 'A' make: latches immediately
    // A mouse packet queues up behind the held keyboard byte. It cannot
    // latch yet -- the output register is occupied -- so no IRQ12 arms.
    assert!(
        !kbd.inject_mouse(5, -3, 0x01),
        "the mouse byte is queued but not yet latched"
    );

    // PoP's own ISR consumes the keyboard byte...
    assert_eq!(kbd.read_port(0x60), Some(0x1e));
    assert_eq!(
        kbd.read_port(0x64).unwrap() & STATUS_AUX,
        0,
        "the byte just consumed was the keyboard's"
    );
    // ...then the chained BIOS handler re-reads 0x60, expecting the
    // same stale scancode -- not the mouse byte waiting right behind it.
    assert_eq!(
        kbd.read_port(0x60),
        Some(0x1e),
        "a pending mouse byte must not hijack the chained re-read"
    );
    assert_eq!(
        kbd.read_port(0x64).unwrap() & STATUS_AUX,
        0,
        "the re-read is still flagged as a keyboard byte, not AUX"
    );
    assert!(
        !kbd.take_irq12(),
        "no mouse interrupt has fired yet -- its byte is still held back"
    );

    // Once the settle window elapses, the untouched mouse byte latches
    // normally and the mouse driver's packet framing is never disturbed.
    kbd.advance_mouse_pacing(2000.0); // comfortably past the settle window
    let status = kbd.read_port(0x64).unwrap();
    assert_eq!(
        status & STATUS_OBF,
        STATUS_OBF,
        "the mouse byte now latches"
    );
    assert_eq!(
        status & STATUS_AUX,
        STATUS_AUX,
        "and is correctly flagged AUX"
    );
    assert!(
        kbd.take_irq12(),
        "IRQ12 arms once the byte actually latches"
    );
}

#[test]
fn movement_queues_three_byte_packet_and_arms_irq12() {
    let mut kbd = Keyboard8042::default();
    enable_mouse(&mut kbd);
    // Move right 5, up 3 (screen dy = -3), left button held.
    let irq = kbd.inject_mouse(5, -3, 0x01);
    assert!(irq, "movement with reporting on raises IRQ12");
    // First byte: AUX bit set in status.
    let status = kbd.read_port(0x64).unwrap();
    assert_eq!(status & STATUS_OBF, STATUS_OBF);
    assert_eq!(status & STATUS_AUX, STATUS_AUX, "byte is from the mouse");
    // Flags byte: bit3 set, left button (bit0); +x and (screen up -> +y)
    // both positive, so neither sign bit is set.
    let b0 = kbd.read_port(0x60).unwrap();
    assert_eq!(b0 & 0x08, 0x08, "always-one bit");
    assert_eq!(b0 & 0x01, 0x01, "left button");
    assert_eq!(b0 & 0x10, 0x00, "X positive, no sign");
    assert_eq!(b0 & 0x20, 0x00, "Y positive (up), no sign");
    // Each byte is paced ~1ms apart (AUX_BYTE_SETTLE_US), matching real
    // PS/2 serial transmission; advance past it between reads.
    kbd.advance_mouse_pacing(AUX_BYTE_SETTLE_US);
    let bx = kbd.read_port(0x60).unwrap();
    assert_eq!(bx, 5, "dx byte");
    kbd.advance_mouse_pacing(AUX_BYTE_SETTLE_US);
    let by = kbd.read_port(0x60).unwrap();
    assert_eq!(by, 3, "dy byte (negated screen delta)");
}

#[test]
fn movement_without_reporting_is_dropped() {
    let mut kbd = Keyboard8042::default();
    // No enable: reporting is off by default.
    let irq = kbd.inject_mouse(10, 10, 0);
    assert!(!irq, "no IRQ12 while reporting is disabled");
    assert_eq!(
        kbd.read_port(0x64).unwrap() & STATUS_OBF,
        0,
        "nothing latched"
    );
}

#[test]
fn set_mouse_reporting_enables_packets_without_a_command() {
    let mut kbd = Keyboard8042::default();
    // Command byte bit1 must be set for IRQ12 to arm in latch_next.
    kbd.write_port(0x64, 0x60); // write command byte
    kbd.write_port(0x60, 0x03); // IRQ1 + IRQ12 enabled
    // Enable reporting via the new seam, not the 0xD4/0xF4 command path.
    kbd.set_mouse_reporting(true);
    assert!(kbd.mouse.reporting, "seam flips the reporting flag");
    // A queued packet now latches and arms IRQ12, with no spurious 0xFA ACK.
    let pulse = kbd.inject_mouse(5, -3, 0x01);
    assert!(
        pulse,
        "reporting on plus an armed mouse byte requests IRQ12"
    );
    assert!(kbd.take_irq12(), "IRQ12 edge is pending");
    let b0 = kbd.read_port(0x60).unwrap();
    assert_eq!(b0 & 0x08, 0x08, "sync bit set on packet byte 0");
    assert_eq!(b0 & 0x01, 0x01, "left button reported");
}

#[test]
fn disable_clears_a_pending_irq12_edge() {
    // Enable IRQ12 and reporting, then queue a packet so a byte latches and
    // arms the IRQ12 edge. Disable the mouse interrupt before the run loop
    // consumes that edge (take_irq12). The disable must drop the pending edge,
    // so a disabled mouse raises no interrupt.
    let mut kbd = Keyboard8042::default();
    kbd.set_mouse_irq(true); // command byte bit1 = IRQ12 enabled
    kbd.set_mouse_reporting(true);
    let pulse = kbd.inject_mouse(5, -3, 0x01);
    assert!(pulse, "an armed mouse byte requests IRQ12 while enabled");
    kbd.set_mouse_irq(false); // disable before take_irq12 consumes the edge
    assert!(
        !kbd.take_irq12(),
        "disabling the mouse interrupt drops the pending IRQ12 edge"
    );
}

#[test]
fn negative_delta_sets_sign_bits() {
    let mut kbd = Keyboard8042::default();
    enable_mouse(&mut kbd);
    // Move left 4 (dx -4), down 7 (screen dy +7 -> packet y -7).
    kbd.inject_mouse(-4, 7, 0);
    let b0 = kbd.read_port(0x60).unwrap();
    assert_eq!(b0 & 0x10, 0x10, "X sign set for leftward move");
    assert_eq!(b0 & 0x20, 0x20, "Y sign set for downward move");
    kbd.advance_mouse_pacing(AUX_BYTE_SETTLE_US);
    let bx = kbd.read_port(0x60).unwrap();
    assert_eq!(bx as i8 as i32, -4, "dx is -4 two's complement");
    kbd.advance_mouse_pacing(AUX_BYTE_SETTLE_US);
    let by = kbd.read_port(0x60).unwrap();
    assert_eq!(by as i8 as i32, -7, "dy is -7 (down)");
}

// Slice A: output port and A20 gate state.

#[test]
fn a20_enabled_by_default() {
    let kbd = Keyboard8042::default();
    assert!(kbd.a20_enabled(), "default output port 0x03 has A20 on");
}

#[test]
fn write_output_port_toggles_a20() {
    let mut kbd = Keyboard8042::default();
    kbd.write_port(0x64, 0xD1); // arm: next 0x60 byte drives the output port
    kbd.write_port(0x60, 0x01); // A20 bit clear, reset line high
    assert!(!kbd.a20_enabled(), "A20 off after clearing bit 1");
    kbd.write_port(0x64, 0xD1);
    kbd.write_port(0x60, 0x03); // A20 bit set again
    assert!(kbd.a20_enabled(), "A20 back on after setting bit 1");
}

// Slice B: read-port commands on 0x64.

#[test]
fn read_output_port_returns_live_state() {
    let mut kbd = Keyboard8042::default();
    kbd.write_port(0x64, 0xD1);
    kbd.write_port(0x60, 0x02); // A20 on, reset low
    kbd.write_port(0x64, 0xD0); // read output port
    assert_eq!(
        kbd.read_port(0x60),
        Some(0x02),
        "0xD0 reads what 0xD1 wrote"
    );
}

#[test]
fn read_input_port_reports_unlocked_normal() {
    let mut kbd = Keyboard8042::default();
    kbd.write_port(0x64, 0xC0);
    let byte = kbd.read_port(0x60).unwrap();
    assert_eq!(byte & 0x80, 0x80, "bit7 set: keyboard not locked");
    assert_eq!(byte & 0x20, 0x20, "bit5 set: normal");
}

#[test]
fn read_test_inputs_idle_high() {
    let mut kbd = Keyboard8042::default();
    kbd.write_port(0x64, 0xE0);
    assert_eq!(kbd.read_port(0x60), Some(0x03), "kbd clock+data idle high");
}

// Slice C: interface-test labels.

#[test]
fn keyboard_interface_test_returns_zero() {
    let mut kbd = Keyboard8042::default();
    kbd.write_port(0x64, 0xAB); // keyboard interface test
    assert_eq!(kbd.read_port(0x60), Some(0x00), "0xAB reports no error");
}

#[test]
fn aux_interface_test_returns_zero() {
    let mut kbd = Keyboard8042::default();
    kbd.write_port(0x64, 0xA9); // aux/mouse interface test
    assert_eq!(kbd.read_port(0x60), Some(0x00), "0xA9 reports no error");
}

#[test]
fn read_command_byte_is_not_blocked_by_keyboard_disable() {
    // The BIOS idiom disables the keyboard (0xAD, command-byte bit4) before reading
    // the command byte. The 0x20 controller response must still reach the output
    // buffer, since the disable bit only holds back queued scancodes.
    let mut kbd = Keyboard8042::default();
    kbd.write_port(0x64, 0x60); // write command byte
    kbd.write_port(0x60, 0x10); // bit4 set: keyboard clock disabled
    kbd.write_port(0x64, 0x20); // read command byte
    assert_eq!(kbd.read_port(0x60), Some(0x10));
}

// Slice D: keyboard-device command set on the 0x60 non-data path.

#[test]
fn echo_answers_ee_not_ack() {
    let mut kbd = Keyboard8042::default();
    kbd.write_port(0x60, 0xEE); // echo
    assert_eq!(
        kbd.read_port(0x60),
        Some(0xEE),
        "echo replies 0xEE, not 0xFA"
    );
}

#[test]
fn read_id_returns_ack_then_mf2_id() {
    let mut kbd = Keyboard8042::default();
    kbd.write_port(0x60, 0xF2); // read ID
    assert_eq!(kbd.read_port(0x60), Some(0xFA), "ACK first");
    assert_eq!(kbd.read_port(0x60), Some(0xAB), "ID low byte");
    assert_eq!(kbd.read_port(0x60), Some(0x41), "ID high byte");
}

#[test]
fn scan_set_store_then_get_roundtrips() {
    let mut kbd = Keyboard8042::default();
    kbd.write_port(0x60, 0xF0); // select scancode set
    assert_eq!(kbd.read_port(0x60), Some(0xFA), "ACK the command");
    kbd.write_port(0x60, 0x01); // store set 1
    assert_eq!(kbd.read_port(0x60), Some(0xFA), "ACK the parameter");
    kbd.write_port(0x60, 0xF0); // ask again
    assert_eq!(kbd.read_port(0x60), Some(0xFA), "ACK the query");
    kbd.write_port(0x60, 0x00); // get current set
    assert_eq!(kbd.read_port(0x60), Some(0xFA), "ACK the get");
    assert_eq!(kbd.read_port(0x60), Some(0x01), "reports the stored set");
}

#[test]
fn set_typematic_consumes_rate_without_spurious_ack() {
    let mut kbd = Keyboard8042::default();
    kbd.write_port(0x60, 0xF3); // set typematic rate/delay
    assert_eq!(kbd.read_port(0x60), Some(0xFA), "ACK the command");
    kbd.write_port(0x60, 0x2A); // the rate byte (consumed as a parameter)
    assert_eq!(kbd.read_port(0x60), Some(0xFA), "ACK the rate byte");
    // The rate byte must not be mistaken for a fresh command: no extra ACK.
    assert_eq!(
        kbd.read_port(0x64).unwrap() & STATUS_OBF,
        0,
        "no spurious second response"
    );
}

#[test]
fn resend_repushes_last_scancode() {
    let mut kbd = Keyboard8042::default();
    kbd.push_scancodes(&[0x1E]); // 'A' make
    assert_eq!(kbd.read_port(0x60), Some(0x1E));
    kbd.write_port(0x60, 0xFE); // resend
    assert_eq!(kbd.read_port(0x60), Some(0x1E), "resend repeats last byte");
}

#[test]
fn reset_acks_then_self_tests() {
    let mut kbd = Keyboard8042::default();
    kbd.write_port(0x60, 0xFF); // keyboard reset
    assert_eq!(kbd.read_port(0x60), Some(0xFA), "ACK");
    assert_eq!(kbd.read_port(0x60), Some(0xAA), "BAT self-test pass");
}

#[test]
fn enable_disable_keyboard_holds_then_releases_scancodes() {
    let mut kbd = Keyboard8042::default();
    kbd.write_port(0x64, 0xAD); // disable keyboard (cmd-byte bit4)
    kbd.push_scancodes(&[0x1E]);
    assert_eq!(
        kbd.read_port(0x64).unwrap() & STATUS_OBF,
        0,
        "masked scancode stays queued, not latched"
    );
    kbd.write_port(0x64, 0xAE); // re-enable keyboard
    assert_eq!(
        kbd.read_port(0x60),
        Some(0x1E),
        "held scancode latches on re-enable"
    );
}

#[test]
fn disable_aux_holds_then_releases_mouse_bytes() {
    let mut kbd = Keyboard8042::default();
    enable_mouse(&mut kbd);
    kbd.write_port(0x64, 0xA7); // disable aux (cmd-byte bit5)
    kbd.inject_mouse(3, 0, 0);
    assert_eq!(
        kbd.read_port(0x64).unwrap() & STATUS_OBF,
        0,
        "masked mouse byte stays queued"
    );
    kbd.write_port(0x64, 0xA8); // re-enable aux
    let status = kbd.read_port(0x64).unwrap();
    assert_eq!(status & STATUS_OBF, STATUS_OBF, "byte latches on re-enable");
    assert_eq!(status & STATUS_AUX, STATUS_AUX, "it is an aux byte");
}
