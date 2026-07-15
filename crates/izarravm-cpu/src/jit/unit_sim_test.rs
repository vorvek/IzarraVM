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
    assert_eq!(
        reports.len(),
        7,
        "SimLadder::new() fans out to the full L0..L6"
    );
    assert_eq!(reports[0].0, "L0");
    assert_eq!(reports[0].1, solo.report());
    assert_eq!(reports[0].2, solo.unit_member_histogram());
}

// --- Review pins: cache flush on kill, shadow-stack edges, quota via the new link kinds ---

#[test]
fn l4_unit_kill_flushes_its_itc_entries() {
    let mut sim = UnitSim::with_config(SimConfig::ladder(4));
    build_known(&mut sim, 0x3000); // known target B

    // First A->B fills the cache; second A->B hits.
    run_a_indirect(&mut sim, 0x3000);
    sim.note_batch_end();
    run_a_indirect(&mut sim, 0x3000);
    assert_eq!(sim.report().itc_hits, 1);
    sim.note_batch_end();

    // SMC-kill A (first-byte write to its entry member kills even under L3+).
    sim.note_code_write(0x1000, 1);
    assert_eq!(sim.report().sim_invalidations, 1);

    // The rebuilt A pays the first-encounter miss again: unresolved + refill, NOT a hit.
    run_a_indirect(&mut sim, 0x3000);
    assert_eq!(sim.report().itc_hits, 1);
    assert_eq!(sim.report().units_rebuilt, 1);
    sim.note_batch_end();

    // The refill re-arms the site: the next stable pass hits again.
    run_a_indirect(&mut sim, 0x3000);
    assert_eq!(sim.report().itc_hits, 2);
}

#[test]
fn l2_shadow_stack_drops_oldest_at_cap() {
    let mut sim = UnitSim::with_config(SimConfig::ladder(2));
    // 65 indirect calls, each pushing its return address (a known unit entry, own page).
    let ret_addr = |i: u32| 0x10000 + i * 0x1000;
    for i in 0..65 {
        build_known(&mut sim, ret_addr(i));
        sim.observe(call_indirect(ret_addr(i) - 2, 2)); // pushes ret_addr(i), closes unresolved
        sim.note_batch_end();
    }

    // 64 returns unwind the NEWEST 64 frames (LIFO: 64 down to 1); each links.
    for i in (1..65).rev() {
        sim.observe(ret(0x90000, 1));
        sim.observe(insn(ret_addr(i), 2));
        sim.note_batch_end();
    }
    assert_eq!(sim.report().ret_links, 64);

    // The 65th return finds an empty stack: frame 0 (the oldest) was dropped at the cap.
    let unresolved_before = sim.report().unresolved_exits;
    sim.observe(ret(0x90000, 1));
    assert_eq!(sim.report().ret_links, 64);
    assert_eq!(sim.report().unresolved_exits, unresolved_before + 1);
}

#[test]
fn l2_ret_mode_mismatch_at_resolution_is_unresolved() {
    let mut sim = UnitSim::with_config(SimConfig::ladder(2));
    build_known(&mut sim, 0x1002); // known in mode 0
    sim.observe(call_indirect(0x1000, 2)); // pushes 0x1002, closes unresolved
    sim.observe(ret(0x2000, 1)); // pops 0x1002, arms the deferred check
    let unresolved_before = sim.report().unresolved_exits;

    // The observed linear MATCHES the popped address, but the mode key differs: no ret-link.
    let mut wrong_mode = insn(0x1002, 2);
    wrong_mode.mode_key = 1;
    sim.observe(wrong_mode);

    let r = sim.report();
    assert_eq!(r.ret_links, 0);
    assert_eq!(r.unresolved_exits, unresolved_before + 1);
}

