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
    sim.note_code_write(0x1001, 1);
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

// --- Task 2: mechanisms and the ladder ---

/// Build a resident unit at `entry` and close its batch (no exit counter charged), so `entry`
/// becomes a known link/return target without perturbing the exit counters.
fn build_known(sim: &mut UnitSim, entry: u32) {
    sim.observe(insn(entry, 2));
    sim.note_batch_end();
}

/// A near-CALL insn (`CallNear`) at `at` of length `len` targeting `target`.
fn call_near(at: u32, len: u8, target: u32) -> ObservedInsn {
    let mut i = insn(at, len);
    i.transfer = TransferKind::CallNear { target };
    i
}

/// A near indirect-CALL insn (`CallIndirect`) at `at` of length `len`.
fn call_indirect(at: u32, len: u8) -> ObservedInsn {
    let mut i = insn(at, len);
    i.transfer = TransferKind::CallIndirect;
    i
}

/// A near-RET insn (`Return`) at `at` of length `len`.
fn ret(at: u32, len: u8) -> ObservedInsn {
    let mut i = insn(at, len);
    i.transfer = TransferKind::Return;
    i
}

/// An `Indirect` insn at `at` of length `len`.
fn indirect(at: u32, len: u8) -> ObservedInsn {
    let mut i = insn(at, len);
    i.transfer = TransferKind::Indirect;
    i
}

/// A `LoopNear` insn at `at` of length `len` targeting `target`.
fn loop_near(at: u32, len: u8, target: u32) -> ObservedInsn {
    let mut i = insn(at, len);
    i.transfer = TransferKind::LoopNear { target };
    i
}

#[test]
fn l1_loop_back_edge_stays_open_and_links_nothing() {
    let mut sim = UnitSim::with_config(SimConfig::ladder(1));
    for _ in 0..3 {
        sim.observe(insn(0x1000, 2));
        sim.observe(loop_near(0x1002, 2, 0x1000));
    }
    let r = sim.report();
    assert_eq!(r.entries, 1);
    assert_eq!(r.retired_in_units, 6);
    assert_eq!(r.unresolved_exits, 0);
    assert_eq!(r.loop_links, 0);
}

#[test]
fn l1_out_of_window_loop_links() {
    let mut sim = UnitSim::with_config(SimConfig::ladder(1));
    build_known(&mut sim, 0x3000);
    let entries_before = sim.report().entries;

    sim.observe(insn(0x1000, 2));
    sim.observe(loop_near(0x1002, 2, 0x3000));

    let r = sim.report();
    assert_eq!(r.loop_links, 1);
    assert_eq!(r.linked_transfers, 0);
    // One fresh entry opened (the 0x1000 unit); the out-of-window link itself opened none.
    assert_eq!(r.entries, entries_before + 1);
}

#[test]
fn l2_call_links_to_known_unit_and_ret_links_back() {
    let mut sim = UnitSim::with_config(SimConfig::ladder(2));
    // Callee unit at 0x3000, entered once, ends Indirect.
    sim.observe(insn(0x3000, 2));
    sim.observe(indirect(0x3002, 2));
    // Make 0x1007 (the call's return address) a known unit entry, for the ret-link.
    build_known(&mut sim, 0x1007);
    let entries_before = sim.report().entries;

    // Caller: two insns then CallNear{0x3000} at 0x1002 len 5 (return address 0x1007).
    sim.observe(insn(0x1000, 2));
    sim.observe(call_near(0x1002, 5, 0x3000));
    // Callee body runs then returns.
    sim.observe(insn(0x3000, 2));
    sim.observe(ret(0x3002, 1));
    // The next observation lands on the return address, a known unit entry: ret-link.
    sim.observe(insn(0x1007, 2));

    let r = sim.report();
    assert_eq!(r.call_links, 1);
    assert_eq!(r.ret_links, 1);
    // Only the caller opened a fresh entry; call-link and ret-link opened none.
    assert_eq!(r.entries, entries_before + 1);
}

#[test]
fn l2_ret_mismatch_is_unresolved_not_async() {
    let mut sim = UnitSim::with_config(SimConfig::ladder(2));
    // Push 0x1007 with an indirect call, then a return arms a deferred check for it.
    sim.observe(call_indirect(0x1005, 2)); // pushes 0x1007, closes unresolved
    sim.observe(insn(0x3000, 2));
    sim.observe(ret(0x3002, 1)); // pops 0x1007, arms the deferred check
    let async_before = sim.report().side_exits_async;
    let unresolved_before = sim.report().unresolved_exits;

    // The next observation is not the popped address: the deferred check fails, unresolved.
    sim.observe(insn(0x2000, 2));

    let r = sim.report();
    assert_eq!(r.ret_links, 0);
    assert_eq!(r.unresolved_exits, unresolved_before + 1);
    assert_eq!(r.side_exits_async, async_before);
}

