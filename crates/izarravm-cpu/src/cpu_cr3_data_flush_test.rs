// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The CR3 DATA-side gate, T1+T2 (`dev_docs/2026-09-02-cr3-data-side-design.md`, amended by
//! `dev_docs/2026-09-02-cr3-code-cache-gate-review.md`'s "Review of the data-side design"
//! section, findings D1 through D5).
//!
//! T1: `data_read_pages`/`data_write_pages` are physical-page-and-bus-epoch keyed with no CR3
//! dependence at all, so they are removed from the `MOV CR3` wipe unconditionally -- and, as a
//! consequence of the shared helper T1 deletes from, from the CR0/task-switch full flush and the
//! `flush_tlb_keep_code_caches` PG=1 arm too.
//!
//! T2: the TLB grows a two-slot generation (`Tlb::generations`) mirroring `DecodeCache`'s ring
//! and the JIT link graph's `link_epochs`. `select_generation`/`allocate_generation` mirror
//! `select_link_context`/`allocate_link_context`; `retire_all_slots` takes the `RingRetired`
//! token every `retire_ring` call now returns, so a site that forgets to retire both generations
//! is a `-D warnings` build failure (review D2), not a discipline; `flush_live_slot` is the one
//! caller (`flush_tlb_keep_code_caches`'s PG=1 arm) that does NOT renumber the ring and so may
//! only touch the currently live slot (review D4); `retire_dormant_slot` is INVLPG's second
//! obligation, since `Tlb::invalidate` clears only the live generation's entry.
//!
//! Fixtures live in `super` (`cpu_cr3_flush_test.rs`, this file's parent): `cr3_fixture`,
//! `write_cr3`, `warm_witness`, `PAGE_DIRECTORY` (directory A, `0x8000`), `DIR_B` (`0xA000`,
//! sharing A's page table so a translation is identical either way), `DIR_C` (`0xC000`, a
//! genuinely different mapping via `ALT_TABLE`/`ALT_FRAME`), `WITNESS`/`WRITER`.

use super::*;

/// A DATA address distinct from `WRITER` (page 0), `WITNESS` (page 0) and `WITNESS_B` (page 0):
/// page 3, still covered by `plant_identity_paging`'s low 16 pages, so it participates in the
/// same identity mapping and the same DIR_B/DIR_C table setup without touching the decode-line
/// fixtures' own slot.
const TLB_DATA: u32 = 0x3000;

/// `cr3_fixture` plus the bus flags `DirectPageCache` needs to actually populate
/// (`data_fixture` in `cpu_cr0_flush_test.rs`'s `data_caches` module sets the same two).
fn data_flush_fixture() -> (CpuGsw, TestBus) {
    let (cpu, mut bus) = cr3_fixture();
    bus.direct_pages_enabled = true;
    bus.direct_pages_writable = true;
    (cpu, bus)
}

/// Touch BOTH `WRITER`'s linear page (0) and `TLB_DATA`'s (3) together, in one call, before
/// anything else in a fixture that will call `write_cr3`. This is NOT merely about which of the
/// two is walked first -- marking is PAGE-granular over the WHOLE shared table (`0x9000`), so
/// the SECOND distinct page ever walked through it retires the ring no matter which one goes
/// first (the first walk's own accessed-bit store always lands before that walk's own mark; the
/// second walk's store always lands after the first walk's mark). One retire is therefore
/// unavoidable the first time two different pages sharing this table are both touched, and this
/// helper is where it happens: called before ANY `write_cr3`, the ring is not yet seeded, so a
/// mid-call retire disrupts nothing. Calling `write_cr3` BEFORE this helper, instead of after,
/// is the mistake this file's tests must avoid -- `cpu_cr3_flush_test.rs`'s own doc comment on
/// `pte_edit_with_a_tlb_warm_target_still_retires` explains the same mechanism from the
/// decode-line side.
fn warm_writer_and_tlb_data(cpu: &mut CpuGsw, bus: &mut TestBus) {
    cpu.read_memory_u8(bus, SegmentIndex::Ds, WRITER, BusAccessKind::DataRead)
        .expect("priming the WRITER page must not fault");
    bus.memory[TLB_DATA as usize] = 0x5a;
    assert_eq!(
        cpu.read_memory_u8(bus, SegmentIndex::Ds, TLB_DATA, BusAccessKind::DataRead)
            .expect("the byte reads"),
        0x5a
    );
}