#[test]
fn l2_call_and_ret_links_consume_quota() {
    let quota = crate::jit::direct::MAX_CHAIN_BLOCKS;
    let mut sim = UnitSim::with_config(SimConfig::ladder(2));
    // K at 0x2000 self-calls; its return address 0x2005 is also a known unit.
    build_known(&mut sim, 0x2000);
    build_known(&mut sim, 0x2005);
    let entries_before = sim.report().entries;

    // quota-6 recursive self-calls, all inside one open entry (each links and burns one slot).
    sim.observe(call_near(0x2000, 5, 0x2000));
    for _ in 1..(quota - 6) {
        sim.observe(call_near(0x2000, 5, 0x2000));
    }
    assert_eq!(sim.report().call_links, (quota - 6) as u64);
    assert_eq!(sim.report().entries, entries_before + 1);

    // Six returns link back to 0x2005 (the shadow stack holds 64 copies), burning the rest.
    sim.observe(ret(0x2005, 1));
    sim.observe(insn(0x2005, 2)); // ret-link; accrues with fall-through 0x2007
    for _ in 1..6 {
        sim.observe(ret(0x2007, 1));
        sim.observe(insn(0x2005, 2));
    }
    assert_eq!(sim.report().ret_links, 6);
    assert_eq!(sim.report().entries, entries_before + 1);
    let unresolved_before = sim.report().unresolved_exits;

    // Quota is saturated: the next return's resolution closes unresolved and the target
    // reprocesses as a fresh entry.
    sim.observe(ret(0x2007, 1));
    sim.observe(insn(0x2005, 2));
    let r = sim.report();
    assert_eq!(r.ret_links, 6);
    assert_eq!(r.unresolved_exits, unresolved_before + 1);
    assert_eq!(r.entries, entries_before + 2);
}

#[test]
fn l1_loop_links_consume_quota() {
    let quota = crate::jit::direct::MAX_CHAIN_BLOCKS;
    let mut sim = UnitSim::with_config(SimConfig::ladder(1));
    build_known(&mut sim, 0x1000);
    build_known(&mut sim, 0x2000);
    let entries_before = sim.report().entries;

    // Open at 0x5000, then ping-pong A<->B via out-of-window LoopNear links until quota saturates.
    sim.observe(loop_near(0x5000, 2, 0x1000));
    let mut at_a = true;
    for _ in 1..quota {
        let (here, there) = if at_a {
            (0x1000, 0x2000)
        } else {
            (0x2000, 0x1000)
        };
        sim.observe(loop_near(here, 2, there));
        at_a = !at_a;
    }
    assert_eq!(sim.report().loop_links, quota as u64);
    assert_eq!(sim.report().entries, entries_before + 1);

    // The next out-of-window loop link exhausts the quota and closes the entry unresolved.
    let (here, there) = if at_a {
        (0x1000, 0x2000)
    } else {
        (0x2000, 0x1000)
    };
    let unresolved_before = sim.report().unresolved_exits;
    sim.observe(loop_near(here, 2, there));
    assert_eq!(sim.report().loop_links, quota as u64);
    assert_eq!(sim.report().unresolved_exits, unresolved_before + 1);
}

#[test]
fn l2_ret_to_unknown_entry_is_unresolved() {
    let mut sim = UnitSim::with_config(SimConfig::ladder(2));
    // 0x1002 is pushed but never made a unit entry.
    sim.observe(call_indirect(0x1000, 2));
    sim.observe(ret(0x2000, 1)); // pops 0x1002, arms the deferred check
    let unresolved_before = sim.report().unresolved_exits;

    // The observed linear matches the popped address, but it is not a known unit entry.
    sim.observe(insn(0x1002, 2));
    let r = sim.report();
    assert_eq!(r.ret_links, 0);
    assert_eq!(r.unresolved_exits, unresolved_before + 1);
}

#[test]
fn l2_failed_call_link_still_pushes_shadow_stack() {
    let mut sim = UnitSim::with_config(SimConfig::ladder(2));
    build_known(&mut sim, 0x1007); // the call's return address, known for the later ret-link

    // The call's TARGET is unknown, so the link fails (unresolved close), but the push happened.
    sim.observe(call_near(0x1002, 5, 0xdea_d000));
    assert_eq!(sim.report().call_links, 0);

    // A later return still links back to call.next via the pushed frame.
    sim.observe(ret(0x3000, 1));
    sim.observe(insn(0x1007, 2));
    assert_eq!(sim.report().ret_links, 1);
}