#[test]
fn l2_empty_stack_ret_is_immediately_unresolved() {
    let mut sim = UnitSim::with_config(SimConfig::ladder(2));
    sim.observe(ret(0x1000, 1));
    let r = sim.report();
    assert_eq!(r.ret_links, 0);
    assert_eq!(r.unresolved_exits, 1);
    assert_eq!(r.side_exits_async, 0);
}

#[test]
fn l2_batch_end_with_pending_ret_check_charges_unresolved() {
    let mut sim = UnitSim::with_config(SimConfig::ladder(2));
    build_known(&mut sim, 0x3000);
    // Call-link into the callee (no unresolved), pushing return address 0x1007.
    sim.observe(call_near(0x1002, 5, 0x3000));
    // A return inside the callee arms the deferred check.
    sim.observe(ret(0x3000, 1));
    let unresolved_before = sim.report().unresolved_exits;

    sim.note_batch_end();
    assert_eq!(sim.report().unresolved_exits, unresolved_before + 1);

    // With the batch ended, the target opens a fresh entry, not a ret-link.
    sim.observe(insn(0x1007, 2));
    assert_eq!(sim.report().ret_links, 0);
}

#[test]
fn l2_indirect_call_pushes_shadow_stack() {
    let mut sim = UnitSim::with_config(SimConfig::ladder(2));
    // The return address (call.next = 0x1002) must be a known unit for the ret-link.
    build_known(&mut sim, 0x1002);
    sim.observe(call_indirect(0x1000, 2)); // pushes 0x1002, closes unresolved (itc off)
    sim.observe(insn(0x2000, 2));
    sim.observe(ret(0x2002, 1)); // pops 0x1002, arms the deferred check
    sim.observe(insn(0x1002, 2)); // == call.next, a known unit: ret-link
    assert_eq!(sim.report().ret_links, 1);
}

#[test]
fn l3_tail_byte_write_restamps_instead_of_killing() {
    let mut sim = UnitSim::with_config(SimConfig::ladder(3));
    sim.observe(insn(0x1000, 3));
    sim.note_batch_end();

    // A write within the member's tail (not its first byte, inside its span) restamps.
    sim.note_code_write(0x1001, 2);
    assert_eq!(sim.report().sim_invalidations, 0);
    assert_eq!(sim.report().units_rebuilt, 0);

    // A second qualifying write before re-entry keeps one dirty mark.
    sim.note_code_write(0x1001, 2);

    // Re-entering counts exactly one restamp and clears the mark.
    sim.observe(insn(0x1000, 3));
    let r = sim.report();
    assert_eq!(r.sim_restamps, 1);
    assert_eq!(r.units_rebuilt, 0);
    assert_eq!(r.sim_invalidations, 0);
}

#[test]
fn l3_first_byte_write_kills() {
    let mut sim = UnitSim::with_config(SimConfig::ladder(3));
    sim.observe(insn(0x1000, 3));
    sim.note_batch_end();
    sim.note_code_write(0x1000, 1); // touches the member's first byte
    assert_eq!(sim.report().sim_invalidations, 1);
}

#[test]
fn l3_spilling_write_kills() {
    let mut sim = UnitSim::with_config(SimConfig::ladder(3));
    sim.observe(insn(0x1000, 3));
    sim.note_batch_end();
    sim.note_code_write(0x1002, 4); // starts in the tail but spills past the member end
    assert_eq!(sim.report().sim_invalidations, 1);
}

#[test]
fn l3_write_into_open_unit_closes_entry_but_unit_survives() {
    let mut sim = UnitSim::with_config(SimConfig::ladder(3));
    sim.observe(insn(0x1000, 3)); // entry stays open
    let unresolved_before = sim.report().unresolved_exits;

    sim.note_code_write(0x1001, 2); // qualifying tail write into the open unit
    let r = sim.report();
    assert_eq!(r.sim_invalidations, 0);
    assert_eq!(r.unresolved_exits, unresolved_before + 1);

    // The unit survived with a dirty mark: re-entry counts a restamp.
    sim.observe(insn(0x1000, 3));
    assert_eq!(sim.report().sim_restamps, 1);
}

#[test]
fn l3_non_member_write_in_owned_page_is_ignored() {
    let mut sim = UnitSim::with_config(SimConfig::ladder(3));
    sim.observe(insn(0x1000, 3));
    sim.note_batch_end();
    // A write in the owned page but outside every member span is ignored.
    sim.note_code_write(0x1100, 1);
    let r = sim.report();
    assert_eq!(r.sim_invalidations, 0);
    assert_eq!(r.sim_restamps, 0);

    sim.observe(insn(0x1000, 3));
    assert_eq!(sim.report().sim_restamps, 0);
}