// ---- T1: data_read_pages / data_write_pages are out of the CR3 path, unconditionally ----------

/// R-D1. Paging on, `data_read_pages` warmed, then a `MOV CR3` to a value the ring already owns
/// (R1: `write_cr3` to the SAME directory `cr3_fixture` already set). Red before T1 (the old
/// unconditional wipe emptied the cache on every write); green after.
///
/// `direct_page_hits` counts BUS REFILLS on a miss, not cache hits (review D7) -- a HIT bumps
/// `direct_data_pointer_reads` instead -- so "does not move on the next read of the same
/// physical page" is the correct assertion for "the entry was served from cache, no refill ran".
#[test]
fn r_d1_direct_page_cache_survives_a_same_directory_reselect() {
    let (mut cpu, mut bus) = data_flush_fixture();
    warm_writer_and_tlb_data(&mut cpu, &mut bus);
    assert!(
        cpu.data_read_pages.get(TLB_DATA).is_some(),
        "the fixture must warm the cache, or the row proves nothing"
    );

    // The write under test: an R1 reselect of the directory the ring already owns.
    write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);

    assert!(
        cpu.data_read_pages.get(TLB_DATA).is_some(),
        "T1: a physical-keyed cache with no CR3 dependence must survive ANY CR3 write"
    );
    // `direct_page_hits` is shared with a SEPARATE consumer, the code-fetch direct-page probe
    // (`decode.rs:1221`): `write_cr3` itself fetches the `MOV CR3` instruction at WRITER, and
    // `invalidate_fetch_frontend` drops the fetch-page cache on EVERY CR3 write, unconditionally,
    // by design -- so the write above always costs exactly one refill on THAT cache, unrelated
    // to `data_read_pages` and to T1. Captured AFTER the write, `hits_before` already includes
    // that one-time cost, so the comparison below isolates the READBACK alone, which touches no
    // fetch frontend.
    let hits_before = cpu.perf_counters().direct_page_hits;
    assert_eq!(
        cpu.read_memory_u8(
            &mut bus,
            SegmentIndex::Ds,
            TLB_DATA,
            BusAccessKind::DataRead
        )
        .expect("the byte reads back"),
        0x5a
    );
    assert_eq!(
        cpu.perf_counters().direct_page_hits,
        hits_before,
        "no refill happened: the read was served from the retained entry"
    );
}

/// The `Taken` arm (a third distinct directory) is the strongest ring event there is -- it fully
/// retires the decode ring and the TLB -- and T1's claim is that even THIS never touches the
/// physical-page caches, because they have no CR3 dependence to protect.
#[test]
fn r_d1_direct_page_cache_survives_the_taken_arm_too() {
    let (mut cpu, mut bus) = data_flush_fixture();
    warm_witness(&mut cpu, &mut bus);
    warm_writer_and_tlb_data(&mut cpu, &mut bus);
    write_cr3(&mut cpu, &mut bus, DIR_B);

    write_cr3(&mut cpu, &mut bus, DIR_C); // R3: both slots occupied, a third value.

    assert!(
        cpu.data_read_pages.get(TLB_DATA).is_some(),
        "T1: even the full-teardown Taken arm must not touch the physical-page cache"
    );
}

/// T1b (design slice list item 2), the ONE of the three bus causes not already covered by an
/// existing row: `note_direct_map_changed` has
/// `cpu_test.rs::a_bus_decode_change_still_drops_ram_and_vga_direct_pages`, and
/// `note_direct_data_map_changed` has
/// `cpu_test.rs::a_vga_aperture_change_drops_the_aperture_and_keeps_ram_live_at_the_same_epoch`.
/// Neither of those exercises A20. Anti-regression, not new behaviour: A20 never touched CR3.
#[test]
fn t1b_a20_toggle_still_drops_the_direct_page_cache() {
    let (mut cpu, mut bus) = data_flush_fixture();
    warm_writer_and_tlb_data(&mut cpu, &mut bus);
    assert!(cpu.data_read_pages.get(TLB_DATA).is_some());

    cpu.note_a20_changed();

    assert!(
        cpu.data_read_pages.get(TLB_DATA).is_none(),
        "the A20 bus cause must still invalidate the physical-page cache"
    );
}

// ---- T2: the TLB's two-slot generation ----------------------------------------------------