// --- C-pre-3: L5 global hashed target lookup and L6 io call-outs ---

#[test]
fn ght_hash_pins_qemu_shape() {
    // The index is a function of the LOW address bits only. Deriving from the pinned formula
    // (index depends on tmp bits {0..5, 12..17}, i.e. linear bits 0..23), two linears differing
    // ONLY above bit 23 collide, while adjacent same-page linears map to distinct slots. This pins
    // the shape of QEMU's tb_jmp_cache_hash_func. (The addendum's "above bit 17" is imprecise: the
    // formula's masks make bits 18..23 matter; bit 24 and up never change the slot.)
    assert_eq!(ght_index(0x2000), ght_index(0x2000 + (1 << 24)));
    assert_eq!(ght_index(0x2000), ght_index(0x2000 + (1 << 27)));
    // Adjacent same-page linears (2-byte insns) do not collide with each other.
    assert_ne!(ght_index(0x1000), ght_index(0x1002));
    assert_ne!(ght_index(0x1002), ght_index(0x1004));
    assert_ne!(ght_index(0x1000), ght_index(0x1004));
    // Every slot is in range.
    for lin in [0u32, 0x1000, 0xdead_beef, 0xffff_ffff] {
        assert!(ght_index(lin) < 4096);
    }
}

#[test]
fn l5_indirect_links_via_global_table() {
    // A ends Indirect to B twice. The first resolution finds B unknown: it misses (unresolved),
    // and reprocessing B opens+installs it. The second resolution hits the table (ght_hits == 1)
    // and the switch opens no fresh entry.
    let mut sim = UnitSim::with_config(SimConfig::ladder(5));

    sim.observe(insn(0x1000, 2));
    sim.observe(indirect(0x1002, 2));
    sim.observe(insn(0x3000, 2)); // B unknown: miss, B built + installed here
    assert_eq!(sim.report().ght_hits, 0);
    sim.note_batch_end();

    sim.observe(insn(0x1000, 2));
    sim.observe(indirect(0x1002, 2));
    let entries_at_exit = sim.report().entries;
    sim.observe(insn(0x3000, 2)); // B resident in the table: hit
    assert_eq!(sim.report().ght_hits, 1);
    assert_eq!(
        sim.report().entries,
        entries_at_exit,
        "the table hit switches without opening a fresh entry"
    );
}

#[test]
fn l5_empty_stack_ret_arms_ght_and_links() {
    // A Return with an EMPTY shadow stack arms a Ght check at L5 and resolves via the table when the
    // slot holds the (known) return target. Attributed by origin: ght_ret_hits, not ght_hits.
    let mut sim = UnitSim::with_config(SimConfig::ladder(5));
    build_known(&mut sim, 0x5000); // T known + installed at entry-open; shadow stays empty

    sim.observe(insn(0x2000, 2)); // open a fresh entry
    sim.observe(ret(0x2002, 1)); // empty stack -> arm Ght{from_return}
    sim.observe(insn(0x5000, 2)); // T is table-resident: return resolves via the table

    let r = sim.report();
    assert_eq!(r.ght_ret_hits, 1);
    assert_eq!(r.ght_hits, 0);
    assert_eq!(r.ret_links, 0);
}

#[test]
fn l5_ret_stage2_fallback() {
    // A non-empty shadow stack holds a WRONG address; the observed target is a different, known,
    // table-resident unit. Stage 1 (shadow compare) fails; stage 2 (the table) links the return.
    let mut sim = UnitSim::with_config(SimConfig::ladder(5));
    build_known(&mut sim, 0x5000); // T known + installed

    // Push a wrong return address (0x1002) via an indirect call, then end the batch (the pending
    // Ght check charges unresolved and closes; the shadow frame survives).
    sim.observe(call_indirect(0x1000, 2));
    sim.note_batch_end();

    sim.observe(insn(0x2000, 2)); // fresh entry
    sim.observe(ret(0x2002, 1)); // pops 0x1002 -> Deferred::Return{0x1002}
    let ret_links_before = sim.report().ret_links;
    sim.observe(insn(0x5000, 2)); // T != 0x1002: stage 1 fails, stage 2 table hit

    let r = sim.report();
    assert_eq!(
        r.ret_links, ret_links_before,
        "the shadow compare did not link"
    );
    assert_eq!(r.ght_ret_hits, 1, "the table resolved the return");
    assert_eq!(r.ght_hits, 0);
}

