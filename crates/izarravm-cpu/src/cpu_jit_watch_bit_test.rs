// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The watched-page-bit coherence battery (`dev_docs/2026-08-06-watched-page-bit-design.md`,
//! tests T1/T2/T3/T6): the strict-edge sweeps, the alias rule, the skip-set rule and the lazy
//! teardown semantics, asserted at the fast-map entry level through the test inspectors.
//!
//! The other halves of the battery live where their machinery lives: the end-to-end watched
//! path (a marked page's native store taking the code-watch exit with the bit SET) is the
//! existing watched-store suites, whose fixtures now mark before they populate; the fast-path
//! skip differential (T4) sits beside the store-driving helpers in `cpu_jit_direct_test.rs`;
//! and the mid-block synchronicity obligation (T7) is discharged structurally — the E1 sweep
//! runs inside `fetch_decoded` itself, which is the ONLY decode insert path, callout or not,
//! so a callout that decodes fresh code has swept before it can return into its caller block.

use super::*;

/// The page under test, far from `ENTRY` so the fixture's own decode marks never touch it.
const PHYS: u32 = 0x3000;
/// A second linear mapping of the same physical page.
const ALIAS: u32 = 0x0051_3000;

fn watched_fixture() -> (CpuGsw, TestBus) {
    flat_fixture(ENTRY, &[0x90])
}

fn populate_target(cpu: &mut CpuGsw, bus: &mut TestBus, linear: u32) {
    let page = bus
        .direct_page(PHYS, BusAccessKind::DataWrite)
        .unwrap()
        .unwrap();
    let watched = cpu.physical_page_watched(PHYS);
    assert!(cpu.jit_fast_map.populate_write(
        linear,
        PHYS,
        page,
        jit::fast_map::PagePermissions::UNPAGED,
        watched,
    ));
}

/// T1: the sticky strict edge (E1). Deleting the sweep in `sweep_sticky_watch_edges`, the edge
/// return in `mark_chunk`, or the stash in `mark_code_range` fails the invalidation assert.
#[test]
fn a_sticky_mark_sweeps_bit_clear_entries_and_the_refill_carries_the_bit() {
    let (mut cpu, mut bus) = watched_fixture();
    populate_target(&mut cpu, &mut bus, PHYS);
    assert!(cpu.jit_fast_map.has_write_mapping(PHYS, PHYS));
    assert!(!cpu.jit_fast_map.page_watched_bit_for_test(PHYS));

    cpu.mark_decode_code_for_test(PHYS + 0x10, 1);
    assert!(
        !cpu.jit_fast_map.has_write_mapping(PHYS, PHYS),
        "the unwatched -> watched edge must invalidate the bit-clear entry (INV-W)"
    );

    populate_target(&mut cpu, &mut bus, PHYS);
    assert!(cpu.jit_fast_map.has_write_mapping(PHYS, PHYS));
    assert!(
        cpu.jit_fast_map.page_watched_bit_for_test(PHYS),
        "the refill must recompute the bit from the live watch state (H4)"
    );
    assert_eq!(cpu.code_watch_edge_counters().sweep_cleared_entries, 1);
}

/// T2: the block-watch strict edge (E2), through the same pending/sweep choke `install` and
/// `reject` use (the test hook routes through it — design H7).
#[test]
fn a_block_watch_acquire_sweeps_bit_clear_entries() {
    let (mut cpu, mut bus) = watched_fixture();
    populate_target(&mut cpu, &mut bus, PHYS);
    assert!(!cpu.jit_fast_map.page_watched_bit_for_test(PHYS));

    cpu.mark_block_code_for_test(PHYS + 0x20, 4);
    assert!(
        !cpu.jit_fast_map.has_write_mapping(PHYS, PHYS),
        "the block-watch edge must invalidate the bit-clear entry (INV-W)"
    );

    populate_target(&mut cpu, &mut bus, PHYS);
    assert!(
        cpu.jit_fast_map.page_watched_bit_for_test(PHYS),
        "the refill sees the block watch through physical_page_watched"
    );
    assert_eq!(cpu.code_watch_edge_counters().sweep_cleared_entries, 1);
}

/// T3: the sweep matches by PHYSICAL page, so every linear alias with a clear bit goes (H2).
#[test]
fn an_edge_sweeps_every_alias_of_the_physical_page() {
    let (mut cpu, mut bus) = watched_fixture();
    populate_target(&mut cpu, &mut bus, PHYS);
    populate_target(&mut cpu, &mut bus, ALIAS);
    assert!(cpu.jit_fast_map.has_write_mapping(ALIAS, PHYS));

    cpu.mark_decode_code_for_test(PHYS, 1);
    assert!(!cpu.jit_fast_map.has_write_mapping(PHYS, PHYS));
    assert!(
        !cpu.jit_fast_map.has_write_mapping(ALIAS, PHYS),
        "both aliases of the watched physical page must go"
    );
    assert_eq!(cpu.code_watch_edge_counters().sweep_cleared_entries, 2);
}