/// R-D2, fixed per review D5 (the counter-proxy version cannot fail: `translation_a_stores` /
/// `_d_stores` only move INSIDE a store guard, and this row's own warm-up already cleared it).
/// Direct assertion instead: the entry itself, through `Tlb::lookup`, plus the new `tlb_walks`
/// counter (design T2 item D5) as the non-proxy "did a walk run" check.
#[test]
fn r_d2_tlb_entry_survives_a_directory_round_trip() {
    let (mut cpu, mut bus) = cr3_fixture();
    warm_writer_and_tlb_data(&mut cpu, &mut bus);
    assert!(
        cpu.tlb.lookup(TLB_DATA >> 12).is_some(),
        "the fixture must warm the TLB, or the row proves nothing"
    );

    write_cr3(&mut cpu, &mut bus, DIR_B);
    write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);

    assert!(
        cpu.tlb.lookup(TLB_DATA >> 12).is_some(),
        "T2: returning to A must restore its TLB entry, not require a re-walk"
    );
    let walks_before = cpu.perf_counters().tlb_walks;
    cpu.read_memory_u8(
        &mut bus,
        SegmentIndex::Ds,
        TLB_DATA,
        BusAccessKind::DataRead,
    )
    .expect("the byte reads");
    assert_eq!(
        cpu.perf_counters().tlb_walks,
        walks_before,
        "the direct, non-proxy check (review D5): no walk ran on the retained entry"
    );
}

/// The ring is real for the TLB too, mirroring `a_second_directory_gets_its_own_generation`: a
/// virgin slot 1 (DIR_B, never before selected) must not accidentally read A's entry live. DIR_B
/// shares A's page table, so this cannot be proven by physical divergence -- only by the
/// generation itself never having been touched under B before this row inserts into it.
#[test]
fn a_second_directory_starts_with_no_tlb_entries_of_its_own() {
    let (mut cpu, mut bus) = cr3_fixture();
    warm_writer_and_tlb_data(&mut cpu, &mut bus);
    assert!(cpu.tlb.lookup(TLB_DATA >> 12).is_some());

    write_cr3(&mut cpu, &mut bus, DIR_B);

    assert!(
        cpu.tlb.lookup(TLB_DATA >> 12).is_none(),
        "B must not see A's entry live merely because it maps the same bytes"
    );
}

/// R-D3, the control that must stay red forever (design review D6 corrects the design's own
/// wording: R-D3 is a GREEN-FOREVER control, not a "must pass on shipped code" line -- the
/// property under test never changes, before or after T2). Same shape as R-D2, but with a store
/// into the PAGE-TABLE page between the two writes. DIR_B shares A's table
/// (`plant_two_directory_paging`), so the store is a genuine cross-context PTE edit and the
/// translation-page watch retires the whole ring, both TLB generations included (review D2).
#[test]
fn r_d3_a_pte_edit_between_the_two_writes_still_kills_the_entry() {
    let (mut cpu, mut bus) = cr3_fixture();
    warm_writer_and_tlb_data(&mut cpu, &mut bus);
    assert!(cpu.tlb.lookup(TLB_DATA >> 12).is_some());

    write_cr3(&mut cpu, &mut bus, DIR_B);
    let data_pte = 0x9000 + (TLB_DATA >> 12) * 4;
    let new_pte = ALT_FRAME | 3;
    cpu.write_memory_sized(
        &mut bus,
        SegmentIndex::Ds,
        data_pte,
        OperandSize::Dword,
        new_pte,
        BusAccessKind::DataWrite,
    )
    .expect("the guest PTE store must retire");

    write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);

    assert!(
        cpu.tlb.lookup(TLB_DATA >> 12).is_none(),
        "a PTE store between the two writes must still kill the entry, on every commit"
    );
}

/// D3: `generations` must never seed a slot with 0, `TlbEntry::EMPTY`'s own sentinel. The FIRST
/// ever switch to a second directory (DIR_B) takes R2, which calls `select_generation` -- NOT
/// `allocate_generation` -- exactly like the JIT link graph's R2 arm (design (b) L1's argument,
/// mirrored). A virgin slot's generation is therefore whatever `Tlb::default()` seeded, never
/// minted at select time. If that seed were `0`, EVERY untouched array slot (`TlbEntry::EMPTY`
/// has `generation: 0, tag: 0`) would read as a live hit for linear page 0 the instant B is
/// selected. Verified by hand: mutating `Tlb::default()`'s `generations: [1, 2]` to `[1, 0]`
/// reddens exactly this row, reverted after.
#[test]
fn a_virgin_ring_slot_never_serves_an_untouched_tlb_entry() {
    let (mut cpu, mut bus) = cr3_fixture();

    write_cr3(&mut cpu, &mut bus, DIR_B);

    assert!(
        cpu.tlb.lookup(0).is_none(),
        "D3: a virgin ring slot's seeded generation must never equal the EMPTY sentinel (0)"
    );
}