#[test]
fn l5_linked_switch_does_not_install() {
    // Linked switches (direct/call/loop chaining) must NOT install into the global table: only
    // entry-open, deferred resolution, and shadow-ret-link success do. Construction: B is built once
    // (entry-open installs it), then its slot is evicted by a colliding install, then B is reached
    // ONLY via a CallNear linked switch. A subsequent Indirect to B must MISS, proving the linked
    // switch did not reinstall B. X = B + (1 << 24) collides with B (the hash ignores bits >= 24).
    const B: u32 = 0x2000;
    const X: u32 = 0x2000 + (1 << 24);
    assert_eq!(ght_index(B), ght_index(X), "B and X must collide");

    let mut sim = UnitSim::with_config(SimConfig::ladder(5));
    build_known(&mut sim, B); // entry-open installs B in its slot
    build_known(&mut sim, X); // colliding install evicts B's slot (now holds X)

    // Reach B ONLY via a CallNear linked switch (call-link, no install).
    sim.observe(insn(0x8000, 2));
    sim.observe(call_near(0x8002, 5, B));
    assert!(sim.report().call_links >= 1, "the caller must link to B");
    sim.note_batch_end();

    // Indirect to B: B is a live unit, but its slot still holds X, so the table probe misses.
    sim.observe(insn(0x9000, 2));
    sim.observe(indirect(0x9002, 2));
    let ght_hits_before = sim.report().ght_hits;
    let unresolved_before = sim.report().unresolved_exits;
    sim.observe(insn(B, 2));

    let r = sim.report();
    assert_eq!(
        r.ght_hits, ght_hits_before,
        "the linked switch must not have reinstalled B"
    );
    assert_eq!(r.unresolved_exits, unresolved_before + 1);
}

#[test]
fn l5_collision_evicts() {
    // Two units whose entries hash to the same slot; alternating indirect transfers between them
    // thrash the direct-mapped table: each landing's resolution-install evicts the other, so the
    // next (opposite) target always misses. P and Q = P + (1 << 24) collide.
    const P: u32 = 0x2000;
    const Q: u32 = 0x2000 + (1 << 24);
    assert_eq!(ght_index(P), ght_index(Q), "P and Q must collide");

    let mut sim = UnitSim::with_config(SimConfig::ladder(5));
    build_known(&mut sim, P);
    build_known(&mut sim, Q); // colliding: the slot now holds Q, evicting P

    for &t in &[P, Q, P, Q, P, Q] {
        sim.observe(insn(0x9000, 2));
        sim.observe(indirect(0x9002, 2));
        sim.observe(insn(t, 2)); // the slot holds the OTHER unit -> miss
        sim.note_batch_end();
    }
    assert_eq!(
        sim.report().ght_hits,
        0,
        "direct-mapped collisions must thrash, never hit"
    );
}

#[test]
fn l6_io_keeps_entry_open() {
    // A poll loop with an IN instruction (touches_io) plus a back-edge. At L6 the IN accrues as an
    // in-unit call-out: io_callouts counts each IN, the single entry stays open, side_exits_io == 0.
    let mut sim = UnitSim::with_config(SimConfig::ladder(6));
    sim.observe(insn(0x1000, 2)); // open once
    for _ in 0..5 {
        let mut io = insn(0x1002, 2);
        io.touches_io = true;
        io.transfer = TransferKind::DirectNear { target: 0x1000 }; // in-window back-edge
        sim.observe(io);
        sim.observe(insn(0x1000, 2)); // continues via the recorded direct target
    }
    let r = sim.report();
    assert_eq!(r.entries, 1, "the io call-out keeps the single entry open");
    assert_eq!(r.io_callouts, 5);
    assert_eq!(r.side_exits_io, 0);
}

