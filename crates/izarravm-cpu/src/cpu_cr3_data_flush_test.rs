// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The CR3 DATA-side gate, T1+T2 (the CR3 data-side design, amended by findings D1
//! through D5 of its review and by finding P1 of the review of PR #826).
//!
//! P1 (PR #826, BLOCKING, fixed in the same PR): `ContextSelect::Skipped(slot)` used to cover
//! both R1 (reselect: an occupied slot's saved generation/epoch must be RESTORED) and R2
//! (allocate: a freshly (re)occupied slot's state must be MINTED). INVLPG's own narrow TLB fix
//! (`retire_dormant_slot`) re-mints only the dormant generation while its wholesale decode/link
//! teardown empties the whole ring, so a `MOV CR3` to a brand-new directory right after an
//! INVLPG could take R2 into the reoccupied (formerly live) slot and RESTORE its untouched
//! generation instead of minting -- silently serving one directory's TLB entries to another.
//! `ContextSelect` now distinguishes `Reselected` (R1, restore) from `Allocated` (R2, mint); see
//! `core.rs`'s `ContextSelect` doc comment and `flush_tlb_and_code_caches_for_cr3_write`'s match
//! arms for the fix, and
//! `a_reoccupied_slot_after_invlpg_never_serves_the_vacating_directorys_entries` below for the
//! red-first row.
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