#[test]
fn l0_still_kills_on_any_owned_page_write() {
    let mut sim = UnitSim::with_config(SimConfig::ladder(0));
    sim.observe(insn(0x1000, 3));
    sim.note_batch_end();
    // The same shape as the L3 "ignored" case, but L0 kills every owned-page hit.
    sim.note_code_write(0x1100, 1);
    assert_eq!(sim.report().sim_invalidations, 1);
}

#[test]
fn l0_page_crossing_write_kills_owners_of_both_pages() {
    let mut sim = UnitSim::with_config(SimConfig::ladder(0));
    // Unit A owns page 0x1, unit B owns page 0x2.
    build_known(&mut sim, 0x1000);
    build_known(&mut sim, 0x2000);
    // A write straddling the 0x1/0x2 boundary kills both.
    sim.note_code_write(0x1fff, 2);
    assert_eq!(sim.report().sim_invalidations, 2);
}

/// Run unit A (entry 0x1000) to its Indirect exit at site 0x1002, then observe `dest`.
fn run_a_indirect(sim: &mut UnitSim, dest: u32) {
    sim.observe(insn(0x1000, 2));
    sim.observe(indirect(0x1002, 2));
    sim.observe(insn(dest, 2));
}

#[test]
fn l4_itc_links_stable_indirect_target() {
    let mut sim = UnitSim::with_config(SimConfig::ladder(4));
    // Known targets B (0x3000) and C (0x5000).
    build_known(&mut sim, 0x3000);
    build_known(&mut sim, 0x5000);

    // Iter 1: first A->B. No cache entry: unresolved + fill (cache = B).
    run_a_indirect(&mut sim, 0x3000);
    assert_eq!(sim.report().itc_hits, 0);
    sim.note_batch_end();

    // Iter 2: second A->B. Cache hits: itc_hits == 1, and the switch opens no fresh entry.
    sim.observe(insn(0x1000, 2));
    sim.observe(indirect(0x1002, 2));
    let entries_at_exit = sim.report().entries;
    sim.observe(insn(0x3000, 2)); // ITC hit
    assert_eq!(sim.report().itc_hits, 1);
    assert_eq!(sim.report().entries, entries_at_exit);
    sim.note_batch_end();

    // Iter 3: A->C. Cache held B, misses, refills to C.
    run_a_indirect(&mut sim, 0x5000);
    assert_eq!(sim.report().itc_hits, 1);
    sim.note_batch_end();

    // Iter 4: A->B again. Cache held C, misses, refills to B.
    run_a_indirect(&mut sim, 0x3000);
    assert_eq!(sim.report().itc_hits, 1);
}

#[test]
fn ladder_feeds_five_sims_and_l0_matches_solo_v1() {
    enum Ev {
        Obs(ObservedInsn),
        Write(u32, u32),
        Batch,
    }
    // A mixed stream: a self-loop, a call/return pair, an indirect exit, code writes with width,
    // and batch ends. L0 lowers every rich kind to Indirect, so the ladder's L0 sim must match a
    // solo default sim byte for byte.
    let events = vec![
        Ev::Obs(insn(0x1000, 2)),
        Ev::Obs(loop_near(0x1002, 2, 0x1000)),
        Ev::Obs(insn(0x1000, 2)),
        Ev::Obs(loop_near(0x1002, 2, 0x1000)),
        Ev::Write(0x1001, 2),
        Ev::Batch,
        Ev::Obs(call_near(0x2000, 5, 0x1000)),
        Ev::Obs(insn(0x3000, 2)),
        Ev::Obs(ret(0x3002, 1)),
        Ev::Obs(insn(0x2005, 2)),
        Ev::Write(0x2fff, 4),
        Ev::Batch,
        Ev::Obs(call_indirect(0x4000, 2)),
        Ev::Obs(indirect(0x4002, 2)),
        Ev::Obs(insn(0x5000, 2)),
        Ev::Batch,
    ];

    let mut solo = UnitSim::default();
    let mut ladder = SimLadder::new();
    for ev in &events {
        match *ev {
            Ev::Obs(i) => {
                solo.observe(i);
                ladder.observe(i);
            }
            Ev::Write(p, w) => {
                solo.note_code_write(p, w);
                ladder.note_code_write(p, w);
            }
            Ev::Batch => {
                solo.note_batch_end();
                ladder.note_batch_end();
            }
        }
    }

    let reports = ladder.reports();
    assert_eq!(reports.len(), 5);
    assert_eq!(reports[0].0, "L0");
    assert_eq!(reports[0].1, solo.report());
    assert_eq!(reports[0].2, solo.unit_member_histogram());
}