/// M3 (design mutation ledger): a generation wrap must never let two ring slots read the same
/// value, even though the wrap ALSO clears every entry (which is why this asserts the stored
/// VALUES directly through `generations_for_test`, rather than through `lookup`: a lookup-only
/// row would pass even with a genuine R6-style collision, because the entries-clear masks it).
#[test]
fn generation_wrap_never_lets_two_ring_slots_alias() {
    let (mut cpu, mut bus) = cr3_fixture();
    write_cr3(&mut cpu, &mut bus, DIR_B);

    cpu.tlb.set_next_generation_for_test(u32::MAX);
    write_cr3(&mut cpu, &mut bus, DIR_C); // R3: both slots occupied, forces retire_all_slots.

    let [slot0, slot1] = cpu.tlb.generations_for_test();
    assert_ne!(
        slot0, slot1,
        "a wrap must never leave two ring slots reading the same generation (R6/D3)"
    );
    assert_ne!(slot0, 0, "the wrap must never mint the EMPTY sentinel");
    assert_ne!(slot1, 0, "the wrap must never mint the EMPTY sentinel");
}

/// M4 / design section (d)'s INVLPG obligation, tested at the mechanism level. `Tlb::invalidate`
/// clears only the entry under the LIVE generation; a dormant slot's array-index-colliding entry
/// for the same linear page is untouched by it, which is exactly what this row constructs: B
/// warms the entry, A (now live) invalidates the same page. `execute_extended.rs`'s INVLPG
/// handler ALSO runs a wholesale decode/link teardown afterward
/// (`invalidate_translation_code_caches`), which retires both TLB generations as a side effect of
/// retiring the ring (review D2) and would mask this specific mutation in an end-to-end row --
/// so this row calls the two lines directly, in the same order the handler does, to isolate the
/// obligation `retire_dormant_slot` exists for. Verified by hand: commenting out
/// `execute_extended.rs`'s `self.tlb.retire_dormant_slot();` call does NOT redden any
/// end-to-end INVLPG row for exactly that masking reason -- recorded in the mutation ledger below
/// rather than claimed as coverage it is not.
#[test]
fn invlpg_retires_the_dormant_generation_too() {
    let (mut cpu, mut bus) = cr3_fixture();
    // Prime WRITER's page (0) and TLB_DATA's (3) together, BEFORE any `write_cr3`: the ring is
    // not yet seeded here, so the one-time retire this always costs (see
    // `warm_writer_and_tlb_data`'s doc comment) disrupts nothing. Doing this AFTER the first
    // `write_cr3` instead would retire the ring MID-SEQUENCE, after B's slot already exists,
    // which is exactly the ordering trap this comment exists to name.
    warm_writer_and_tlb_data(&mut cpu, &mut bus);

    write_cr3(&mut cpu, &mut bus, DIR_B);
    // B's own entry: TLB_DATA's page was already walked under A above (no new accessed-bit
    // store, so no further retire), so this just re-inserts under B's generation.
    cpu.read_memory_u8(
        &mut bus,
        SegmentIndex::Ds,
        TLB_DATA,
        BusAccessKind::DataRead,
    )
    .expect("the byte reads");
    assert!(cpu.tlb.lookup(TLB_DATA >> 12).is_some());

    write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
    // The array slot TLB_DATA collides on still carries B's generation (A never touched it), so
    // this call alone is a no-op -- the gap the design names.
    cpu.tlb.invalidate(TLB_DATA >> 12);
    cpu.tlb.retire_dormant_slot();

    write_cr3(&mut cpu, &mut bus, DIR_B);
    assert!(
        cpu.tlb.lookup(TLB_DATA >> 12).is_none(),
        "the dormant generation must retire too, or B's stale entry resurrects"
    );
}