/// P1 (review, PR #826, BLOCKING). INVLPG's own narrow fix (`retire_dormant_slot`) re-mints only
/// the DORMANT generation; the LIVE slot's generation is untouched even though INVLPG's own
/// wholesale decode/link teardown (`invalidate_translation_code_caches`, mirrored here by calling
/// `decode_cache.invalidate_and_clear_code_marks(false)` directly and discarding the token,
/// exactly as `execute_extended.rs`'s INVLPG handler does) EMPTIES the decode ring. Before P1's
/// fix, `select_context`'s R1 (reselect: restore) and R2 (allocate: mint) both returned one
/// `Skipped(slot)`, so a `MOV CR3` to a brand-new directory right after an INVLPG took R2 into
/// the now-empty ring and "restored" (`select_generation`, not `allocate_generation`) slot 0's
/// UNTOUCHED generation -- silently serving directory A's surviving TLB entries to a directory
/// the ring had never seen. It bites on exactly the entry states an INVLPG's own `invalidate`
/// did NOT touch, which is why this row invalidates an UNRELATED page (`WRITER`'s) and checks
/// `TLB_DATA`'s survives the INVLPG only to be required gone after the reoccupation.
///
/// Constructed at the mechanism level, exactly like `invlpg_retires_the_dormant_generation_too`
/// above and for the same reason: an end-to-end row through the real INVLPG opcode exercises
/// this too, but the mechanism level isolates it from everything else INVLPG's handler does.
/// Mutation-verified by hand: reverting the `Allocated` arm in
/// `core.rs::flush_tlb_and_code_caches_for_cr3_write` to call `select_generation` instead of
/// `allocate_generation` (the pre-P1-fix shape) reddens this row; the fixed code leaves it green.
#[test]
fn a_reoccupied_slot_after_invlpg_never_serves_the_vacating_directorys_entries() {
    let (mut cpu, mut bus) = cr3_fixture();
    warm_writer_and_tlb_data(&mut cpu, &mut bus);

    // A same-value reload seeds the ring (R8) and puts directory A properly in slot 0 via a real
    // R1 reselect, exactly as any first `MOV CR3` would. This is load-bearing for reproducing
    // the review's exact sequence: without it, the FIRST `write_cr3` below would be the one that
    // seeds the ring, and the R8 seed always lands in slot 0, which would (coincidentally) mask
    // this bug by never letting the exploit land in the slot whose generation INVLPG left
    // untouched. A guest that has been running for more than one `MOV CR3` always has a seeded
    // ring by the time it issues an INVLPG, which is the case this row must cover.
    write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
    assert!(
        cpu.tlb.lookup(TLB_DATA >> 12).is_some(),
        "the same-value reload above must not have disturbed TLB_DATA's warmed entry"
    );

    // Directory A is live in slot 0. INVLPG targets WRITER's page (0), not TLB_DATA's (3), so
    // TLB_DATA's entry survives the `invalidate` call, still stamped with A's generation. The
    // decode/link teardown that follows every real INVLPG then empties the ring regardless of
    // which page was addressed; `ring_seeded` (set above) stays true, so the ring's next
    // `select_context` call does NOT re-seed slot 0 -- it lands the next directory in whichever
    // slot is free first, which is slot 0, the one INVLPG's narrow fix did not touch.
    cpu.tlb.invalidate(WRITER >> 12);
    cpu.tlb.retire_dormant_slot();
    let _ = cpu.decode_cache.invalidate_and_clear_code_marks(false);

    write_cr3(&mut cpu, &mut bus, DIR_C); // a directory the ring has never seen occupy the slot.

    assert!(
        cpu.tlb.lookup(TLB_DATA >> 12).is_none(),
        "P1: a reoccupied ring slot must MINT a fresh TLB generation, never RESTORE one --          directory A's surviving entry must not answer for directory C, which has never walked          this page"
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

// ---- The CR3 census (`IZARRAVM_FAST_MAP_CR3_CENSUS`) ---------------------------------------------

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
mod cr3_census {
    use super::*;
    use crate::jit::fast_map::{
        parse_cr3_census_for_test as parse_knob, set_cr3_census_for_test as set_knob,
    };

    /// Linear page 0x403 shares the TLB array slot of page 3 (`& 1023`). Directory B reaches it
    /// through `ALIAS_TABLE`, which `collide_plant` fills for physical page 3.
    pub(super) const COLLIDING_LINEAR: u32 = ALIAS_LIN + TLB_DATA;

    /// Also sets the accessed bit in B's first directory entry. Without it, the first walk under B
    /// would store that bit into a directory page the alias walk already marked, and that store
    /// would retire the ring.
    pub(super) fn collide_plant(bus: &mut TestBus) {
        bus.memory[DIR_B as usize..DIR_B as usize + 4]
            .copy_from_slice(&0x0000_9023u32.to_le_bytes());
        let at = ALIAS_TABLE as usize + ((TLB_DATA >> 12) as usize) * 4;
        bus.memory[at..at + 4].copy_from_slice(&(TLB_DATA | ACCESSED_RW).to_le_bytes());
    }

    /// A, then B, then A again. Under B the guest reads `under_b` when it is given. Returns the
    /// CPU and bus so a test can compare guest state.
    fn round_trip(armed: bool, under_b: Option<u32>) -> (CpuGsw, TestBus) {
        set_knob(Some(armed));
        let (mut cpu, mut bus) = data_flush_fixture();
        cpu.set_fast_map_enabled_for_test(true);
        collide_plant(&mut bus);
        warm_writer_and_tlb_data(&mut cpu, &mut bus);
        write_cr3(&mut cpu, &mut bus, DIR_B);
        if let Some(linear) = under_b {
            cpu.translate_linear(&mut bus, linear, false)
                .expect("the translation under B must not fault");
        }
        write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
        set_knob(None);
        (cpu, bus)
    }

    #[test]
    fn a_page_left_alone_in_b_is_live_on_return() {
        // This row measures the one-table layout, so both knobs go off before the CPU is built.
        crate::paging::set_tlb_slot_partition_for_test(Some(false));
        crate::jit::fast_map::set_fast_map_two_tables_for_test(Some(false));
        let (cpu, _bus) = round_trip(true, None);
        let audit = cpu.fast_map_audit_counters();
        assert_eq!(audit.cr3_census_switches, 2, "A to B, then B to A");
        assert!(audit.cr3_census_saved_pages >= 1, "{audit:?}");
        assert_eq!(audit.cr3_census_tlb_live_same_physical, 1, "{audit:?}");
        assert_eq!(audit.cr3_census_tlb_live_other_physical, 0, "{audit:?}");
    }

    #[test]
    fn a_colliding_page_in_b_evicts_the_entry_of_a() {
        // This row measures the one-table layout, so both knobs go off before the CPU is built.
        crate::paging::set_tlb_slot_partition_for_test(Some(false));
        crate::jit::fast_map::set_fast_map_two_tables_for_test(Some(false));
        let (quiet, _) = round_trip(true, None);
        let quiet = quiet.fast_map_audit_counters();
        let (cpu, _bus) = round_trip(true, Some(COLLIDING_LINEAR));
        let audit = cpu.fast_map_audit_counters();
        assert_eq!(
            audit.cr3_census_tlb_live_same_physical,
            quiet.cr3_census_tlb_live_same_physical - 1,
            "{audit:?}"
        );
        assert_eq!(
            audit.cr3_census_tlb_absent,
            quiet.cr3_census_tlb_absent + 1,
            "{audit:?}"
        );
        assert!(audit.tlb_insert_evicts_other_generation >= 1, "{audit:?}");
        assert!(
            audit.tlb_insert_evicts_other_generation > quiet.tlb_insert_evicts_other_generation,
            "{audit:?}"
        );
    }

    #[test]
    fn the_census_counters_stay_zero_when_the_knob_is_off() {
        let (cpu, _bus) = round_trip(false, Some(COLLIDING_LINEAR));
        let audit = cpu.fast_map_audit_counters();
        assert_eq!(audit.cr3_census_switches, 0);
        assert_eq!(audit.cr3_census_saved_pages, 0);
        assert_eq!(audit.cr3_census_tlb_live_same_physical, 0);
        assert_eq!(audit.cr3_census_tlb_live_other_physical, 0);
        assert_eq!(audit.cr3_census_tlb_absent, 0);
        assert_eq!(audit.cr3_census_write_not_dirty, 0);
        assert_eq!(audit.cr3_census_not_reselected, 0);
    }

    #[test]
    fn guest_state_is_identical_with_the_knob_on_and_off() {
        let (on, on_bus) = round_trip(true, Some(COLLIDING_LINEAR));
        let (off, off_bus) = round_trip(false, Some(COLLIDING_LINEAR));
        assert!(on == off, "registers and control state must match");
        assert!(on_bus.memory == off_bus.memory, "memory must match");
        assert_eq!(
            on.perf_counters().tlb_walks,
            off.perf_counters().tlb_walks,
            "the census must not change how many walks run"
        );
    }

    #[test]
    fn a_new_directory_counts_its_saved_pages_as_not_reselected() {
        set_knob(Some(true));
        let (mut cpu, mut bus) = data_flush_fixture();
        cpu.set_fast_map_enabled_for_test(true);
        warm_writer_and_tlb_data(&mut cpu, &mut bus);
        write_cr3(&mut cpu, &mut bus, DIR_B);
        write_cr3(&mut cpu, &mut bus, DIR_C); // The third directory retires the ring.
        set_knob(None);
        let audit = cpu.fast_map_audit_counters();
        assert_eq!(audit.cr3_census_tlb_live_same_physical, 0, "{audit:?}");
        assert!(audit.cr3_census_not_reselected >= 1, "{audit:?}");
    }

    #[test]
    fn the_knob_spelling_table() {
        let parse = |value: &str| parse_knob(Ok(value.to_string()));
        assert!(!parse_knob(Err(std::env::VarError::NotPresent)));
        assert!(!parse(""));
        assert!(!parse("0"));
        assert!(parse("1"));
        assert!(parse(" 1 "));
        for bad in ["2", "true", "on", "yes", "-1"] {
            assert!(
                std::panic::catch_unwind(|| parse_knob(Ok(bad.to_string()))).is_err(),
                "{bad:?} must be refused"
            );
        }
    }
}
// ---- The TLB slot partition and the two fast map tables across `MOV CR3` -------------------------
//
// `IZARRAVM_TLB_SLOT_PARTITION` gives each decode-ring slot its own TLB array.
// `IZARRAVM_FAST_MAP_TWO_TABLES` gives each ring slot its own fast map table. A `MOV CR3` that
// changes the live slot swaps the tables, so a context that comes back finds its mappings in
// place. Every row names the knob states it runs under. The fixture pre-sets the accessed and
// dirty bits it does not test, so no page walk stores into a marked table page and retires the
// ring behind the row's back.
//
// The suite runs with debug assertions, so every `MOV CR3` in the two-table mode also runs the
// oracle `debug_verify_live_fast_map` and the checks of the live ring slot and of the R15 slots.
// A native block that runs across a context switch is not built here: the rows cover the native
// side through the slot checks (`the_r15_slots_name_the_live_table_after_every_arm`) and through
// `lookup_access`, which the emitted code mirrors.

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
mod partition_and_two_tables {
    use super::cr3_census::{COLLIDING_LINEAR, collide_plant};
    use super::*;
    use crate::jit::fast_map::{
        NATIVE_STORE_BIAS_POISON, TableTag, set_fast_map_two_tables_for_test,
    };
    use crate::paging::set_tlb_slot_partition_for_test;

    /// Linear page 3: a clean read of a dirty-bit-set page.
    const READ_PAGE: u32 = TLB_DATA;
    /// Written once under A, so its write mapping exists. PTE already dirty.
    const WRITE_PAGE: u32 = 0x4000;
    /// Supervisor-or-user page: its PTE carries the U/S bit.
    const USER_PAGE: u32 = 0x5000;
    /// PTE with the accessed bit and without the dirty bit.
    const CLEAN_PAGE: u32 = 0x6000;
    /// PTE read-only, accessed and dirty.
    const RO_PAGE: u32 = 0x7000;
    /// Reaches the VGA aperture (physical 0xA0000) through its own page table.
    const APERTURE_LINEAR: u32 = 0x0080_8000;
    const APERTURE_TABLE: u32 = 0xE000;
    const APERTURE_PHYSICAL: u32 = 0x000a_0000;

    fn put32(bus: &mut TestBus, at: u32, value: u32) {
        let at = at as usize;
        bus.memory[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn plant(bus: &mut TestBus) {
        for page in 0..16u32 {
            put32(bus, 0x9000 + page * 4, (page << 12) | 0x23);
        }
        put32(bus, 0x9000 + 3 * 4, 0x3063);
        put32(bus, 0x9000 + 4 * 4, 0x4063);
        put32(bus, 0x9000 + 5 * 4, 0x5027);
        put32(bus, 0x9000 + 7 * 4, 0x7061);
        collide_plant(bus);
        // The directories are user-accessible so that page 5 can be a user page. Every other PTE
        // lacks U/S, so every other page stays supervisor-only.
        put32(bus, PAGE_DIRECTORY, 0x9027);
        put32(bus, DIR_B, 0x9027);
        put32(bus, PAGE_DIRECTORY + 2 * 4, APERTURE_TABLE | 0x23);
        put32(bus, APERTURE_TABLE + 8 * 4, APERTURE_PHYSICAL | 0x23);
        bus.memory[READ_PAGE as usize] = 0x90;
    }

    fn fixture(partition: bool, two_tables: bool) -> (CpuGsw, TestBus) {
        set_tlb_slot_partition_for_test(Some(partition));
        set_fast_map_two_tables_for_test(Some(two_tables));
        let (mut cpu, mut bus) = cr3_fixture_sized(0xb_0000);
        set_tlb_slot_partition_for_test(None);
        set_fast_map_two_tables_for_test(None);
        // The rows reach linear pages above 64 KiB through DS.
        let mut ds = cpu.registers.segment(SegmentIndex::Ds);
        ds.limit = 0xffff_ffff;
        cpu.registers.set_segment(SegmentIndex::Ds, ds);
        bus.direct_pages_enabled = true;
        bus.direct_pages_writable = true;
        cpu.set_fast_map_enabled_for_test(true);
        plant(&mut bus);
        (cpu, bus)
    }

    fn read(cpu: &mut CpuGsw, bus: &mut TestBus, linear: u32) -> u8 {
        cpu.read_memory_u8(bus, SegmentIndex::Ds, linear, BusAccessKind::DataRead)
            .expect("the read must not fault")
    }

    fn write(cpu: &mut CpuGsw, bus: &mut TestBus, linear: u32, value: u8) {
        cpu.write_memory_u8(
            bus,
            SegmentIndex::Ds,
            linear,
            value,
            BusAccessKind::DataWrite,
        )
        .expect("the write must not fault");
    }

    /// Whether the fast map serves `linear` the way native code and the interpreter ask for it.
    fn served(cpu: &CpuGsw, linear: u32, write: bool, user: bool, write_protect: bool) -> bool {
        let epoch = if write {
            cpu.data_write_pages.mapping_epoch()
        } else {
            cpu.data_read_pages.mapping_epoch()
        };
        cpu.jit_fast_map
            .lookup_access(linear, epoch, BusWidth::Byte, write, user, write_protect)
            .is_some()
    }

    fn walks(cpu: &CpuGsw) -> u64 {
        cpu.perf_counters().tlb_walks
    }

    fn switches(cpu: &CpuGsw) -> u64 {
        cpu.fast_map_audit_counters().fast_map_two_tables_switches
    }

    /// Touch the pages A works with, in the order a guest would.
    fn warm_a(cpu: &mut CpuGsw, bus: &mut TestBus) {
        read(cpu, bus, READ_PAGE);
        write(cpu, bus, WRITE_PAGE + 0x10, 0x11);
        read(cpu, bus, WRITE_PAGE + 0x10);
        read(cpu, bus, CLEAN_PAGE);
        write(cpu, bus, RO_PAGE + 0x10, 0x22);
        read(cpu, bus, RO_PAGE + 0x10);
    }

    fn round_trip(cpu: &mut CpuGsw, bus: &mut TestBus) {
        write_cr3(cpu, bus, DIR_B);
        write_cr3(cpu, bus, PAGE_DIRECTORY);
    }

    /// The fast map and the TLB agree on the live ring slot.
    fn assert_slots_agree(cpu: &CpuGsw) {
        assert_eq!(
            usize::from(cpu.jit_fast_map.live_slot()),
            cpu.tlb.live_slot(),
            "the live fast map table names another ring slot than the TLB"
        );
    }

    // ---- Part A: one TLB array for each ring slot ------------------------------------------------

    #[test]
    fn partition_keeps_a_page_of_a_after_b_touches_the_same_array_slot() {
        for partition in [false, true] {
            let (mut cpu, mut bus) = fixture(partition, false);
            read(&mut cpu, &mut bus, READ_PAGE);
            write_cr3(&mut cpu, &mut bus, DIR_B);
            cpu.translate_linear(&mut bus, COLLIDING_LINEAR, false)
                .expect("the translation under B must not fault");
            if partition {
                assert!(
                    cpu.tlb.lookup(COLLIDING_LINEAR >> 12).is_some(),
                    "B keeps its own entry"
                );
            }
            write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);

            let before = walks(&cpu);
            read(&mut cpu, &mut bus, READ_PAGE);
            let audit = cpu.fast_map_audit_counters();
            if partition {
                assert!(cpu.tlb.lookup(READ_PAGE >> 12).is_some());
                assert_eq!(walks(&cpu), before, "no page walk for the entry of A");
                assert_eq!(audit.tlb_insert_evicts_other_generation, 0, "{audit:?}");
            } else {
                assert_eq!(
                    walks(&cpu),
                    before + 1,
                    "the shared array lost the entry of A"
                );
                assert!(audit.tlb_insert_evicts_other_generation >= 1, "{audit:?}");
            }
        }
    }

    /// INVLPG in B, at the mechanism level (the handler also tears the ring down, which would hide
    /// a missing `retire_dormant_slot`). The partition keeps the entry of A in its own array, so
    /// only the retire of the dormant generation makes it unreachable.
    #[test]
    fn partition_invlpg_in_b_makes_the_entry_of_a_unreachable() {
        let (mut cpu, mut bus) = fixture(true, false);
        read(&mut cpu, &mut bus, READ_PAGE);
        write_cr3(&mut cpu, &mut bus, DIR_B);
        read(&mut cpu, &mut bus, READ_PAGE);
        assert!(cpu.tlb.lookup(READ_PAGE >> 12).is_some());

        cpu.tlb.invalidate(READ_PAGE >> 12);
        cpu.tlb.retire_dormant_slot();
        assert!(cpu.tlb.lookup(READ_PAGE >> 12).is_none(), "gone in B");

        write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
        assert!(
            cpu.tlb.lookup(READ_PAGE >> 12).is_none(),
            "the entry of A for the same linear page must not come back"
        );
        let before = walks(&cpu);
        read(&mut cpu, &mut bus, READ_PAGE);
        assert_eq!(walks(&cpu), before + 1, "A walks again");
    }

    /// The same, through the production INVLPG path, for both knob states.
    #[test]
    fn invlpg_in_b_leaves_no_entry_in_either_context() {
        for partition in [false, true] {
            let (mut cpu, mut bus) = fixture(partition, false);
            read(&mut cpu, &mut bus, READ_PAGE);
            write_cr3(&mut cpu, &mut bus, DIR_B);
            read(&mut cpu, &mut bus, READ_PAGE);
            cpu.apply_invlpg(READ_PAGE);
            assert!(cpu.tlb.lookup(READ_PAGE >> 12).is_none());
            write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
            assert!(cpu.tlb.lookup(READ_PAGE >> 12).is_none());
        }
    }

    /// A PTE edit made in B to the table that A uses reaches A: the translation-page watch
    /// retires the ring, so A walks again and sees the new frame.
    #[test]
    fn partition_a_pte_edit_made_in_b_is_observed_in_a() {
        for partition in [false, true] {
            let (mut cpu, mut bus) = fixture(partition, false);
            read(&mut cpu, &mut bus, READ_PAGE);
            write_cr3(&mut cpu, &mut bus, DIR_B);
            cpu.write_memory_sized(
                &mut bus,
                SegmentIndex::Ds,
                0x9000 + (READ_PAGE >> 12) * 4,
                OperandSize::Dword,
                ALT_FRAME | 0x63,
                BusAccessKind::DataWrite,
            )
            .expect("the guest PTE store must retire");
            write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
            assert_eq!(
                cpu.translate_linear(&mut bus, READ_PAGE, false)
                    .expect("A translates"),
                ALT_FRAME,
                "partition={partition}"
            );
        }
    }

    /// A third CR3 value retires both arrays: no entry of either array matches a live generation.
    #[test]
    fn partition_a_third_directory_leaves_no_stale_entry_in_either_array() {
        let (mut cpu, mut bus) = fixture(true, false);
        read(&mut cpu, &mut bus, READ_PAGE);
        write_cr3(&mut cpu, &mut bus, DIR_B);
        read(&mut cpu, &mut bus, READ_PAGE);
        write_cr3(&mut cpu, &mut bus, DIR_C);
        let generations = cpu.tlb.generations_for_test();
        for array in 0..2 {
            for entry in cpu.tlb.array_of_slot_for_test(array) {
                assert!(
                    !generations.contains(&entry.generation) || entry.generation == 0,
                    "array {array} holds an entry stamped with a live generation"
                );
            }
        }
        assert!(cpu.tlb.lookup(READ_PAGE >> 12).is_none());
    }

    /// The generation wrap clears both arrays.
    #[test]
    fn partition_a_generation_wrap_clears_both_arrays() {
        let (mut cpu, mut bus) = fixture(true, false);
        read(&mut cpu, &mut bus, READ_PAGE);
        write_cr3(&mut cpu, &mut bus, DIR_B);
        read(&mut cpu, &mut bus, READ_PAGE);
        assert!(
            (0..2).all(|array| cpu
                .tlb
                .array_of_slot_for_test(array)
                .iter()
                .any(|entry| entry.generation != 0)),
            "both arrays hold an entry before the wrap"
        );
        cpu.tlb.set_next_generation_for_test(0);
        write_cr3(&mut cpu, &mut bus, DIR_C);
        for array in 0..2 {
            assert!(
                cpu.tlb
                    .array_of_slot_for_test(array)
                    .iter()
                    .all(|entry| entry.generation == 0),
                "array {array} survived the wrap"
            );
        }
    }

    // ---- Part B: the fast map keeps one table for each ring slot --------------------------------

    /// A program that switches contexts in a loop. Returns what the guest could see.
    fn context_loop(partition: bool, two_tables: bool) -> (CpuGsw, TestBus, Vec<u8>, u64) {
        let (mut cpu, mut bus) = fixture(partition, two_tables);
        // The loop retires the ring once, at round 5. The clean page starts dirty so that its
        // first store does not retire it a second time.
        bus.memory[0x9000 + 6 * 4] |= 0x40;
        let mut seen = Vec::new();
        warm_a(&mut cpu, &mut bus);
        for round in 0..8u8 {
            write_cr3(&mut cpu, &mut bus, DIR_B);
            seen.push(read(&mut cpu, &mut bus, COLLIDING_LINEAR));
            seen.push(read(&mut cpu, &mut bus, ALIAS_LIN + 0x10));
            if round == 5 {
                // A store to a page the alias table maps clean: it sets a dirty bit in a table
                // page the walk marked, which retires the ring.
                write(&mut cpu, &mut bus, COLLIDING_LINEAR + 0x20, 0x77);
            }
            write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
            seen.push(read(&mut cpu, &mut bus, READ_PAGE));
            seen.push(read(&mut cpu, &mut bus, WRITE_PAGE + 0x10));
            write(&mut cpu, &mut bus, WRITE_PAGE + 0x11, round);
            seen.push(read(&mut cpu, &mut bus, CLEAN_PAGE));
            write(&mut cpu, &mut bus, CLEAN_PAGE + 0x12, round);
            seen.push(read(&mut cpu, &mut bus, RO_PAGE + 0x10));
        }
        let total_walks = walks(&cpu);
        (cpu, bus, seen, total_walks)
    }

    /// Row 14: registers, memory, clocks and walks are the same with and without the second
    /// table, on a context loop that also retires the ring once.
    #[test]
    fn guest_state_equals_with_the_partition_and_with_both_knobs() {
        let (base, base_bus, base_seen, base_walks) = context_loop(false, false);
        for (partition, two_tables) in [(true, false), (true, true)] {
            let (cpu, bus, seen, total) = context_loop(partition, two_tables);
            assert!(
                cpu == base,
                "registers differ: partition={partition} two={two_tables}"
            );
            assert!(bus.memory == base_bus.memory, "memory differs");
            assert_eq!(seen, base_seen, "partition={partition} two={two_tables}");
            assert_eq!(cpu.elapsed_clocks, base.elapsed_clocks, "clocks differ");
            if partition {
                assert!(
                    total <= base_walks,
                    "the partition must not add walks: {total} against {base_walks}"
                );
            }
        }
        let (_, _, _, partitioned) = context_loop(true, false);
        let (two, _, _, with_two) = context_loop(true, true);
        assert_eq!(
            with_two, partitioned,
            "the second table must not change how many walks run"
        );
        assert!(switches(&two) > 0, "the loop must switch tables");
        println!("context loop walks: shared array {base_walks}, partition {partitioned}");
    }

    #[test]
    fn a_surviving_page_is_served_again_after_the_round_trip() {
        let (mut cpu, mut bus) = fixture(true, true);
        assert!(cpu.jit_fast_map.two_tables());
        warm_a(&mut cpu, &mut bus);
        assert!(served(&cpu, READ_PAGE, false, false, false));
        round_trip(&mut cpu, &mut bus);
        assert!(
            served(&cpu, READ_PAGE, false, false, false),
            "read mapping survived"
        );
        assert!(
            served(&cpu, WRITE_PAGE, true, false, true),
            "write mapping survived"
        );
        let before = walks(&cpu);
        read(&mut cpu, &mut bus, READ_PAGE);
        write(&mut cpu, &mut bus, WRITE_PAGE + 0x11, 0x12);
        assert_eq!(walks(&cpu), before, "the pages of A need no walk");
        assert_eq!(switches(&cpu), 2, "A to B, then B to A");
        assert_slots_agree(&cpu);
        assert_eq!(
            cpu.fast_map_audit_counters().fast_map_two_tables_tag_wipes,
            0
        );
    }

    #[test]
    fn the_knob_off_wipes_on_every_write_as_before() {
        for partition in [false, true] {
            let (mut cpu, mut bus) = fixture(partition, false);
            assert!(!cpu.jit_fast_map.two_tables());
            warm_a(&mut cpu, &mut bus);
            round_trip(&mut cpu, &mut bus);
            assert!(!served(&cpu, READ_PAGE, false, false, false));
            assert_eq!(switches(&cpu), 0);
        }
    }

    /// Row 13: the knob does nothing without the TLB slot partition.
    #[test]
    fn the_knob_has_no_effect_without_the_partition() {
        let (mut cpu, mut bus) = fixture(false, true);
        assert!(!cpu.jit_fast_map.two_tables(), "no partition");
        warm_a(&mut cpu, &mut bus);
        round_trip(&mut cpu, &mut bus);
        assert!(!served(&cpu, READ_PAGE, false, false, false));
        assert_eq!(switches(&cpu), 0);
    }

    /// A clean PTE: the read mapping returns, the write mapping does not exist until the
    /// interpreter has walked and set the dirty bit.
    #[test]
    fn a_clean_page_needs_the_walk_and_the_dirty_bit_before_a_write_mapping() {
        let (mut cpu, mut bus) = fixture(true, true);
        warm_a(&mut cpu, &mut bus);
        round_trip(&mut cpu, &mut bus);
        assert!(served(&cpu, CLEAN_PAGE, false, false, false));
        assert!(!served(&cpu, CLEAN_PAGE, true, false, false));
        assert_eq!(bus.memory[0x9000 + 6 * 4] & 0x40, 0, "PTE still clean");

        let before = walks(&cpu);
        write(&mut cpu, &mut bus, CLEAN_PAGE + 0x20, 0x33);
        assert_eq!(walks(&cpu), before + 1, "the first write walks");
        assert_eq!(bus.memory[0x9000 + 6 * 4] & 0x40, 0x40, "the walk set D");
        assert!(served(&cpu, CLEAN_PAGE, true, false, false));
    }

    /// The oracle refuses a write mapping whose TLB entry is not dirty. The hook that clears the
    /// dirty bit builds a state production cannot reach. Debug builds only.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "without a dirty TLB entry")]
    fn the_oracle_refuses_a_write_mapping_without_a_dirty_tlb_entry() {
        let (mut cpu, mut bus) = fixture(true, true);
        warm_a(&mut cpu, &mut bus);
        cpu.tlb.clear_dirty_for_test(WRITE_PAGE >> 12);
        write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
    }

    #[test]
    fn a_read_only_page_stays_read_only() {
        let (mut cpu, mut bus) = fixture(true, true);
        warm_a(&mut cpu, &mut bus);
        round_trip(&mut cpu, &mut bus);
        assert!(served(&cpu, RO_PAGE, false, false, true));
        assert!(
            !served(&cpu, RO_PAGE, true, false, true),
            "a write-protected store must not take the fast path"
        );
        assert!(
            served(&cpu, RO_PAGE, true, false, false),
            "a supervisor store with WP clear keeps the write mapping, as before the switch"
        );
        assert!(
            served(&cpu, WRITE_PAGE, true, false, true),
            "a writable page is writable"
        );
    }

    /// Row 1: B puts a decode line on the physical page that A maps as data at another linear
    /// address. The sweep of the watch edge reaches the table of A while it is dormant, so A
    /// finds no entry with a fast store bias when it comes back.
    #[test]
    fn a_watch_edge_raised_in_b_poisons_the_entry_of_a_before_any_store_in_a() {
        let (mut cpu, mut bus) = fixture(true, true);
        read(&mut cpu, &mut bus, READ_PAGE);
        write(&mut cpu, &mut bus, READ_PAGE + 0x10, 0x44);
        assert_ne!(
            cpu.jit_fast_map.store_bias_for_test(READ_PAGE),
            NATIVE_STORE_BIAS_POISON,
            "the entry starts with a fast store bias"
        );
        write_cr3(&mut cpu, &mut bus, DIR_B);
        assert!(cpu.jit_fast_map.dormant_has_entry_for_test(READ_PAGE));
        cpu.fetch_decoded(&mut bus, COLLIDING_LINEAR)
            .expect("B decodes the byte at physical page 3");
        let d = cpu.registers.cs().default_size_32;
        assert!(cpu.decode_cache.line_live(COLLIDING_LINEAR, d));
        assert!(
            !cpu.jit_fast_map.dormant_has_entry_for_test(READ_PAGE),
            "the edge swept the dormant table"
        );
        assert_eq!(
            cpu.jit_fast_map.dormant_store_bias_for_test(READ_PAGE),
            NATIVE_STORE_BIAS_POISON
        );

        write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
        assert_eq!(
            cpu.jit_fast_map.store_bias_for_test(READ_PAGE),
            NATIVE_STORE_BIAS_POISON,
            "no unguarded native store through the entry of A"
        );
        // A store from A kills the line, as it does without the knob.
        write(&mut cpu, &mut bus, READ_PAGE, 0xf4);
        assert!(!cpu.decode_cache.line_live(COLLIDING_LINEAR, d));
    }

    /// Row 2: a guest store to a translation page in B retires the ring. A comes back by
    /// allocation, the wipe empties its table, and the new frame shows.
    #[test]
    fn a_translation_page_write_in_b_wipes_the_table_of_a() {
        let (mut cpu, mut bus) = fixture(true, true);
        warm_a(&mut cpu, &mut bus);
        write_cr3(&mut cpu, &mut bus, DIR_B);
        cpu.write_memory_sized(
            &mut bus,
            SegmentIndex::Ds,
            0x9000 + (READ_PAGE >> 12) * 4,
            OperandSize::Dword,
            ALT_FRAME | 0x63,
            BusAccessKind::DataWrite,
        )
        .expect("the guest PTE store must retire");
        assert_slots_agree(&cpu);
        write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
        assert_slots_agree(&cpu);
        assert!(!served(&cpu, READ_PAGE, false, false, false));
        assert!(!served(&cpu, WRITE_PAGE, true, false, false));
        assert_eq!(
            cpu.translate_linear(&mut bus, READ_PAGE, false)
                .expect("A translates"),
            ALT_FRAME
        );
    }

    /// A live table that lost track of the live ring slot is a defect. Debug builds stop at once.
    /// A release build wipes both tables and takes the slot of the TLB.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "lost track of the live ring slot")]
    fn a_live_table_that_lost_its_slot_stops_a_debug_build() {
        let (mut cpu, mut bus) = fixture(true, true);
        warm_a(&mut cpu, &mut bus);
        cpu.jit_fast_map.relabel_live(1);
        write_cr3(&mut cpu, &mut bus, DIR_B);
    }

    /// Row 3: INVLPG in B tears the ring down. The table of A is wiped when A comes back, and the
    /// table B left is never selected again.
    #[test]
    fn invlpg_in_b_then_a_wipes_and_never_reselects_the_old_table_of_b() {
        let (mut cpu, mut bus) = fixture(true, true);
        warm_a(&mut cpu, &mut bus);
        write_cr3(&mut cpu, &mut bus, DIR_B);
        read(&mut cpu, &mut bus, WRITE_PAGE + 0x30);
        assert!(served(&cpu, WRITE_PAGE, false, false, false));
        cpu.apply_invlpg(CLEAN_PAGE);

        write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
        assert!(!served(&cpu, READ_PAGE, false, false, false), "A is wiped");
        assert!(!served(&cpu, WRITE_PAGE, false, false, false));
        assert_slots_agree(&cpu);
        write_cr3(&mut cpu, &mut bus, DIR_B);
        assert!(
            !served(&cpu, WRITE_PAGE, false, false, false),
            "the old table of B must not come back"
        );
        assert_slots_agree(&cpu);
    }

    /// Row 4: a TLB generation wrap clears the arrays but keeps the generation of the dormant
    /// slot, so only the wrap wipe stops the table of A from coming back with no TLB entries.
    #[test]
    fn a_generation_wrap_wipes_both_tables() {
        let (mut cpu, mut bus) = fixture(true, true);
        warm_a(&mut cpu, &mut bus);
        write_cr3(&mut cpu, &mut bus, DIR_B);
        read(&mut cpu, &mut bus, COLLIDING_LINEAR);
        // The next mint wraps. A CR0 write that keeps paging mints for the live slot.
        cpu.tlb.set_next_generation_for_test(0);
        let old = cpu.control.cr0;
        cpu.control.cr0 = old ^ CR0_TS;
        cpu.flush_tlb_for_cr0_write(old, old ^ CR0_TS);
        assert!(cpu.jit_fast_map.dormant_has_entry_for_test(READ_PAGE));

        write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
        assert!(!served(&cpu, READ_PAGE, false, false, false));
        assert!(cpu.jit_fast_map.dormant_live_pages_for_test().is_empty());
        assert!(cpu.tlb.lookup(READ_PAGE >> 12).is_none());
        assert_slots_agree(&cpu);
    }

    /// Row 5: a table whose tag names another generation is wiped and counted.
    #[test]
    fn a_forced_tag_mismatch_wipes_and_counts() {
        let (mut cpu, mut bus) = fixture(true, true);
        warm_a(&mut cpu, &mut bus);
        write_cr3(&mut cpu, &mut bus, DIR_B);
        assert!(cpu.jit_fast_map.dormant_has_entry_for_test(READ_PAGE));
        let tag = cpu.jit_fast_map.dormant_tag();
        assert_eq!(
            tag,
            cpu.fast_map_tag_of(0),
            "the tag of A names its slot now"
        );
        cpu.jit_fast_map.set_dormant_tag_for_test(TableTag {
            slot: tag.slot,
            generation: tag.generation + 100,
        });
        write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
        assert!(!served(&cpu, READ_PAGE, false, false, false));
        assert_eq!(
            cpu.fast_map_audit_counters().fast_map_two_tables_tag_wipes,
            1
        );
    }

    /// Row 6: the six R15 slots name the live table after every arm, including the first switch,
    /// which gives the second table its storage.
    #[test]
    fn the_r15_slots_name_the_live_table_after_every_arm() {
        let (mut cpu, mut bus) = fixture(true, true);
        warm_a(&mut cpu, &mut bus);
        // Stand in for the compile that publishes the table of A.
        let a_bases = cpu.jit_fast_map.native_bases().expect("A has storage");
        cpu.native_table_slots.publish_map(a_bases);
        assert!(cpu.native_table_slots.map_slots_match(Some(a_bases)));

        // Allocated: the first switch. The second table gets its storage here.
        write_cr3(&mut cpu, &mut bus, DIR_B);
        let b_bases = cpu.jit_fast_map.native_bases().expect("B has storage");
        assert!(cpu.native_table_slots.map_slots_match(Some(b_bases)));
        assert!(!cpu.native_table_slots.map_slots_match(Some(a_bases)));
        assert_eq!(
            cpu.jit_fast_map
                .dormant_native_bases_for_test()
                .map(|b| b.flags()),
            Some(a_bases.flags())
        );
        // Reselected, other slot.
        write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
        assert!(cpu.native_table_slots.map_slots_match(Some(a_bases)));
        // Reselected, same slot.
        write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
        assert!(cpu.native_table_slots.map_slots_match(Some(a_bases)));
        // Taken: a third directory.
        write_cr3(&mut cpu, &mut bus, DIR_B);
        write_cr3(&mut cpu, &mut bus, DIR_C);
        let live = cpu.jit_fast_map.native_bases().expect("storage stays");
        assert!(cpu.native_table_slots.map_slots_match(Some(live)));
        cpu.check_fast_map_slots();
    }

    /// Row 7: a retire while B is live renumbers the ring. Four more writes keep the live table
    /// and the live TLB slot in step, and every translation is right.
    #[test]
    fn a_retire_while_b_is_live_keeps_the_live_slots_in_step() {
        let (mut cpu, mut bus) = fixture(true, true);
        warm_a(&mut cpu, &mut bus);
        write_cr3(&mut cpu, &mut bus, DIR_B);
        assert_eq!(cpu.tlb.live_slot(), 1);
        let token = cpu.decode_cache.invalidate_and_clear_code_marks(false);
        cpu.retire_tlb_slots(token);
        assert_slots_agree(&cpu);
        for dir in [PAGE_DIRECTORY, DIR_B, PAGE_DIRECTORY, DIR_B] {
            write_cr3(&mut cpu, &mut bus, dir);
            assert_slots_agree(&cpu);
            assert_eq!(
                cpu.translate_linear(&mut bus, READ_PAGE, false)
                    .expect("translates"),
                READ_PAGE
            );
            assert_eq!(read(&mut cpu, &mut bus, WRITE_PAGE + 0x10), 0x11);
        }
    }

    /// Row 8: a clone made while slot 1 is live starts with an empty TLB and an empty fast map.
    /// Directory C maps linear page 0 to another frame than A does. A clone that kept the live
    /// slot and the generations of the original would revive the translation of A under C.
    #[test]
    fn a_clone_made_while_slot_1_is_live_translates_correctly() {
        let (mut cpu, mut bus) = fixture(true, true);
        // The writer must run under C too, where page 0 is another frame.
        let writer = mov_to_cr(3);
        let at = (ALT_FRAME + WRITER) as usize;
        bus.memory[at..at + writer.len()].copy_from_slice(&writer);
        cpu.translate_linear(&mut bus, 0, false)
            .expect("A translates page 0");
        write_cr3(&mut cpu, &mut bus, DIR_C);
        assert_eq!(cpu.tlb.live_slot(), 1, "C is in slot 1");
        assert_eq!(
            cpu.translate_linear(&mut bus, 0, false)
                .expect("C translates page 0"),
            ALT_FRAME
        );

        let mut clone = cpu.clone();
        assert!(clone.jit_fast_map.two_tables());
        assert_eq!(
            clone.tlb.live_slot(),
            0,
            "the clone TLB starts like a new one"
        );
        assert!(clone.tlb.lookup(0).is_none());
        for (dir, expected) in [(DIR_C, ALT_FRAME), (PAGE_DIRECTORY, 0), (DIR_C, ALT_FRAME)] {
            write_cr3(&mut clone, &mut bus, dir);
            assert_slots_agree(&clone);
            assert_eq!(
                clone
                    .translate_linear(&mut bus, 0, false)
                    .expect("the clone translates"),
                expected,
                "directory {dir:#x}"
            );
        }
    }

    /// Row 9: a paging toggle and a task-switch flush wipe both tables and renumber the ring.
    /// CR3 writes after them keep every translation right.
    #[test]
    fn a_paging_toggle_or_a_task_switch_then_cr3_writes_are_correct() {
        for task_switch in [false, true] {
            let (mut cpu, mut bus) = fixture(true, true);
            warm_a(&mut cpu, &mut bus);
            write_cr3(&mut cpu, &mut bus, DIR_B);
            read(&mut cpu, &mut bus, COLLIDING_LINEAR);
            if task_switch {
                cpu.flush_tlb_and_code_caches(TranslationFlushReason::TaskSwitch);
            } else {
                let old = cpu.control.cr0;
                cpu.control.cr0 = old & !CR0_PG;
                cpu.flush_tlb_for_cr0_write(old, old & !CR0_PG);
                cpu.control.cr0 = old;
                cpu.flush_tlb_for_cr0_write(old & !CR0_PG, old);
            }
            assert!(cpu.jit_fast_map.dormant_live_pages_for_test().is_empty());
            assert_eq!(cpu.jit_fast_map.live_page_count_for_test(), 0);
            assert_slots_agree(&cpu);
            for dir in [PAGE_DIRECTORY, DIR_B, PAGE_DIRECTORY] {
                write_cr3(&mut cpu, &mut bus, dir);
                assert_slots_agree(&cpu);
                assert_eq!(
                    cpu.translate_linear(&mut bus, READ_PAGE, false)
                        .expect("translates"),
                    READ_PAGE
                );
            }
        }
    }

    /// Row 10 and the CR0 row: a CR0 write that keeps paging wipes the live table only. A keeps
    /// its table and finds it on return.
    #[test]
    fn a_cr0_write_in_b_with_paging_kept_wipes_only_the_live_table() {
        let (mut cpu, mut bus) = fixture(true, true);
        warm_a(&mut cpu, &mut bus);
        write_cr3(&mut cpu, &mut bus, DIR_B);
        read(&mut cpu, &mut bus, COLLIDING_LINEAR);
        assert!(cpu.jit_fast_map.live_page_count_for_test() > 0);
        let old = cpu.control.cr0;
        cpu.control.cr0 = old ^ CR0_TS;
        cpu.flush_tlb_for_cr0_write(old, old ^ CR0_TS);
        assert_ne!(cpu.control.cr0 & CR0_PG, 0, "PG stays 1");
        assert_eq!(cpu.jit_fast_map.live_page_count_for_test(), 0, "B is wiped");
        assert!(cpu.jit_fast_map.dormant_has_entry_for_test(READ_PAGE));

        write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
        assert!(served(&cpu, READ_PAGE, false, false, false));
        assert!(served(&cpu, WRITE_PAGE, true, false, true));
        let before = walks(&cpu);
        read(&mut cpu, &mut bus, READ_PAGE);
        write(&mut cpu, &mut bus, WRITE_PAGE + 0x11, 0x12);
        assert_eq!(walks(&cpu), before, "no walk for the pages of A");
        assert_eq!(
            cpu.fast_map_audit_counters().fast_map_two_tables_tag_wipes,
            0
        );
    }

    /// Row 11: a WP change wipes both tables.
    #[test]
    fn a_wp_toggle_wipes_both_tables() {
        let (mut cpu, mut bus) = fixture(true, true);
        warm_a(&mut cpu, &mut bus);
        write_cr3(&mut cpu, &mut bus, DIR_B);
        read(&mut cpu, &mut bus, COLLIDING_LINEAR);
        assert!(cpu.jit_fast_map.live_page_count_for_test() > 0);
        assert!(!cpu.jit_fast_map.dormant_live_pages_for_test().is_empty());
        let old = cpu.control.cr0;
        cpu.control.cr0 = old ^ CR0_WP;
        cpu.flush_tlb_for_cr0_write(old, old ^ CR0_WP);
        assert_eq!(cpu.jit_fast_map.live_page_count_for_test(), 0);
        assert!(cpu.jit_fast_map.dormant_live_pages_for_test().is_empty());
        assert_slots_agree(&cpu);
        write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
        assert!(!served(&cpu, READ_PAGE, false, false, false));
    }

    /// The paging toggle row: both tables are empty after it.
    #[test]
    fn a_paging_toggle_empties_both_tables() {
        let (mut cpu, mut bus) = fixture(true, true);
        warm_a(&mut cpu, &mut bus);
        write_cr3(&mut cpu, &mut bus, DIR_B);
        read(&mut cpu, &mut bus, COLLIDING_LINEAR);
        let old = cpu.control.cr0;
        cpu.control.cr0 = old & !CR0_PG;
        cpu.flush_tlb_for_cr0_write(old, old & !CR0_PG);
        assert_eq!(cpu.jit_fast_map.live_page_count_for_test(), 0);
        assert!(cpu.jit_fast_map.dormant_live_pages_for_test().is_empty());
    }

    /// Row 12: a CR3 write while paging is off wipes both tables first.
    #[test]
    fn a_cr3_write_with_paging_off_wipes_both_tables() {
        let (mut cpu, mut bus) = fixture(true, true);
        warm_a(&mut cpu, &mut bus);
        write_cr3(&mut cpu, &mut bus, DIR_B);
        read(&mut cpu, &mut bus, COLLIDING_LINEAR);
        let old = cpu.control.cr0;
        cpu.control.cr0 = old & !CR0_PG;
        cpu.flush_tlb_for_cr0_write(old, old & !CR0_PG);
        // Paging off: the read fills an unpaged entry in the live table.
        read(&mut cpu, &mut bus, READ_PAGE);
        assert!(cpu.jit_fast_map.live_page_count_for_test() > 0);
        write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
        assert_eq!(cpu.jit_fast_map.live_page_count_for_test(), 0);
        assert!(cpu.jit_fast_map.dormant_live_pages_for_test().is_empty());
        assert_slots_agree(&cpu);
        write_cr3(&mut cpu, &mut bus, DIR_B);
        assert_eq!(cpu.jit_fast_map.live_page_count_for_test(), 0);
    }

    /// The aperture page is allowed in a dormant table. A remap while it is dormant reaches it
    /// (row 16), and without a remap it comes back with its table.
    #[test]
    fn an_aperture_remap_reaches_the_dormant_table() {
        for remap in [false, true] {
            let (mut cpu, mut bus) = fixture(true, true);
            warm_a(&mut cpu, &mut bus);
            read(&mut cpu, &mut bus, APERTURE_LINEAR);
            assert!(
                served(&cpu, APERTURE_LINEAR, false, false, false),
                "the aperture page is mapped before the switch"
            );
            write_cr3(&mut cpu, &mut bus, DIR_B);
            assert!(cpu.jit_fast_map.dormant_has_entry_for_test(APERTURE_LINEAR));
            if remap {
                cpu.note_direct_data_map_changed();
                assert!(!cpu.jit_fast_map.dormant_has_entry_for_test(APERTURE_LINEAR));
            }
            write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
            assert_eq!(
                served(&cpu, APERTURE_LINEAR, false, false, false),
                !remap,
                "remap={remap}"
            );
            assert!(
                served(&cpu, READ_PAGE, false, false, false),
                "remap={remap}: the RAM page of A stays"
            );
        }
    }

    /// A third directory retires the ring: both tables are empty and nothing of A comes back.
    #[test]
    fn a_third_directory_leaves_both_tables_empty() {
        let (mut cpu, mut bus) = fixture(true, true);
        warm_a(&mut cpu, &mut bus);
        write_cr3(&mut cpu, &mut bus, DIR_B);
        read(&mut cpu, &mut bus, COLLIDING_LINEAR);
        write_cr3(&mut cpu, &mut bus, DIR_C);
        assert_eq!(cpu.jit_fast_map.live_page_count_for_test(), 0);
        assert!(cpu.jit_fast_map.dormant_live_pages_for_test().is_empty());
        assert_slots_agree(&cpu);
        write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
        assert!(!served(&cpu, READ_PAGE, false, false, false));
    }

    /// Mappings filled at CPL 0 are checked against the CPL at the time of the access.
    #[test]
    fn a_ring_3_access_follows_the_page_permissions() {
        let (mut cpu, mut bus) = fixture(true, true);
        read(&mut cpu, &mut bus, READ_PAGE);
        read(&mut cpu, &mut bus, USER_PAGE);
        round_trip(&mut cpu, &mut bus);
        assert!(
            served(&cpu, READ_PAGE, false, false, false),
            "ring 0 is served"
        );
        assert!(
            !served(&cpu, READ_PAGE, false, true, false),
            "ring 3 on a supervisor page"
        );
        assert!(
            served(&cpu, USER_PAGE, false, true, false),
            "ring 3 on a user page"
        );

        cpu.cpl = 3;
        let fault = cpu
            .read_memory_u8(
                &mut bus,
                SegmentIndex::Ds,
                READ_PAGE,
                BusAccessKind::DataRead,
            )
            .expect_err("ring 3 must fault on a supervisor page");
        let (mut reference, mut reference_bus) = fixture(false, false);
        read(&mut reference, &mut reference_bus, READ_PAGE);
        read(&mut reference, &mut reference_bus, USER_PAGE);
        round_trip(&mut reference, &mut reference_bus);
        reference.cpl = 3;
        let expected = reference
            .read_memory_u8(
                &mut reference_bus,
                SegmentIndex::Ds,
                READ_PAGE,
                BusAccessKind::DataRead,
            )
            .expect_err("the reference faults too");
        assert_eq!(format!("{fault:?}"), format!("{expected:?}"));
        read(&mut cpu, &mut bus, USER_PAGE);
    }

    /// A same-value reload keeps the table. An edit to a watched PTE before the reload still
    /// retires it.
    #[test]
    fn a_same_value_reload_is_correct() {
        let (mut cpu, mut bus) = fixture(true, true);
        warm_a(&mut cpu, &mut bus);
        write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
        assert!(served(&cpu, READ_PAGE, false, false, false));
        assert_eq!(
            switches(&cpu),
            0,
            "a reload of the live directory swaps nothing"
        );

        // Now edit the PTE of the page and reload the same directory.
        cpu.write_memory_sized(
            &mut bus,
            SegmentIndex::Ds,
            0x9000 + (READ_PAGE >> 12) * 4,
            OperandSize::Dword,
            ALT_FRAME | 0x63,
            BusAccessKind::DataWrite,
        )
        .expect("the guest PTE store must retire");
        write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
        assert_eq!(
            cpu.translate_linear(&mut bus, READ_PAGE, false)
                .expect("A translates"),
            ALT_FRAME
        );
        assert!(
            !cpu.jit_fast_map.has_read_mapping(READ_PAGE, READ_PAGE),
            "the old mapping is gone"
        );
    }
}

// MUTATION LEDGER, CR3 data-side gate T1+T2 (2026-09-02), amended for P1 (PR #826 review, fixed
// in the same PR):
//
// | mutation | row that reddens | verified |
// |---|---|---|
// | P1: revert `core.rs`'s `ContextSelect::Allocated` arm from `self.tlb.allocate_generation(slot)` back to `self.tlb.select_generation(slot)` (the pre-fix shape, indistinguishable from the `Reselected` arm) | `a_reoccupied_slot_after_invlpg_never_serves_the_vacating_directorys_entries` | by hand |
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
