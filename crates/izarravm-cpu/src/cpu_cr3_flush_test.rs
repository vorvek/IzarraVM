// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The CR3 code-cache gate (`dev_docs/2026-09-02-cr3-code-cache-gate-design.md`): a two-slot ring
//! of `(cr3 & 0xffff_f000, generation)` that lets `MOV CR3` retain decode lines across a reselect
//! (R1/R2) instead of flushing the whole decode cache on every write, while a third distinct
//! value (R3) or any edit to the structures a walk read (R4/R5, the `translation_pages` watch)
//! still forces today's full teardown.
//!
//! Fixtures live in `super` (`cpu_cr0_flush_test.rs`'s `cr0_flush` module, this file's parent):
//! `real_mode_cr0_cpu`, `WITNESS`/`WRITER`, `warm_witness`, `mov_to_cr`, `PAGE_DIRECTORY` and
//! `plant_identity_paging` (directory `PAGE_DIRECTORY` = `DIR_A` at `0x8000`, table at `0x9000`,
//! identity for the low 16 pages).

use super::*;

/// The second directory: points at the SAME table `plant_identity_paging` builds, so A and B map
/// every identity page -- including the witness's -- to the SAME physical byte. This is what the
/// headline row needs: the property under test is the RING, not a difference in what the two
/// directories mean.
const DIR_B: u32 = 0xA000;
/// A third directory, for the different-physical anti-regression row: points at `ALT_TABLE`,
/// which remaps the witness's page to `ALT_FRAME` instead of leaving it identity.
const DIR_C: u32 = 0xC000;
const ALT_TABLE: u32 = 0xB000;
const ALT_FRAME: u32 = 0x5000;
/// Distinct from the witness's `0x90` (NOP), so a re-decode proves it read NEW bytes rather than
/// merely that the line was dead.
const ALT_WITNESS_BYTE: u8 = 0xf4; // HLT
/// A second NOP, at a linear address that does not collide with `WITNESS`'s decode-cache slot
/// (the cache is direct-mapped, so decoding `WITNESS` itself under B would simply EVICT A's
/// entry -- an ordinary cache collision, nothing to do with the ring). Used by
/// `a_second_directory_gets_its_own_generation` to decode something under B without touching A's
/// slot at all.
const WITNESS_B: u32 = 0x0500;

fn plant_two_directory_paging(memory: &mut [u8]) {
    plant_identity_paging(memory);
    memory[DIR_B as usize..DIR_B as usize + 4].copy_from_slice(&0x0000_9003u32.to_le_bytes());
    memory[DIR_C as usize..DIR_C as usize + 4].copy_from_slice(&(ALT_TABLE | 3).to_le_bytes());
    let alt_pte = ALT_FRAME | 3;
    memory[ALT_TABLE as usize..ALT_TABLE as usize + 4].copy_from_slice(&alt_pte.to_le_bytes());
    for page in 1..16usize {
        let entry = ((page as u32) << 12) | 3;
        let at = ALT_TABLE as usize + page * 4;
        memory[at..at + 4].copy_from_slice(&entry.to_le_bytes());
    }
    memory[(ALT_FRAME + (WITNESS & 0xfff)) as usize] = ALT_WITNESS_BYTE;
}

/// A paged fixture: `MOV CR3, eax` at `WRITER`, the witness NOP at `WITNESS`, CR3 already `DIR_A`
/// (set directly on the struct -- the R8 seed row needs this: lines decoded here are decoded
/// under a CR3 the ring has never seen selected).
fn cr3_fixture() -> (CpuGsw, TestBus) {
    let mut memory = vec![0u8; 0x10000];
    let writer = mov_to_cr(3);
    memory[WRITER as usize..WRITER as usize + writer.len()].copy_from_slice(&writer);
    memory[WITNESS as usize] = 0x90;
    memory[WITNESS_B as usize] = 0x90;
    plant_two_directory_paging(&mut memory);
    let mut cpu = real_mode_cr0_cpu(WRITER);
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.control.cr3 = PAGE_DIRECTORY;
    (cpu, TestBus::with_memory(memory))
}

