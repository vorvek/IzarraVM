// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// Build a plain observed instruction: physical page mirrors the linear page, mode key zero,
/// no control transfer and no side effects.
fn insn(linear: u32, len: u8) -> ObservedInsn {
    ObservedInsn {
        linear,
        len,
        physical_page: linear >> 12,
        mode_key: 0,
        transfer: TransferKind::None,
        is_terminator: false,
        touches_io: false,
    }
}

#[test]
fn straight_line_then_indirect_exit_counts_one_entry() {
    let mut sim = UnitSim::default();
    sim.observe(insn(0x1000, 2));
    sim.observe(insn(0x1002, 3));
    let mut exit = insn(0x1005, 2);
    exit.transfer = TransferKind::Indirect;
    exit.is_terminator = true;
    sim.observe(exit);

    let r = sim.report();
    assert_eq!(r.entries, 1);
    assert_eq!(r.retired_in_units, 3);
    assert_eq!(r.unresolved_exits, 1);
}

#[test]
fn internal_back_edge_stays_in_the_unit() {
    let mut sim = UnitSim::default();
    for _ in 0..3 {
        sim.observe(insn(0x1000, 2));
        let mut back = insn(0x1002, 2);
        back.transfer = TransferKind::DirectNear { target: 0x1000 };
        sim.observe(back);
    }

    let r = sim.report();
    assert_eq!(r.entries, 1);
    assert_eq!(r.retired_in_units, 6);
    assert_eq!(r.unresolved_exits, 0);
}

#[test]
fn cross_page_direct_branch_ends_the_unit() {
    let mut sim = UnitSim::default();
    sim.observe(insn(0x1ffd, 2));
    let mut br = insn(0x1fff, 1);
    br.transfer = TransferKind::DirectNear { target: 0x3000 };
    sim.observe(br);
    sim.observe(insn(0x3000, 2));

    assert_eq!(sim.report().entries, 2);
}

#[test]
fn second_visit_to_a_known_entry_is_a_linked_transfer() {
    let mut sim = UnitSim::default();
    // Build unit A at 0x1000.
    sim.observe(insn(0x1000, 2));
    let mut term = insn(0x1002, 2);
    term.transfer = TransferKind::Indirect;
    term.is_terminator = true;
    sim.observe(term);

    // A second unit branches back into A's entry: that is a linked transfer, not a new entry.
    let mut br = insn(0x3000, 2);
    br.transfer = TransferKind::DirectNear { target: 0x1000 };
    sim.observe(br);
    sim.observe(insn(0x1000, 2));

    let r = sim.report();
    assert!(r.linked_transfers >= 1);
    assert_eq!(r.entries, 2);
}

#[test]
fn code_write_invalidates_owning_unit() {
    let mut sim = UnitSim::default();
    sim.observe(insn(0x1000, 2));
    sim.note_code_write(0x1001);
    assert_eq!(sim.report().sim_invalidations, 1);
}

#[test]
fn batch_end_closes_the_entry() {
    let mut sim = UnitSim::default();
    sim.observe(insn(0x1000, 2));
    sim.note_batch_end();
    sim.observe(insn(0x1002, 2));
    assert_eq!(sim.report().entries, 2);
}

#[test]
fn discontinuity_closes_the_entry_as_async_side_exit() {
    let mut sim = UnitSim::default();
    sim.observe(insn(0x1000, 2));
    sim.observe(insn(0x8000, 2));

    let r = sim.report();
    assert_eq!(r.entries, 2);
    assert_eq!(r.side_exits_async, 1);
}

#[test]
fn linked_transfer_quota_closes_the_entry() {
    let quota = crate::jit::direct::MAX_CHAIN_BLOCKS;
    let mut sim = UnitSim::default();

    // Build units A (0x1000) and B (0x2000) so both are known linked-transfer targets.
    for entry in [0x1000u32, 0x2000u32] {
        sim.observe(insn(entry, 2));
        let mut term = insn(entry + 2, 2);
        term.transfer = TransferKind::Indirect;
        term.is_terminator = true;
        sim.observe(term);
    }
    assert_eq!(sim.report().entries, 2);

    // Open a fresh entry that branches into A, then ping-pong A<->B inside the one entry.
    let mut open = insn(0x3000, 2);
    open.transfer = TransferKind::DirectNear { target: 0x1000 };
    sim.observe(open);
    assert_eq!(sim.report().entries, 3);

    // After the open we sit at A with the branch target 0x1000 pending. Each feed here consumes
    // one more quota slot. quota-1 feeds keep the entry open (transfers 2..=quota).
    let mut at_a = true;
    for _ in 0..(quota - 1) {
        let (here, there) = if at_a {
            (0x1000, 0x2000)
        } else {
            (0x2000, 0x1000)
        };
        let mut b = insn(here, 2);
        b.transfer = TransferKind::DirectNear { target: there };
        sim.observe(b);
        at_a = !at_a;
    }
    // Quota is now saturated but the entry has not yet been forced closed.
    assert_eq!(sim.report().entries, 3);
    assert_eq!(sim.report().linked_transfers, quota as u64);

    // The next branch exhausts the quota and closes the entry.
    let (here, there) = if at_a {
        (0x1000, 0x2000)
    } else {
        (0x2000, 0x1000)
    };
    let mut bind = insn(here, 2);
    bind.transfer = TransferKind::DirectNear { target: there };
    sim.observe(bind);
    assert_eq!(sim.report().entries, 3);

    // A fresh observation now opens a brand new entry.
    sim.observe(insn(0x4000, 2));
    assert_eq!(sim.report().entries, 4);
    assert_eq!(sim.report().linked_transfers, quota as u64);
}

