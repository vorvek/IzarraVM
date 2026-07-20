// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_core::{
    CanonicalSectionId, CanonicalSectionRequirement, CanonicalSectionVersion, CanonicalStateView,
    CanonicalStateWriter,
};

fn canonical_payload(pic: &Pic8259Pair) -> Vec<u8> {
    let projection = pic.canonical_projection();
    let mut state = CanonicalStateWriter::new().unwrap();
    state
        .section(
            CanonicalSectionId::new(0x0002_0004).unwrap(),
            CanonicalSectionVersion::new(1).unwrap(),
            CanonicalSectionRequirement::Required,
            |out| projection.write_payload(out),
        )
        .unwrap();
    let bytes = state.finish().unwrap();
    let view = CanonicalStateView::parse(&bytes).unwrap();
    view.sections()[0].payload().to_vec()
}

fn projection_equivalence_pair(
    master_base: u8,
    master_icw4: u8,
    slave_base: u8,
    slave_id: u8,
    slave_icw4: u8,
) -> Pic8259Pair {
    let mut pic = Pic8259Pair::default();
    pic.write_port(0x20, 0x11);
    pic.write_port(0x21, master_base);
    pic.write_port(0x21, 0x04);
    pic.write_port(0x21, master_icw4);
    pic.write_port(0xa0, 0x11);
    pic.write_port(0xa1, slave_base);
    pic.write_port(0xa1, slave_id);
    pic.write_port(0xa1, slave_icw4);
    pic
}

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

fn level_pair() -> Pic8259Pair {
    let mut pic = Pic8259Pair::default();
    pic.write_port(0x20, 0x19);
    pic.write_port(0x21, 0x08);
    pic.write_port(0x21, 0x04);
    pic.write_port(0x21, 0x01);
    pic.write_port(0xa0, 0x19);
    pic.write_port(0xa1, 0x70);
    pic.write_port(0xa1, 0x02);
    pic.write_port(0xa1, 0x01);
    pic
}

#[test]
fn canonical_payload_pins_every_effective_field_for_both_chips() {
    let pic = Pic8259Pair {
        master: Pic {
            irr: 0x11,
            asserted: 0x12,
            isr: 0x13,
            imr: 0x14,
            icw2: 0x1f,
            icw3: 0x16,
            init: InitStage::ExpectIcw2,
            expect_icw4: true,
            single: false,
            level_triggered: true,
            auto_eoi: false,
            buffered: true,
            is_master: false,
            read_isr: true,
            poll_pending: false,
            special_mask: true,
            sfnm: false,
            lowest: 0x07,
            auto_rotate: true,
        },
        slave: Pic {
            irr: 0x21,
            asserted: 0x22,
            isr: 0x23,
            imr: 0x24,
            icw2: 0x2f,
            icw3: 0xea,
            init: InitStage::ExpectIcw4,
            expect_icw4: false,
            single: true,
            level_triggered: false,
            auto_eoi: true,
            buffered: false,
            is_master: true,
            read_isr: false,
            poll_pending: true,
            special_mask: false,
            sfnm: true,
            lowest: 0x05,
            auto_rotate: false,
        },
    };

    let expected = vec![
        0x11, 0x12, 0x13, 0x14, 0x18, 0x16, 0x01, 0x01, 0x00, 0x01, 0x00, 0x01, 0x00, 0x01, 0x00,
        0x07, 0x00, 0x21, 0x22, 0x23, 0x24, 0x28, 0x02, 0x03, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01,
        0x00, 0x00, 0x05, 0x00,
    ];
    let payload = canonical_payload(&pic);

    assert_eq!(payload.len(), 34);
    assert_eq!(payload, expected);
}

#[test]
fn canonical_payload_pins_each_effective_boolean_offset_independently() {
    for (slave, base) in [(false, 0), (true, 17)] {
        let offsets: &[usize] = if slave {
            &[7, 8, 9, 10, 11, 12, 13, 16]
        } else {
            &[7, 8, 9, 10, 11, 12, 13, 14, 16]
        };
        for &offset in offsets {
            let mut pic = Pic8259Pair::default();
            {
                let target = if slave {
                    &mut pic.slave
                } else {
                    &mut pic.master
                };
                if matches!(offset, 7 | 8) {
                    target.init = InitStage::ExpectIcw2;
                }
                if offset == 16 {
                    target.auto_eoi = true;
                }
            }
            let before = canonical_payload(&pic);
            let target = if slave {
                &mut pic.slave
            } else {
                &mut pic.master
            };
            match offset {
                7 => target.expect_icw4 = true,
                8 => target.single = true,
                9 => target.level_triggered = true,
                10 => target.auto_eoi = true,
                11 => target.read_isr = true,
                12 => target.poll_pending = true,
                13 => target.special_mask = true,
                14 => target.sfnm = true,
                16 => target.auto_rotate = true,
                _ => unreachable!(),
            }
            let after = canonical_payload(&pic);
            let changed: Vec<_> = before
                .iter()
                .zip(&after)
                .enumerate()
                .filter_map(|(index, (left, right))| (left != right).then_some(index))
                .collect();

            assert_eq!(changed, [base + offset]);
            assert_eq!(before[base + offset], 0);
            assert_eq!(after[base + offset], 1);
        }
    }
}