/// Execute `MOV CR3, eax` at `WRITER` with `eax = value`, exactly like `write_cr0` does for CR0.
fn write_cr3(cpu: &mut CpuGsw, bus: &mut TestBus, value: u32) {
    cpu.registers.set_eax(value);
    cpu.set_eip(WRITER);
    exec_one_split(cpu, bus).expect("MOV CR3 must retire");
}

// ---- The headline row -------------------------------------------------------------------------

/// RED on the branch before the ring exists (a second `MOV CR3` write always fully flushed).
/// The property the ring delivers, and the one a VCPI round trip needs: RETURNING to a directory
/// restores the lines decoded under it, without a re-decode. `line_live` compares against the
/// CURRENT generation, so this can only pass by the ring giving A its OWN slot and generation back
/// -- there is no way to make "B still sees A's line" true without the walk option 2 (design part
/// 2(b)) was rejected for, which is why this is stated as "returning restores", not "B sees A".
///
/// This also pins R8 (finding F12): the witness is decoded BEFORE any `MOV CR3` executes (CR3 is
/// set directly by `cr3_fixture`), so the first write below is the ring's first-ever
/// `select_context` call. Without the seed rule, that call allocates fresh for B and orphans A's
/// line -- the SAME line the second write is trying to restore -- so this row is exactly what F12
/// named as still red in a ring lacking R8.
#[test]
fn returning_to_a_directory_restores_its_decode_lines() {
    let (mut cpu, mut bus) = cr3_fixture();
    warm_witness(&mut cpu, &mut bus);
    let d = cpu.registers.cs().default_size_32;
    let misses_before = cpu.perf_counters().decode_misses;

    write_cr3(&mut cpu, &mut bus, DIR_B);
    assert!(
        !cpu.decode_cache.line_live(WITNESS, d),
        "B must not see A's line live merely because it maps the same bytes"
    );

    write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
    assert!(
        cpu.decode_cache.line_live(WITNESS, d),
        "returning to A must restore its line"
    );
    assert_eq!(
        cpu.perf_counters().decode_misses,
        misses_before,
        "no re-decode occurred: the line came back from the ring, not from a fresh decode"
    );
}

/// The ring is REAL, not a gate that merely skips the flush: B gets its OWN generation, so a line
/// decoded under B does not read live under A without A doing its own decode. Without this row a
/// (buggy) gate that shared one generation across both slots would still pass the row above.
#[test]
fn a_second_directory_gets_its_own_generation() {
    let (mut cpu, mut bus) = cr3_fixture();
    warm_witness(&mut cpu, &mut bus);
    let d = cpu.registers.cs().default_size_32;

    write_cr3(&mut cpu, &mut bus, DIR_B);
    cpu.fetch_decoded(&mut bus, WITNESS_B)
        .expect("a second address decodes under B, in a different slot from WITNESS");
    assert!(cpu.decode_cache.line_live(WITNESS_B, d));
    let misses_after_b = cpu.perf_counters().decode_misses;

    write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
    assert!(
        cpu.decode_cache.line_live(WITNESS, d),
        "A's own earlier line, untouched under B, must still be live"
    );
    assert!(
        !cpu.decode_cache.line_live(WITNESS_B, d),
        "B's line must not read live under A: it belongs to B's own generation"
    );
    assert_eq!(
        cpu.perf_counters().decode_misses,
        misses_after_b,
        "A's return must not re-decode: its line is the FIRST one, still in the ring"
    );
}

// ---- Anti-regression rows, green today (option 1 -- same-value-as-full-flush -- fails these) ---

/// A directory that maps the witness's page to a DIFFERENT physical frame must MISS, and a
/// re-decode must read the frame's bytes, not merely find the line dead.
#[test]
fn different_physical_page_misses_and_redecodes() {
    let (mut cpu, mut bus) = cr3_fixture();
    warm_witness(&mut cpu, &mut bus);
    let d = cpu.registers.cs().default_size_32;

    write_cr3(&mut cpu, &mut bus, DIR_C);
    assert!(
        !cpu.decode_cache.line_live(WITNESS, d),
        "a directory mapping the witness elsewhere must miss"
    );
    cpu.fetch_decoded(&mut bus, WITNESS)
        .expect("re-decode under the new mapping");
    assert_eq!(
        cpu.decode_cache.line_phys_start(WITNESS, d),
        Some(ALT_FRAME + (WITNESS & 0xfff)),
        "the re-decode must read the NEW frame, not the old one"
    );
}