#[test]
fn l6_terminator_with_io_still_closes() {
    // An instruction that is BOTH a terminator and touches io: the terminator check precedes the io
    // rule, so it closes unresolved and never counts an io call-out.
    let mut sim = UnitSim::with_config(SimConfig::ladder(6));
    let mut i = insn(0x1000, 2);
    i.is_terminator = true;
    i.touches_io = true;
    sim.observe(i);

    let r = sim.report();
    assert_eq!(r.entries, 1);
    assert_eq!(r.unresolved_exits, 1);
    assert_eq!(r.io_callouts, 0);
    assert_eq!(r.side_exits_io, 0);
}

#[test]
fn l6_pending_deferred_resolves_on_io_insn() {
    // A Return arms a deferred check; the resolving observation touches io. The deferred resolves
    // normally (a shadow ret-link here), and the io instruction accrues into the switched unit,
    // incrementing io_callouts without closing.
    let mut sim = UnitSim::with_config(SimConfig::ladder(6));
    build_known(&mut sim, 0x1007); // return target T, known

    sim.observe(call_indirect(0x1005, 2)); // pushes 0x1007, arms a Ght check
    sim.note_batch_end(); // clear the pending check; shadow keeps 0x1007

    sim.observe(insn(0x2000, 2)); // open a fresh entry
    sim.observe(ret(0x2002, 1)); // pops 0x1007 -> Deferred::Return{0x1007}

    let mut io = insn(0x1007, 2); // the resolving observation touches io
    io.touches_io = true;
    sim.observe(io);

    let r = sim.report();
    assert_eq!(r.ret_links, 1, "the shadow compare linked the return");
    assert_eq!(r.io_callouts, 1, "the io insn accrued to the switched unit");
    assert_eq!(
        r.side_exits_io, 0,
        "the io call-out did not close the entry"
    );
}

#[test]
fn with_rungs_selects_and_labels() {
    let ladder = SimLadder::with_rungs(&[0, 4, 5, 6]);
    let reports = ladder.reports();
    assert_eq!(reports.len(), 4);
    assert_eq!(reports[0].0, "L0");
    assert_eq!(reports[1].0, "L4");
    assert_eq!(reports[2].0, "L5");
    assert_eq!(reports[3].0, "L6");
}

#[test]
fn ladder_first_five_reports_unchanged() {
    // The L0-L4 bit-identity guarantee: an identical mixed stream fed to five solo ladder(0..=4)
    // configs and to the full L0..L6 SimLadder produces field-for-field equal reports on the first
    // five rungs, with all new counters zero.
    enum Ev {
        Obs(ObservedInsn),
        Write(u32, u32),
        Batch,
    }
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

    let mut solos: Vec<UnitSim> = (0u8..=4)
        .map(|r| UnitSim::with_config(SimConfig::ladder(r)))
        .collect();
    let mut ladder = SimLadder::new();
    for ev in &events {
        match *ev {
            Ev::Obs(i) => {
                for s in &mut solos {
                    s.observe(i);
                }
                ladder.observe(i);
            }
            Ev::Write(p, w) => {
                for s in &mut solos {
                    s.note_code_write(p, w);
                }
                ladder.note_code_write(p, w);
            }
            Ev::Batch => {
                for s in &mut solos {
                    s.note_batch_end();
                }
                ladder.note_batch_end();
            }
        }
    }

    let reports = ladder.reports();
    assert_eq!(reports.len(), 7);
    for (r, solo) in solos.iter().enumerate() {
        assert_eq!(reports[r].1, solo.report(), "rung L{r} report changed");
        assert_eq!(reports[r].2, solo.unit_member_histogram());
        assert_eq!(reports[r].1.ght_hits, 0, "L{r} ght_hits must be zero");
        assert_eq!(
            reports[r].1.ght_ret_hits, 0,
            "L{r} ght_ret_hits must be zero"
        );
        assert_eq!(reports[r].1.io_callouts, 0, "L{r} io_callouts must be zero");
    }
}