#[test]
fn canonical_projection_normalizes_ready_initialization_residue() {
    for (command, data, vector) in [(0x20, 0x21, 0x20), (0xa0, 0xa1, 0x70)] {
        let mut full = Pic8259Pair::default();
        full.write_port(command, 0x11);
        full.write_port(data, vector);
        full.write_port(data, 0x00);
        full.write_port(data, 0x01);

        let mut single = Pic8259Pair::default();
        single.write_port(command, 0x12);
        single.write_port(data, vector);

        assert_ne!(full, single);
        assert_eq!(canonical_payload(&full), canonical_payload(&single));
        assert_eq!(full.read_port(data), single.read_port(data));

        for pic in [&mut full, &mut single] {
            pic.write_port(command, 0x11);
            pic.write_port(data, vector);
            pic.write_port(data, 0x00);
            pic.write_port(data, 0x01);
        }
        assert_eq!(full, single);
    }
}

#[test]
fn canonical_projection_ignores_bits_that_cannot_change_continuation() {
    let mut plain = projection_equivalence_pair(0x20, 0x01, 0x28, 0x02, 0x01);
    let mut residue = projection_equivalence_pair(0x27, 0x0d, 0x2f, 0xfa, 0x19);
    residue.write_port(0x20, 0x80);
    residue.write_port(0xa0, 0x80);

    assert_ne!(plain, residue);
    assert_eq!(canonical_payload(&plain), canonical_payload(&residue));

    plain.request(3);
    residue.request(3);
    assert_eq!(plain.acknowledge(), Some(0x23));
    assert_eq!(residue.acknowledge(), Some(0x23));
    plain.write_port(0x20, 0x20);
    residue.write_port(0x20, 0x20);

    plain.request(10);
    residue.request(10);
    assert_eq!(plain.acknowledge(), Some(0x2a));
    assert_eq!(residue.acknowledge(), Some(0x2a));
    plain.write_port(0xa0, 0x20);
    residue.write_port(0xa0, 0x20);
    plain.write_port(0x20, 0x20);
    residue.write_port(0x20, 0x20);

    assert_eq!(plain.interrupt_pending(), residue.interrupt_pending());
    assert_eq!(canonical_payload(&plain), canonical_payload(&residue));
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
fn held_master_level_reasserts_only_after_eoi() {
    let mut pic = level_pair();
    pic.set_irq_level(3, true);
    assert_eq!(pic.acknowledge(), Some(0x0b));
    assert_eq!(pic.master.irr & 0x08, 0);
    assert_eq!(pic.master.isr & 0x08, 0x08);
    assert!(!pic.interrupt_pending());

    pic.write_port(0x20, 0x20);
    assert_eq!(pic.master.isr & 0x08, 0);
    assert_eq!(pic.master.irr & 0x08, 0x08);
    assert!(pic.interrupt_pending());

    pic.set_irq_level(3, false);
    assert_eq!(pic.master.irr & 0x08, 0);
}

#[test]
fn deasserted_master_level_does_not_return_after_eoi() {
    let mut pic = level_pair();
    pic.set_irq_level(4, true);
    assert_eq!(pic.acknowledge(), Some(0x0c));
    pic.set_irq_level(4, false);
    pic.write_port(0x20, 0x20);
    assert!(!pic.interrupt_pending());
}

#[test]
fn edge_mode_still_requires_a_new_rising_edge() {
    let mut pic = master_initialized();
    pic.set_irq_level(3, true);
    assert_eq!(pic.acknowledge(), Some(0x0b));
    pic.write_port(0x20, 0x20);
    assert!(!pic.interrupt_pending());
    pic.set_irq_level(3, true);
    assert!(!pic.interrupt_pending());
    pic.set_irq_level(3, false);
    pic.set_irq_level(3, true);
    assert!(pic.interrupt_pending());
}

#[test]
fn automatic_eoi_reasserts_a_held_level_after_acknowledge() {
    let mut pic = Pic8259Pair::default();
    pic.write_port(0x20, 0x19);
    pic.write_port(0x21, 0x08);
    pic.write_port(0x21, 0x04);
    pic.write_port(0x21, 0x03);
    pic.set_irq_level(5, true);
    assert_eq!(pic.acknowledge(), Some(0x0d));
    assert_eq!(pic.master.isr & 0x20, 0);
    assert_eq!(pic.master.irr & 0x20, 0x20);
    assert!(pic.interrupt_pending());
}

#[test]
fn held_slave_level_reasserts_through_the_cascade() {
    let mut pic = level_pair();
    pic.set_irq_level(12, true);
    assert_eq!(pic.acknowledge(), Some(0x74));
    assert_eq!(pic.master.isr & 0x04, 0x04);
    assert_eq!(pic.slave.isr & 0x10, 0x10);

    pic.write_port(0xa0, 0x20);
    assert_eq!(pic.slave.irr & 0x10, 0x10);
    assert!(!pic.interrupt_pending());
    pic.write_port(0x20, 0x20);
    assert!(pic.interrupt_pending());
    assert_eq!(pic.acknowledge(), Some(0x74));

    pic.set_irq_level(12, false);
    pic.write_port(0xa0, 0x20);
    pic.write_port(0x20, 0x20);
    assert!(!pic.interrupt_pending());
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