/// A PTE edit under a live CR3, through the GUEST WRITE PATH (the write watch is the whole
/// mechanism, so poking `bus.memory` directly would prove nothing). Kills option 1 (same-value
/// CR3 as a full flush): the write, not the reselect, is what must retire.
#[test]
fn pte_edit_under_a_live_cr3_forces_a_redecode() {
    let (mut cpu, mut bus) = cr3_fixture();
    warm_witness(&mut cpu, &mut bus);
    let d = cpu.registers.cs().default_size_32;

    let witness_pte = 0x9000 + (WITNESS >> 12) * 4;
    let new_pte = ALT_FRAME | 3;
    cpu.write_memory_sized(
        &mut bus,
        SegmentIndex::Ds,
        witness_pte,
        OperandSize::Dword,
        new_pte,
        BusAccessKind::DataWrite,
    )
    .expect("the guest PTE store must retire");
    assert!(
        !cpu.decode_cache.line_live(WITNESS, d),
        "the PTE store alone must kill the line -- no CR3 write has happened yet"
    );

    // The same value CR3 already holds. Only the TLB flush this write does unconditionally makes
    // the edited mapping visible to the next walk; the ring reselect itself changes nothing (R1).
    write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
    cpu.fetch_decoded(&mut bus, WITNESS)
        .expect("re-decode under the edited mapping");
    assert_eq!(
        cpu.decode_cache.line_phys_start(WITNESS, d),
        Some(ALT_FRAME + (WITNESS & 0xfff)),
        "the re-decode must read the frame the edited PTE now names"
    );
}