#[test]
fn prefixed_instruction_continues_the_unit() {
    let mut sim = UnitSim::default();
    sim.observe(insn(0x1000, 2));
    // A wider in-window instruction standing in for a prefixed interpreter call-out.
    sim.observe(insn(0x1002, 3));
    sim.observe(insn(0x1005, 2));

    let r = sim.report();
    assert_eq!(r.entries, 1);
    assert_eq!(r.retired_in_units, 3);
}

#[test]
fn poll_loop_is_bounded_by_batch_ends() {
    let mut sim = UnitSim::default();
    for _ in 0..3 {
        sim.observe(insn(0x1000, 2));
        let mut back = insn(0x1002, 2);
        back.transfer = TransferKind::DirectNear { target: 0x1000 };
        sim.observe(back);
        sim.note_batch_end();
    }
    assert_eq!(sim.report().entries, 3);
}

#[test]
fn rich_kinds_map_to_indirect_under_default_config() {
    // A near CALL rel is classified `CallNear` by the hook, but the default config lowers it to
    // `Indirect`, so the entry closes as an unresolved exit exactly like a raw indirect.
    let mut sim = UnitSim::default();
    let mut call = insn(0x1000, 5);
    call.transfer = TransferKind::CallNear { target: 0x3000 };
    sim.observe(call);

    let r = sim.report();
    assert_eq!(r.entries, 1);
    assert_eq!(r.retired_in_units, 1);
    assert_eq!(r.unresolved_exits, 1);
    assert_eq!(r.linked_transfers, 0);
    assert_eq!(r.side_exits_io, 0);
    assert_eq!(r.side_exits_async, 0);
}

#[test]
fn return_maps_to_indirect_under_default_config() {
    // A near RET is `Return` at the classifier, `Indirect` under the default config.
    let mut sim = UnitSim::default();
    let mut ret = insn(0x1000, 1);
    ret.transfer = TransferKind::Return;
    sim.observe(ret);

    let r = sim.report();
    assert_eq!(r.entries, 1);
    assert_eq!(r.retired_in_units, 1);
    assert_eq!(r.unresolved_exits, 1);
}

#[test]
fn loop_near_maps_to_indirect_under_default_config() {
    // A LOOP back-edge: were `LoopNear` (mis)treated as `DirectNear`, the in-window target would
    // keep the entry open and `unresolved_exits` would stay 0. Under the default config it lowers
    // to `Indirect`, so the entry CLOSES unresolved. This pins that L0 never confuses the two.
    let mut sim = UnitSim::default();
    sim.observe(insn(0x1000, 2));
    let mut back = insn(0x1002, 2);
    back.transfer = TransferKind::LoopNear { target: 0x1000 };
    sim.observe(back);

    let r = sim.report();
    assert_eq!(r.entries, 1);
    assert_eq!(r.retired_in_units, 2);
    assert_eq!(r.unresolved_exits, 1);
}

#[test]
fn call_indirect_maps_to_indirect_under_default_config() {
    // A near indirect CALL is `CallIndirect` at the classifier, `Indirect` under the default
    // config: one unresolved exit, and nothing else moves.
    let mut sim = UnitSim::default();
    let mut call = insn(0x1000, 2);
    call.transfer = TransferKind::CallIndirect;
    sim.observe(call);

    let r = sim.report();
    assert_eq!(r.entries, 1);
    assert_eq!(r.retired_in_units, 1);
    assert_eq!(r.unresolved_exits, 1);
    assert_eq!(r.linked_transfers, 0);
    assert_eq!(r.side_exits_io, 0);
    assert_eq!(r.side_exits_async, 0);
}

#[test]
fn instruction_end_crossing_the_window_closes_growth() {
    let mut sim = UnitSim::default();
    // Root a unit in page 0x1.
    sim.observe(insn(0x1ffc, 2));
    // This instruction ends at 0x2002, crossing the 4 KiB window; it must open its own entry.
    sim.observe(insn(0x1ffe, 4));

    assert_eq!(sim.report().entries, 2);
}
