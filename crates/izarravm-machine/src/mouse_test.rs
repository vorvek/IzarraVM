// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn magic_knock_switches_to_intellimouse() {
    let mut m = Ps2Mouse::default();
    for rate in [200u8, 100, 80] {
        m.write_byte(0xF3); // set sample rate
        m.queue.clear(); // drop the ACK
        m.write_byte(rate); // the parameter
        m.queue.clear();
    }
    m.write_byte(0xF2); // get device id
    assert_eq!(m.queue.pop_front(), Some(0xFA)); // ACK
    assert_eq!(m.queue.pop_front(), Some(0x03)); // IntelliMouse id
}

#[test]
fn intellimouse_packet_is_four_bytes_with_z() {
    let mut m = Ps2Mouse::default();
    for rate in [200u8, 100, 80] {
        m.write_byte(0xF3);
        m.write_byte(rate);
    }
    m.queue.clear();
    m.reporting = true;
    assert!(m.queue_movement(0, 0, 0, -1)); // dz = -1
    assert_eq!(m.queue.len(), 4);
    let _b0 = m.queue.pop_front().unwrap();
    let _x = m.queue.pop_front().unwrap();
    let _y = m.queue.pop_front().unwrap();
    assert_eq!(m.queue.pop_front().unwrap() as i8, -1); // Z byte
}

#[test]
fn wrong_sample_rate_sequence_does_not_knock() {
    let mut m = Ps2Mouse::default();
    for rate in [200u8, 100, 81] {
        // 81, not 80 -> not the magic knock
        m.write_byte(0xF3);
        m.write_byte(rate);
    }
    m.queue.clear();
    m.write_byte(0xF2);
    assert_eq!(m.queue.pop_front(), Some(0xFA));
    assert_eq!(m.queue.pop_front(), Some(0x00)); // still standard PS/2 id
    m.reporting = true;
    assert!(m.queue_movement(1, 1, 0, 0));
    assert_eq!(m.queue.len(), 3); // still a 3-byte packet (no wheel)
}

#[test]
fn reset_drops_back_to_three_byte() {
    let mut m = Ps2Mouse::default();
    for rate in [200u8, 100, 80] {
        m.write_byte(0xF3);
        m.write_byte(rate);
    }
    m.write_byte(0xFF); // reset
    m.queue.clear();
    m.write_byte(0xF2);
    assert_eq!(m.queue.pop_front(), Some(0xFA));
    assert_eq!(m.queue.pop_front(), Some(0x00)); // back to standard id
    m.reporting = true;
    assert!(m.queue_movement(1, 1, 0, 0));
    assert_eq!(m.queue.len(), 3); // 3-byte packet again
}