/// The #820 non-gate, diagnosed and fixed (design doc `2026-09-02-cr3-jit-half-design.md` (e)).
///
/// The original row ran the warm-up BEFORE the witness was decoded. That put the fixture's own
/// order backwards: the warm-up's walk (translating linear page 9 to store page 14's throwaway
/// PTE at `0x9038`) sets A on the directory's PDE and A+D on page 9's OWN PTE (`0x9024`), each
/// store landing before F11's mark, so nothing retires there -- but the walk THEN marks `0x8000`
/// and `0x9000` as translation structure. `warm_witness` ran next and its OWN code walk touched
/// page 0's PTE at `0x9000`, which the warm-up had JUST marked, so the ring retired right there,
/// before the witness line was ever inserted. The final assertion under test was then satisfied
/// by the WARM-UP's incidental retire, not by the store under test: the mutation this row exists
/// to catch (dropping `code_write_watched`'s `range_hits_translation_page` disjunct) could never
/// make it fail, because the row never observed the mutated predicate. Verified by hand: with
/// that disjunct forced off, this row was still green before the fix below.
///
/// Fixed by ORDER: run the throwaway store FIRST, before any decode has run, so its walk's marks
/// land with nothing live to retire. Decode the witness next (its own walk DOES retire against
/// the now-marked table page -- F11's placement correction doing its job, not a bug; the line is
/// inserted afterward and `warm_witness`'s own assertion is the proof it survived). Only THEN
/// take the store under test, on an ALREADY-WARM translation for linear page 9 (the throwaway
/// store's own walk set the page's TLB/FastMap entry dirty), so `translation_a_stores` /
/// `translation_d_stores` not moving across it is this row's own proof that it took no walk and
/// that `code_write_watched`'s `range_hits_translation_page` disjunct is the only door left open
/// to it.
#[test]
fn pte_edit_with_a_tlb_warm_target_still_retires() {
    let (mut cpu, mut bus) = cr3_fixture();
    let d = cpu.registers.cs().default_size_32;

    // Throwaway: page 14's PTE slot, same table page as the witness's PTE but an entry the
    // witness never reads. No decode has happened yet, so this walk's own A/D stores land on an
    // UNMARKED table page and do not retire anything on their OWN account. The CONTENT write
    // that follows the walk lands in the table page the walk just marked, so it is itself a
    // `code_write_watched`-caught translation-page write (measured, not assumed: this table is
    // one of the identity-mapped low pages, so "write into the page holding your own PTE" is a
    // self-referential shape and cannot avoid marking itself first) -- which is fine; nothing
    // downstream depends on this store staying silent, only on the STORE UNDER TEST below doing
    // so.
    let throwaway_pte = 0x9000 + 14 * 4;
    let throwaway_value = (14u32 << 12) | 3;
    cpu.write_memory_sized(
        &mut bus,
        SegmentIndex::Ds,
        throwaway_pte,
        OperandSize::Dword,
        throwaway_value,
        BusAccessKind::DataWrite,
    )
    .expect("the warm-up store must retire");

    // NOW decode the witness -- AFTER the warm-up, so its walk touches an ALREADY-marked table
    // page and retires there, and the line inserted afterward is the one the store under test
    // must kill.
    warm_witness(&mut cpu, &mut bus);

    // The store under test: page 0's PTE at 0x9000, rewritten to ALT_FRAME | 3. Its own
    // translation (linear page 9) reuses the warm-up's already-dirty TLB/FastMap entry for that
    // page, so it takes no walk of its own -- proved by `translation_a_stores` /
    // `translation_d_stores` not moving across it, below.
    let a_stores_before = cpu.perf_counters().translation_a_stores;
    let d_stores_before = cpu.perf_counters().translation_d_stores;
    let writes_before = cpu.perf_counters().translation_page_writes;
    let witness_pte = 0x9000 + (WITNESS >> 12) * 4;
    let new_pte = ALT_FRAME | 3;
    cpu.write_memory_sized(
        &mut bus,
        SegmentIndex::Ds,
        witness_pte,
        OperandSize::Dword,
        new_pte,
        BusAccessKind::DataWrite,
    )
    .expect("the store under test must retire");
    assert_eq!(
        cpu.perf_counters().translation_a_stores,
        a_stores_before,
        "the store under test must take no walk of its own: code_write_watched, not \
         write_page_walk_entry, is what must catch it"
    );
    assert_eq!(
        cpu.perf_counters().translation_d_stores,
        d_stores_before,
        "same proof, the dirty-bit half"
    );

    assert!(
        !cpu.decode_cache.line_live(WITNESS, d),
        "code_write_watched's translation_pages disjunct, alone, must catch this store"
    );
    assert_eq!(
        cpu.perf_counters().translation_page_writes,
        writes_before + 1,
        "exactly one translation-page WRITE happened: the store under test"
    );
}

/// The VCPI server's exact shape: edit A's table while B is selected, then return to A. Option 1
/// passes every row above and fails this one -- editing under a DIFFERENT live directory is
/// exactly what same-value elision cannot see.
#[test]
fn cross_context_pte_edit_is_observed_on_return() {
    let (mut cpu, mut bus) = cr3_fixture();
    warm_witness(&mut cpu, &mut bus);
    let d = cpu.registers.cs().default_size_32;

    write_cr3(&mut cpu, &mut bus, DIR_B);
    let witness_pte = 0x9000 + (WITNESS >> 12) * 4;
    let new_pte = ALT_FRAME | 3;
    cpu.write_memory_sized(
        &mut bus,
        SegmentIndex::Ds,
        witness_pte,
        OperandSize::Dword,
        new_pte,
        BusAccessKind::DataWrite,
    )
    .expect("the guest PTE store must retire");

    write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
    assert!(
        !cpu.decode_cache.line_live(WITNESS, d),
        "A's table moved while B was selected; A's old line must not resurrect"
    );
}

