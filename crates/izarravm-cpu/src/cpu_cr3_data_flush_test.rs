// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The CR3 DATA-side gate, T1 (`dev_docs/2026-09-02-cr3-data-side-design.md`, amended by
//! `dev_docs/2026-09-02-cr3-code-cache-gate-review.md`'s "Review of the data-side design"
//! section). T2 (the TLB's own two-slot generation) is a follow-up slice; this file grows to
//! cover it there.
//!
//! T1: `data_read_pages`/`data_write_pages` are physical-page-and-bus-epoch keyed with no CR3
//! dependence at all, so they are removed from the `MOV CR3` wipe unconditionally -- and, as a
//! consequence of the shared helper T1 deletes from, from the CR0/task-switch full flush and the
//! `flush_tlb_keep_code_caches` PG=1 arm too.
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
/// decode-line side. This is #820's pre-existing translation-page watch, not new to T1.
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
/// retires the decode ring -- and T1's claim is that even THIS never touches the physical-page
/// caches, because they have no CR3 dependence to protect.
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

// MUTATION LEDGER, CR3 data-side gate T1 (2026-09-02):
//
// | mutation | row that reddens | verified |
// |---|---|---|
// | M6: re-add `data_read_pages.invalidate()`/`data_write_pages.invalidate()` to `wipe_tlb_and_direct_pages` | `r_d1_direct_page_cache_survives_a_same_directory_reselect`, `r_d1_direct_page_cache_survives_the_taken_arm_too` | by hand |