/// The gap `invlpg_retires_the_dormant_generation_too` exists to close, isolated: `invalidate`
/// ALONE (no `retire_dormant_slot`) must leave the dormant entry reachable. Anti-regression for
/// the `Tlb::invalidate` contract itself, not a claim that production code stops here.
#[test]
fn invalidate_alone_does_not_reach_the_dormant_slot() {
    let (mut cpu, mut bus) = cr3_fixture();
    // Same ordering rule as `invlpg_retires_the_dormant_generation_too`: prime both pages before
    // any `write_cr3`, while the ring is still unseeded.
    warm_writer_and_tlb_data(&mut cpu, &mut bus);

    write_cr3(&mut cpu, &mut bus, DIR_B);
    cpu.read_memory_u8(
        &mut bus,
        SegmentIndex::Ds,
        TLB_DATA,
        BusAccessKind::DataRead,
    )
    .expect("the byte reads");

    write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
    cpu.tlb.invalidate(TLB_DATA >> 12);

    write_cr3(&mut cpu, &mut bus, DIR_B);
    assert!(
        cpu.tlb.lookup(TLB_DATA >> 12).is_some(),
        "this is the documented gap, not a bug: invalidate() alone is generation-scoped to the \
         LIVE slot, which is exactly why retire_dormant_slot exists"
    );
}

/// M5, the `frame_remap`-shaped row (design falsification FS3, mirrored for the TLB): write a
/// PTE, reload the SAME CR3 value, assert the NEW mapping is what a subsequent access sees.
/// `select_context` takes R1 for a same-value reload, but the PTE store already retired the ring
/// (both TLB generations, review D2), so this is really an R2 allocate into an empty ring wearing
/// an R1 costume. A T2 that skipped the retire on the translation-page arm would silently keep
/// serving the OLD frame here, with the right instruction count and the wrong bytes.
#[test]
fn frame_remap_shaped_row_uses_the_new_mapping_after_a_same_value_reload() {
    let (mut cpu, mut bus) = cr3_fixture();
    warm_writer_and_tlb_data(&mut cpu, &mut bus);
    assert_eq!(
        cpu.tlb.lookup(TLB_DATA >> 12).unwrap().phys,
        TLB_DATA & !0xfff,
        "the fixture's identity mapping must hold before the edit"
    );

    let data_pte = 0x9000 + (TLB_DATA >> 12) * 4;
    let new_pte = ALT_FRAME | 3;
    cpu.write_memory_sized(
        &mut bus,
        SegmentIndex::Ds,
        data_pte,
        OperandSize::Dword,
        new_pte,
        BusAccessKind::DataWrite,
    )
    .expect("the guest PTE store must retire");

    write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY); // old == new value: frame_remap's own shape.

    assert!(
        cpu.tlb.lookup(TLB_DATA >> 12).is_none(),
        "the old mapping must not be servable after the translation-page retire"
    );
    cpu.read_memory_u8(
        &mut bus,
        SegmentIndex::Ds,
        TLB_DATA,
        BusAccessKind::DataRead,
    )
    .expect("the byte reads");
    assert_eq!(
        cpu.tlb.lookup(TLB_DATA >> 12).unwrap().phys,
        ALT_FRAME,
        "the re-walk must read the NEW mapping the PTE store installed"
    );
}

/// Section (f)'s "anyone reading `wipe_pages_cleared` as this slice's gate is reading T3's gate"
/// warning, made a check: T1+T2 do not touch the FastMap, so its wipe extent is UNCHANGED by a
/// plain ring-served `MOV CR3` versus one that also warms and drops a direct-page cache entry.
#[cfg(feature = "jit")]
#[test]
fn wipe_pages_cleared_is_not_this_slices_gate() {
    let (mut cpu, mut bus) = cr3_fixture();
    warm_writer_and_tlb_data(&mut cpu, &mut bus);
    let before = cpu.fast_map_audit_counters().wipe_pages_cleared;

    write_cr3(&mut cpu, &mut bus, DIR_B); // R2: allocate, ring-served.
    write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY); // R1: select, ring-served.

    assert_eq!(
        cpu.fast_map_audit_counters().wipe_pages_cleared,
        before,
        "T1 and T2 do not touch the FastMap; wipe_pages_cleared is T3's gate, not this one's"
    );
}