/// R4, the row Revision 1 did not have. A store into a byte carrying a live line under context A,
/// made while B is selected, must not survive: `narrow_invalidate` refuses whenever more than one
/// context is live, forcing the wholesale fallback that retires BOTH slots. This has nothing to
/// do with CR3 semantics -- it is an ordinary SMC store -- which is exactly why it was missed.
#[test]
fn cross_context_smc_store_kills_the_other_context() {
    let (mut cpu, mut bus) = cr3_fixture();
    warm_witness(&mut cpu, &mut bus);
    let d = cpu.registers.cs().default_size_32;

    write_cr3(&mut cpu, &mut bus, DIR_B);
    // Store new bytes over the witness INSTRUCTION while B is selected, through the guest write
    // path (a self-modifying-code store, not a page-table edit).
    cpu.write_memory_u8(
        &mut bus,
        SegmentIndex::Ds,
        WITNESS,
        ALT_WITNESS_BYTE,
        BusAccessKind::DataWrite,
    )
    .expect("the SMC store must retire");

    write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
    assert!(
        !cpu.decode_cache.line_live(WITNESS, d),
        "the cross-context SMC store must have killed A's line too"
    );
    cpu.fetch_decoded(&mut bus, WITNESS)
        .expect("re-decode must see the patched byte, not error on stale decode state");
}

/// Finding F11 (blocking): a TLB entry filled by a DATA access is reused by a later CODE
/// translation with no walk, so a code-only marking rule would leave the line with no mark
/// behind it. Fill the TLB with a data access, THEN insert a decode line on that page, THEN edit
/// its PTE through the guest write path: the line must die.
#[test]
fn a_tlb_hit_code_translation_is_still_protected() {
    let (mut cpu, mut bus) = cr3_fixture();
    let d = cpu.registers.cs().default_size_32;

    // A data access to the witness's OWN page fills the TLB without ever inserting a decode
    // line: this is the walk that must mark the structure pages, since the later code fetch
    // will not.
    cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, WITNESS, BusAccessKind::DataRead)
        .expect("the data read must fill the TLB");

    // Now decode the witness: `translate_code_linear` hits the TLB fast path and returns with no
    // walk (design part (a)), so if marking were code-only, nothing would protect this line.
    cpu.fetch_decoded(&mut bus, WITNESS)
        .expect("witness decodes off the TLB-filled page");
    assert!(cpu.decode_cache.line_live(WITNESS, d));

    let witness_pte = 0x9000 + (WITNESS >> 12) * 4;
    let new_pte = ALT_FRAME | 3;
    cpu.write_memory_sized(
        &mut bus,
        SegmentIndex::Ds,
        witness_pte,
        OperandSize::Dword,
        new_pte,
        BusAccessKind::DataWrite,
    )
    .expect("the PTE store must retire");
    assert!(
        !cpu.decode_cache.line_live(WITNESS, d),
        "a TLB-hit code translation must still be protected by the DATA walk's marks (F11)"
    );
}

/// A third distinct CR3 value with both slots occupied retires everything -- today's behaviour --
/// and then occupies slot 0 with the new value.
#[test]
fn a_third_directory_retires_everything() {
    let (mut cpu, mut bus) = cr3_fixture();
    warm_witness(&mut cpu, &mut bus);
    let d = cpu.registers.cs().default_size_32;
    write_cr3(&mut cpu, &mut bus, DIR_B);
    let before = cpu.perf_counters().clone();

    write_cr3(&mut cpu, &mut bus, DIR_C);

    assert!(
        !cpu.decode_cache.line_live(WITNESS, d),
        "a third distinct directory must retire everything"
    );
    assert_eq!(
        cpu.perf_counters().cr3_code_flush_taken,
        before.cr3_code_flush_taken + 1
    );
    assert_eq!(
        cpu.perf_counters().decode_inval_other,
        before.decode_inval_other + 1,
        "a taken (full-teardown) write must land in the wholesale aggregate"
    );

    // Returning to A (now evicted) must allocate fresh, not resurrect the pre-teardown line.
    write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY);
    assert!(
        !cpu.decode_cache.line_live(WITNESS, d),
        "A was evicted by the third value; its old generation is gone"
    );
}

/// `cr3_code_flush_taken + cr3_code_flush_skipped == decode_inval_cr3` is an identity for every
/// write that goes through the ring-gated path (Part 1's `decode_inval_cr3` doc comment, amended).
#[test]
fn taken_plus_skipped_equals_decode_inval_cr3() {
    let (mut cpu, mut bus) = cr3_fixture();
    warm_witness(&mut cpu, &mut bus);
    for target in [DIR_B, PAGE_DIRECTORY, DIR_C, DIR_B, PAGE_DIRECTORY] {
        write_cr3(&mut cpu, &mut bus, target);
    }
    let perf = cpu.perf_counters();
    assert_eq!(
        perf.cr3_code_flush_taken + perf.cr3_code_flush_skipped,
        perf.decode_inval_cr3
    );
    assert!(perf.decode_inval_cr3 > 0);
}

