// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The FastMap slot-reject census (`IZARRAVM_SLOT_CENSUS=1`).
//!
//! The slow-read page histogram (`cpu_slow_read_histo_test.rs`) answers "WHICH PAGES do the slow
//! reads come from"; this instrument answers "WHICH CLAUSE refused them". They are complementary
//! and deliberately independent: the histogram is page-granular, boxed and default-off with a
//! `Box` behind it; this one is reason-granular, always-compiled and gated on a bare `CpuGsw`
//! byte. The cross-check that ties them together is `slot_reject_misaligned` against
//! `data_slow_reads` -- NOT against `interp_fast_map_misses`, because
//! `call_out_stack_frame_resident` calls `lookup_access` directly and deliberately bypasses the
//! probe counters.
//!
//! What these tests pin:
//!
//! 1. **It is off unless armed.** An unarmed run must leave every reject counter at zero while
//!    the refusals themselves still happen, and `slot_reject_enabled` must report the LIVE gate,
//!    so five zeroes can never be read as "nothing was refused".
//! 2. **Each reason lands in its own bucket**, against a FastMap that is genuinely populated --
//!    a census run against an empty map would attribute everything to `Absent` and would pass
//!    under any classifier at all. That is the fixtures-that-cannot-fail shape this file has to
//!    avoid, so every case below first proves the SAME page serves an aligned access.

use super::*;

/// A CPU with the FastMap armed and a bus that serves plain RAM directly, plus one warm-up
/// access that populates the map. Returns the linear base of the populated page.
fn cpu_with_populated_page() -> (CpuGsw, TestBus, u32) {
    let mut cpu = CpuGsw::default();
    cpu.jit_direct.set_fast_map_enabled_for_test(true);
    cpu.refresh_fast_map_serve_gate();
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    // Roomy enough that every case below can use a page of its OWN: a second access to a
    // page a previous case populated would HIT, and the refusal under test would never happen.
    let mut bus = TestBus::with_memory(vec![0; 0x2_0000]);
    bus.direct_pages_enabled = true;

    // Populate: an ALIGNED word read on page 4 goes through the DirectPageCache and publishes a
    // read bias. Without this every case below would be `Absent` and would prove nothing.
    let base = 0x4000u32;
    cpu.read_memory_sized(
        &mut bus,
        SegmentIndex::Ds,
        base,
        OperandSize::Word,
        BusAccessKind::DataRead,
    )
    .unwrap();
    // Prove the map really serves this page now, so a later refusal is attributable to the
    // clause under test rather than to a cold map.
    let before = cpu.fast_map_probe_counters().hits;
    cpu.read_memory_sized(
        &mut bus,
        SegmentIndex::Ds,
        base + 2,
        OperandSize::Word,
        BusAccessKind::DataRead,
    )
    .unwrap();
    assert_eq!(
        cpu.fast_map_probe_counters().hits,
        before + 1,
        "the warm-up did not actually populate the FastMap; every case below would be Absent"
    );
    (cpu, bus, base)
}

fn read_word(cpu: &mut CpuGsw, bus: &mut TestBus, offset: u32) {
    cpu.read_memory_sized(
        bus,
        SegmentIndex::Ds,
        offset,
        OperandSize::Word,
        BusAccessKind::DataRead,
    )
    .unwrap();
}

#[test]
fn the_census_is_silent_until_it_is_armed() {
    let (mut cpu, mut bus, base) = cpu_with_populated_page();

    // A read on a page nothing has populated: refused, and the refusal is an `Absent`. (It used
    // to be a misaligned read on a live page; that access is SERVED now, so it is no longer a
    // refusal at all and cannot exercise the gate.)
    let _ = base;
    read_word(&mut cpu, &mut bus, 0x9000);
    let audit = cpu.fast_map_audit_counters();
    assert_eq!(audit.slot_reject_absent, 0);
    assert!(!audit.slot_reject_enabled);
    // The refusal DID happen -- this is what stops the assertion above from being vacuous.
    let misses_unarmed = cpu.fast_map_probe_counters().misses;
    assert!(misses_unarmed > 0);

    cpu.set_slot_census_enabled(true);
    read_word(&mut cpu, &mut bus, 0xb000);
    let audit = cpu.fast_map_audit_counters();
    assert_eq!(audit.slot_reject_absent, 1, "only the armed access counts");
    assert!(audit.slot_reject_enabled);

    cpu.set_slot_census_enabled(false);
    read_word(&mut cpu, &mut bus, 0xd000);
    let audit = cpu.fast_map_audit_counters();
    assert_eq!(audit.slot_reject_absent, 1);
    assert!(!audit.slot_reject_enabled);
}

#[test]
fn each_refusal_reason_lands_in_its_own_bucket() {
    let (mut cpu, mut bus, base) = cpu_with_populated_page();
    cpu.set_slot_census_enabled(true);

    // Misaligned, page-local, on a live page with a current epoch: SERVED now, so it must land in
    // `slot_admit_misaligned` and in no reject bucket at all.
    read_word(&mut cpu, &mut bus, base + 1);
    // Page-crossing. This one splits upstream into two byte fragments, each of which probes on
    // its own page, so the crossing rejection is counted by the probe that sees the whole width.
    read_word(&mut cpu, &mut bus, base + 0xfff);
    // A page that was never populated: the ordinary cold miss.
    read_word(&mut cpu, &mut bus, 0x7000);

    let audit = cpu.fast_map_audit_counters();
    assert_eq!(audit.slot_admit_misaligned, 1);
    assert_eq!(
        audit.slot_reject_misaligned, 0,
        "the alignment rung is gone; nothing may be attributed to it"
    );
    assert_eq!(audit.slot_reject_page_cross, 1);
    assert!(
        audit.slot_reject_absent >= 1,
        "the unpopulated page must be Absent, not Misaligned or Epoch"
    );
    // Nothing has invalidated the mapping epoch and no permission has moved, so those two buckets
    // must stay empty. A classifier that fell through to a single catch-all would light them up.
    assert_eq!(audit.slot_reject_epoch, 0);
    assert_eq!(audit.slot_reject_permission, 0);
}

#[test]
fn a_stale_mapping_epoch_is_attributed_to_the_epoch_bucket() {
    let (mut cpu, mut bus, base) = cpu_with_populated_page();
    cpu.set_slot_census_enabled(true);

    // Advance the BUS's mapping epoch without touching the CPU's FastMap, which is exactly the
    // shape `slot_reject_epoch` exists to size: a live entry whose epoch no longer matches. In
    // production the epoch rides in on `DirectPage`, so this is the bus-side half of a re-map
    // arriving before anything has wiped the map.
    bus.direct_mapping_epoch += 1;
    // A read on a DIFFERENT page pulls the new epoch into `data_read_pages` via `insert`, which
    // is what makes the surviving entry on `base`'s page stale rather than merely unmatched.
    // It is itself a refusal, so it is taken BEFORE the counters are sampled.
    read_word(&mut cpu, &mut bus, 0x5000);
    let before = cpu.fast_map_audit_counters();

    read_word(&mut cpu, &mut bus, base + 2);
    let audit = cpu.fast_map_audit_counters();
    assert_eq!(
        (
            audit.slot_reject_epoch - before.slot_reject_epoch,
            audit.slot_reject_absent - before.slot_reject_absent
        ),
        (1, 0),
        "an aligned access to a live-but-stale page is an Epoch refusal"
    );
}