// MUTATION LEDGER, CR3 data-side gate T1+T2 (2026-09-02):
//
// | mutation | row that reddens | verified |
// |---|---|---|
// | M6: re-add `data_read_pages.invalidate()`/`data_write_pages.invalidate()` to the CR3 path | `r_d1_direct_page_cache_survives_a_same_directory_reselect`, `r_d1_direct_page_cache_survives_the_taken_arm_too` | by hand |
// | M1: retire only the live TLB generation at the translation-page arm in `note_code_write_inner` (`self.tlb.retire_all_slots(retired)` -> `self.tlb.flush_live_slot()`) | `r_d3_a_pte_edit_between_the_two_writes_still_kills_the_entry` | by hand |
// | M1, the SMC-wholesale arm's twin: same substitution at the second `note_code_write_inner` call site | `cross_context_smc_store_kills_the_other_context` (parent file) continues to pass at the DECODE layer regardless, since it never reads `cpu.tlb`; the TLB-specific catch for this exact site is `invlpg_retires_the_dormant_generation_too`'s sibling shape is not built here, because the parent file's `cross_context_smc_store_kills_the_other_context` fixture does not warm a TLB entry -- recorded as an untested site rather than claimed. See "Open items" below. |
// | M2: the `Skipped` arm allocates a fresh TLB generation instead of restoring (`select_generation` -> `allocate_generation`) | `r_d2_tlb_entry_survives_a_directory_round_trip` (green would be impossible: a fresh mint can never equal the entry's stored generation) | by hand |
// | D3: seed `Tlb::default()`'s `generations: [1, 2]` -> `[1, 0]` | `a_virgin_ring_slot_never_serves_an_untouched_tlb_entry` | by hand |
// | M3/R6: `retire_all_slots` mints both slots from independent counters instead of one shared monotonic allocator (so a wrap can alias) | `generation_wrap_never_lets_two_ring_slots_alias` | by construction: `mint_generation` is the SOLE minter, so this mutation cannot even be expressed without restructuring the type; recorded as a structural non-gate the way `cpu_cr0_flush_test.rs` records its inert argument swap |
// | M4: drop `self.tlb.retire_dormant_slot()` from `execute_extended.rs`'s INVLPG handler | `invlpg_retires_the_dormant_generation_too` at the MECHANISM level (see that row's doc comment for why an end-to-end INVLPG row cannot catch this: the handler's own subsequent wholesale teardown retires both generations regardless, per D2) | by hand, direct-method level |
// | M5: the translation-page arm retires only the live TLB generation (same substitution as M1, different observable) | `frame_remap_shaped_row_uses_the_new_mapping_after_a_same_value_reload` | by hand |
// | D4: `flush_tlb_keep_code_caches`'s PG=1 arm calls `retire_all_slots` instead of `flush_live_slot` (there is no `RingRetired` token there, so this mutation cannot even compile) | `cpu_cr0_flush_test.rs::data_caches::cr0_ts_change_under_paging_still_flushes_the_tlb` | by construction (compile error) plus that row's `Tlb::lookup` assertion |
// | D2: drop the `Tlb` retire from `invalidate_decode_frontend` (A20/direct-map/aperture/CR0-task-switch-full/INVLPG's shared choke) | no row in THIS file exercises it end to end (the choke's callers are exhaustively tested by `cpu_test.rs`'s and `cpu_cr0_flush_test.rs`'s own suites for DECODE-side correctness); `invalidate_and_clear_code_marks`'s `#[must_use]` return type makes an unconsumed retire a `-D warnings` build failure regardless of test coverage, which is the compile-time half of this gate (design review D2's own stated fix) | by construction |
//
// Open items, recorded rather than silently left uncovered:
//
// * The SMC-wholesale arm's OWN TLB retire (the second `note_code_write_inner` call site,
//   `core.rs`'s `None => { ... }` branch) has no dedicated TLB-observing row in this file. Its
//   decode-side twin (`cross_context_smc_store_kills_the_other_context`, parent file) proves the
//   RING retires; `retire_all_slots`'s `#[must_use]` token proves the CALL SITE cannot compile
//   without consuming a retire. What is untested end to end is the TLB-specific consequence of
//   THAT SPECIFIC site retiring only the live generation, as opposed to the translation-page
//   arm's, which `r_d3_a_pte_edit_between_the_two_writes_still_kills_the_entry` and
//   `frame_remap_shaped_row_uses_the_new_mapping_after_a_same_value_reload` both exercise. The
//   fix is the SAME one-line substitution reviewed by hand for M1 above; a dedicated row would
//   need a fixture that warms a TLB entry AND triggers an SMC store under the other context,
//   which the parent file's SMC fixture does not currently do (its witness is a decode line, not
//   a data byte). Left as a follow-up rather than a rushed fixture.