/// Bar C's divergence check: `wipes_tlb_flush` keeps counting every `MOV CR3` EVENT, ring hit or
/// not -- the data half (TLB, FastMap, `data_read_pages`/`data_write_pages`) stays unconditional.
#[cfg(feature = "jit")]
#[test]
fn wipes_tlb_flush_counts_every_cr3_write_ring_hit_or_not() {
    let (mut cpu, mut bus) = cr3_fixture();
    warm_witness(&mut cpu, &mut bus);
    let before = cpu.fast_map_audit_counters().wipes_tlb_flush;

    write_cr3(&mut cpu, &mut bus, DIR_B); // R2 allocate: skipped
    write_cr3(&mut cpu, &mut bus, PAGE_DIRECTORY); // R1 select: skipped

    assert_eq!(
        cpu.fast_map_audit_counters().wipes_tlb_flush,
        before + 2,
        "both writes are TLB-flush EVENTS regardless of the ring outcome"
    );
}

// MUTATION LEDGER, CR3 code-cache gate (2026-09-02):
//
// | mutation | must go red | verified |
// |---|---|---|
// | force the fast arm unconditionally (never retire on R3) | `a_third_directory_retires_everything` | by hand |
// | drop `range_hits_translation_page` from `code_write_watched` | **FIXED, no longer a
//   non-gate** (design doc `2026-09-02-cr3-jit-half-design.md` (e)). The original
//   `pte_edit_with_a_tlb_warm_target_still_retires` ran its warm-up BEFORE the witness decode,
//   so `warm_witness`'s own walk (not the store under test) did the only retire the row ever
//   observed -- the row's final assertion was satisfied by an ORDERING accident, not by the
//   mechanism it was built to isolate. Reordered so the warm-up runs FIRST (marking the table
//   page with nothing live to retire) and the witness decodes SECOND (retiring there, against
//   the now-marked page -- F11's placement doing its job), leaving the STORE UNDER TEST as the
//   only remaining door: `translation_a_stores`/`translation_d_stores` asserted unchanged across
//   it proves it takes no walk of its own. Verified by hand: forcing `code_write_watched`'s
//   `range_hits_translation_page` disjunct to `false` now reddens this row (and only this row;
//   the other ten still pass through `write_page_walk_entry`'s independent unconditional path,
//   exactly as the original ledger entry for the un-warmed rows described), reverted after. |
// | `narrow_invalidate` does not refuse with two contexts occupied | `cross_context_smc_store_kills_the_other_context` | by hand |
// | mark only on a CODE-producing walk, not every walk | `a_tlb_hit_code_translation_is_still_protected` | by hand |
// | `translation_pages` cleared on a select (R7) | `pte_edit_under_a_live_cr3_forces_a_redecode` after a prior A/B/A cycle | by hand |
// | generation allocator not shared between the ring and the plain bump (R6) | flagged by
//   `cpu_persona_system_test.rs::decode_cache_generation_wrap_clears_lines_and_watches` once the
//   ring has a live slot; not re-proven here to avoid duplicating that fixture |
// | R8 seed omitted (first select allocates instead of seeding) | `returning_to_a_directory_restores_its_decode_lines` | by hand |
// | ring select does not adopt the OLD cr3 for the seed | `returning_to_a_directory_restores_its_decode_lines` | by hand |
// | `wipes_tlb_flush` made conditional on the ring outcome | `wipes_tlb_flush_counts_every_cr3_write_ring_hit_or_not` | by hand |
// | **NON-GATE:** a ring lookup that masks the key with the PERSONA mask (0xffff_f018) instead of
//   `0xffff_f000` is inert on every row here, because none of these directories set PWT/PCD bits.
//   It would need a dedicated PCD/PWT row to catch, which this file does not add (advisory 17 is
//   satisfied by inspection: `select_context`'s callers mask with `0xffff_f000` explicitly).

#[path = "cpu_cr3_data_flush_test.rs"]
mod cr3_data_flush;