/// T6: the lazy edges (E4 here) leave bits STALE-SET with no sweep — the doom generation
/// pattern — and the next generation's strict edge SKIPS stale-set entries while clearing the
/// bit-clear ones filled during the unwatched window. The skip rule is what bounds the E1
/// sweep's cost at doom's churn; deleting it turns every generation into a full re-fill of the
/// code working set's entries.
#[test]
fn stale_set_bits_survive_the_lazy_edges_and_the_skip_rule_spares_them() {
    let (mut cpu, mut bus) = watched_fixture();
    // Generation 1: the page becomes watched, then the entry fills with the bit set.
    cpu.mark_decode_code_for_test(PHYS, 1);
    populate_target(&mut cpu, &mut bus, PHYS);
    assert!(cpu.jit_fast_map.page_watched_bit_for_test(PHYS));

    // E4: a sticky generation clear is LAZY — no sweep, the entry survives with the bit
    // stale-set (a set bit over empty tables just routes stores through the slow guard).
    cpu.decode_cache.native_code_watch.clear();
    assert!(cpu.jit_fast_map.has_write_mapping(PHYS, PHYS));
    assert!(cpu.jit_fast_map.page_watched_bit_for_test(PHYS));

    // An alias filled DURING the unwatched window carries a clear bit...
    populate_target(&mut cpu, &mut bus, ALIAS);
    assert!(!cpu.jit_fast_map.page_watched_bit_for_test(ALIAS));

    // ...and the next generation's re-mark is a fresh strict edge whose sweep clears exactly
    // that alias while the skip rule spares the stale-set entry.
    cpu.mark_decode_code_for_test(PHYS, 1);
    assert!(
        cpu.jit_fast_map.has_write_mapping(PHYS, PHYS),
        "stale-SET entry must be SKIPPED by the sweep"
    );
    assert!(
        !cpu.jit_fast_map.has_write_mapping(ALIAS, PHYS),
        "the bit-clear alias from the unwatched window must be swept"
    );
    assert_eq!(cpu.code_watch_edge_counters().sweep_cleared_entries, 1);
}

/// T2b (review gap 1): the PRODUCTION install path's edge extend, not the test hook's. The
/// bit-clear entry on the block's own page is constructed with an explicit `false` — production
/// cannot reach this state (decode marks the page before compile, so honest populates carry the
/// bit) — which is exactly why the E2 sweep is near-vacuous in production and why deleting the
/// `pending_watch_edges.extend` in `BlockCache::install` would otherwise survive the suite.
#[test]
fn a_real_install_records_the_edge_that_sweeps_a_bit_clear_entry() {
    let code = [0x40, 0x41, 0x42, 0xf4];
    let (mut cpu, mut bus) = flat_fixture(ENTRY, &code);
    warm(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );
    let compilation = compiled(jit::direct::compile(&mut cpu, ENTRY, true));

    // A bit-clear entry on the code page, planted between decode and install.
    let page = bus
        .direct_page(ENTRY, BusAccessKind::DataWrite)
        .unwrap()
        .unwrap();
    assert!(cpu.jit_fast_map.populate_write(
        ENTRY & !0xfff,
        ENTRY & !0xfff,
        page,
        jit::fast_map::PagePermissions::UNPAGED,
        false,
    ));

    // The production method (the extend at direct.rs's acquire site) plus the production sweep,
    // exactly as run.rs sequences them. The double probe is install's Seen precondition, and
    // probing acquires nothing (the review pinned that), so the planted entry survives to it.
    assert!(matches!(
        cpu.jit_direct.probe(compilation.span.key),
        jit::direct::BlockProbe::Interpret
    ));
    assert!(matches!(
        cpu.jit_direct.probe(compilation.span.key),
        jit::direct::BlockProbe::Compile
    ));
    assert!(cpu.jit_direct.install(&compilation).is_some());
    cpu.sweep_block_watch_edges();
    assert!(
        !cpu.jit_fast_map
            .has_write_mapping(ENTRY & !0xfff, ENTRY & !0xfff),
        "the install's E2 edge must reach the sweep (deleting install's extend leaves this live)"
    );
}

/// T7-lite (review gap 2): the decode-insert sweep is SYNCHRONOUS — `fetch_decoded` itself
/// drains the sticky edges it creates, which is the one placement the mid-block-callout
/// argument leans on. Deleting the sweep call in `decode.rs` leaves the edge parked and this
/// fails; the native-entry backstop must never become the only drain.
#[test]
fn fetch_decoded_sweeps_its_own_sticky_edges_before_returning() {
    let (mut cpu, mut bus) = watched_fixture();
    cpu.registers.eip = ENTRY;
    cpu.begin_instruction();
    cpu.fetch_decoded(&mut bus, ENTRY).expect("fixture decode");
    assert!(
        cpu.code_watch_edge_counters().sticky_page_edges >= 1,
        "the decode must have crossed the page's first edge"
    );
    assert!(
        cpu.decode_cache.native_code_watch.take_pending().is_empty(),
        "the edge must already be swept when fetch_decoded returns (mid-block callout contract)"
    );
}
