// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const READ: bool = false;
const WRITE: bool = true;

/// 486 unified geometry: 16-byte lines.
fn line_addr_486(line: u32) -> u32 {
    line << 4
}

/// 586 split geometry: 32-byte lines.
fn line_addr_586(line: u32) -> u32 {
    line << 5
}

fn probe_for(persona: CpuPersona) -> ShadowL1Probe {
    ShadowL1Probe {
        persona,
        arrays: ShadowArrays::for_persona(persona),
        enabled: true,
    }
}

/// Known-pattern proof of the 4-way pseudo-LRU replacement: fill one set,
/// evict its pseudo-LRU victim, and confirm the victim -- not just "a"
/// miss -- is exactly the one Intel's 3-bit tree algorithm predicts. Every
/// access here is a READ, so every miss allocates (no-write-allocate is
/// exercised separately below).
///
/// Four lines alias into set 0 (line numbers 128 apart, since the 486 arm has
/// 128 sets), then a fifth forces an eviction. Every step below is
/// hand-traced against `plru4_victim`/`plru4_touch`, not just re-derived from
/// the code under test:
///
/// 1. probe(L0)   MISS  bits 0b000 -> victim way0        -> bits 0b011
/// 2. probe(L128) MISS  bits 0b011 -> victim way2        -> bits 0b110
/// 3. probe(L256) MISS  bits 0b110 -> victim way1        -> bits 0b101
/// 4. probe(L384) MISS  bits 0b101 -> victim way3        -> bits 0b000
/// 5. probe(L0)   HIT   (way0)                           -> bits 0b011
/// 6. probe(L512) MISS  bits 0b011 -> victim way2 (L128 evicted) -> bits 0b110
/// 7. probe(L256) HIT   (way1, untouched by the eviction) -> bits 0b101
/// 8. probe(L128) MISS  (evicted at step 6, so this proves the eviction
///    target -- not merely that some slot missed)
#[test]
fn four_way_pseudo_lru_matches_hand_traced_sequence() {
    let mut tags = ShadowTags::new_i486_unified();
    let sequence = [
        (0u32, false),
        (128, false),
        (256, false),
        (384, false),
        (0, true),
        (512, false),
        (256, true),
        (128, false),
    ];
    for (step, (line, expect_hit)) in sequence.into_iter().enumerate() {
        let hit = tags.probe(line_addr_486(line), READ);
        assert_eq!(
            hit, expect_hit,
            "step {step}: probing line {line} expected hit={expect_hit}, got {hit}"
        );
    }
}

/// A write MISS must install nothing on the 486's write-through arm (i486 DX2
/// Data Book Sec 5.3: "cache allocations are not made on write misses"). A
/// read of the same line must still miss afterward, proving the write miss
/// installed nothing.
#[test]
fn write_miss_does_not_allocate_on_486() {
    let mut tags = ShadowTags::new_i486_unified();
    assert!(!tags.probe(line_addr_486(0), WRITE), "write miss must miss");
    assert!(
        !tags.probe(line_addr_486(0), WRITE),
        "a write miss must not have installed the line: the same write misses again"
    );
    assert!(
        !tags.probe(line_addr_486(0), READ),
        "a write miss must not have installed the line: a read still misses"
    );
}

/// A write HIT still touches LRU (only a write MISS skips allocation): the
/// hand-traced fill order is the same as
/// `four_way_pseudo_lru_matches_hand_traced_sequence` (l0/l128/l256/l384 land
/// in ways 0/2/1/3 from cold, ending with PLRU bits `0b000`), but the
/// protecting touch here is a WRITE HIT on line 0 rather than a read, and a
/// 5th line then evicts way 2 (line 128) rather than way 0 -- proving the
/// write hit moved the PLRU state exactly like a read hit would have.
#[test]
fn write_hit_touches_lru_like_a_read_hit_on_486() {
    let mut tags = ShadowTags::new_i486_unified();
    assert!(
        !tags.probe(line_addr_486(0), READ),
        "cold miss installs line 0 at way 0"
    );
    assert!(
        !tags.probe(line_addr_486(128), READ),
        "installs line 128 at way 2"
    );
    assert!(
        !tags.probe(line_addr_486(256), READ),
        "installs line 256 at way 1"
    );
    assert!(
        !tags.probe(line_addr_486(384), READ),
        "installs line 384 at way 3"
    );
    assert!(
        tags.probe(line_addr_486(0), WRITE),
        "a write to an already-resident line is a hit"
    );
    assert!(!tags.probe(line_addr_486(512), READ), "5th line misses");
    assert!(
        tags.probe(line_addr_486(0), READ),
        "line 0 must have survived: the write hit kept it out of the victim path"
    );
    assert!(
        !tags.probe(line_addr_486(128), READ),
        "line 128 (way 2) is the one that should have been evicted"
    );
    assert_eq!(
        tags.write_back_victims, 0,
        "the 486 arm is write-through: it never counts a write-back"
    );
}

#[test]
fn flush_clears_tags_and_lru_on_486() {
    let mut tags = ShadowTags::new_i486_unified();
    assert!(
        !tags.probe(line_addr_486(0), READ),
        "cold miss installs the line"
    );
    assert!(
        tags.probe(line_addr_486(0), READ),
        "resident line is now a hit"
    );
    tags.flush();
    assert!(
        !tags.probe(line_addr_486(0), READ),
        "a flushed line must miss again"
    );
}

// ---------------------------------------------------------------------------
// S1a: 586 split-array geometry (2-way, 32-byte lines, 256 sets, 1-bit LRU,
// write-allocate on the data array).
// ---------------------------------------------------------------------------

/// A stride thrashing exactly 2 ways of one set: the third distinct line
/// aliasing into that set must evict the LRU one (S1a's certifier, first
/// half). Two lines 256 sets apart alias into set 0 on the 586 split
/// geometry; a third line another 256 sets on must evict the first (the
/// least-recently-touched way), leaving the second resident.
#[test]
fn two_way_lru_evicts_the_least_recently_used_line() {
    let mut tags = ShadowTags::new_i586_split(false);
    assert!(
        !tags.probe(line_addr_586(0), READ),
        "cold miss installs L0 at way0, victim bit -> way1"
    );
    assert!(
        !tags.probe(line_addr_586(256), READ),
        "cold miss installs L256 at way1, victim bit -> way0"
    );
    // Touching L256 again would flip the victim back to way0, so instead
    // touch L0 to confirm true-LRU tracks the ACCESS order, not install
    // order: L0 is now the most-recently-used, so the next miss must evict
    // L256 (way1), not L0 (way0).
    assert!(tags.probe(line_addr_586(0), READ), "L0 is resident: hit");
    assert!(
        !tags.probe(line_addr_586(512), READ),
        "third distinct line in the set misses and evicts way1 (L256, the LRU one)"
    );
    assert!(
        tags.probe(line_addr_586(0), READ),
        "L0 (way0) must have survived: it was the more-recently-used line"
    );
    assert!(
        !tags.probe(line_addr_586(256), READ),
        "L256 is the one that should have been evicted"
    );
}

/// S1a's certifier, second half: a write miss now installs on 586 (write-
/// allocate) and still does not on 486 (write-through, already proved above
/// by `write_miss_does_not_allocate_on_486`).
#[test]
fn write_miss_allocates_on_586_data_array() {
    let mut tags = ShadowTags::new_i586_split(true);
    assert!(
        !tags.probe(line_addr_586(0), WRITE),
        "write miss must still report a miss"
    );
    assert!(
        tags.probe(line_addr_586(0), READ),
        "but the write-allocate arm must have installed the line: a read now hits"
    );
}

/// The line the write-allocate arm installs on a write miss is DIRTY (the
/// data was just modified), so evicting it later must count a write-back.
#[test]
fn write_miss_installs_a_dirty_line_that_write_backs_on_eviction() {
    let mut tags = ShadowTags::new_i586_split(true);
    assert!(
        !tags.probe(line_addr_586(0), WRITE),
        "write miss installs L0, dirty"
    );
    assert!(
        !tags.probe(line_addr_586(256), WRITE),
        "write miss installs L256 (aliases L0's set), dirty"
    );
    // Two ways, both now dirty and full; a third distinct line in the set
    // forces an eviction of whichever is LRU (L0, installed first and never
    // touched again).
    assert_eq!(tags.write_back_victims, 0, "no eviction has happened yet");
    assert!(
        !tags.probe(line_addr_586(512), READ),
        "third line misses and evicts the dirty L0"
    );
    assert_eq!(
        tags.write_back_victims, 1,
        "evicting a dirty line must count exactly one write-back"
    );
}

/// A CLEAN eviction (a read-miss-installed line, never written) must NOT
/// count a write-back.
#[test]
fn clean_eviction_does_not_write_back() {
    let mut tags = ShadowTags::new_i586_split(true);
    assert!(
        !tags.probe(line_addr_586(0), READ),
        "read miss installs L0, clean"
    );
    assert!(
        !tags.probe(line_addr_586(256), READ),
        "read miss installs L256, clean"
    );
    assert!(
        !tags.probe(line_addr_586(512), READ),
        "evicts the LRU clean line"
    );
    assert_eq!(
        tags.write_back_victims, 0,
        "a clean line's eviction is a silent discard, not a write-back"
    );
}

/// A write HIT dirties an already-resident line, so it too write-backs on a
/// later eviction (not just a write-miss-installed line).
#[test]
fn write_hit_dirties_a_clean_line() {
    let mut tags = ShadowTags::new_i586_split(true);
    assert!(
        !tags.probe(line_addr_586(0), READ),
        "read miss installs L0, clean"
    );
    assert!(
        tags.probe(line_addr_586(0), WRITE),
        "write hit on L0: dirties it"
    );
    assert!(
        !tags.probe(line_addr_586(256), READ),
        "read miss installs L256, clean"
    );
    assert!(
        !tags.probe(line_addr_586(512), READ),
        "evicts the LRU line (L0, dirty from the write hit)"
    );
    assert_eq!(
        tags.write_back_victims, 1,
        "the write-hit-dirtied line must write back when it is evicted"
    );
}

// ---------------------------------------------------------------------------
// S1c: L2 shadow, probed only on an L1 miss.
// ---------------------------------------------------------------------------

/// An L1-thrash / L2-resident stride: two lines alias into the same 586 L1
/// data set (256 sets) but land in DIFFERENT L2 sets (4096 sets), so as the
/// L1 set thrashes between them every access after the first two misses at
/// L1 -- while L2, having room for both, keeps missing only on their FIRST
/// arrival and then hits every time. This is S1c's certifier: "L1 misses
/// rising with L2 misses flat".
#[test]
fn l1_thrash_l2_resident_certifier() {
    let persona = CpuPersona::I586;
    let mut probe = probe_for(persona);
    // Three lines, all 256 lines apart, all alias into L1 data set 0 (256
    // sets: 0, 256, 512 all reduce to set 0). At L2 (4096 sets) they land in
    // sets 0, 256 and 512 respectively -- three DIFFERENT L2 sets, so L2
    // never even has to evict across this whole sequence.
    let lines = [0u32, 256, 512];
    let addrs: Vec<u32> = lines.iter().map(|&l| line_addr_586(l)).collect();
    // Round 1: every access is a cold miss at both L1 and L2.
    for &addr in &addrs {
        probe.probe(ShadowAccessClass::DataRead, addr);
    }
    let after_round1 = probe.diagnostics();
    assert_eq!(
        after_round1.data_read.misses, 3,
        "round 1: all three cold-miss at L1"
    );
    assert_eq!(
        after_round1.l2.data_read.misses, 3,
        "round 1: all three cold-miss at L2 too"
    );
    // Round 2: the 2-way L1 set can hold only 2 of the 3 lines, so this
    // round L1 thrashes (every access misses -- the set can never contain
    // all three), but L2 (4096 sets, no collision among these three) now
    // HITS on all three, since the L1 miss probes L2 lazily.
    for &addr in &addrs {
        probe.probe(ShadowAccessClass::DataRead, addr);
    }
    let after_round2 = probe.diagnostics();
    assert_eq!(
        after_round2.data_read.misses, 6,
        "L1 misses keep rising: the 2-way set cannot hold all three lines"
    );
    assert_eq!(
        after_round2.l2.data_read.misses, 3,
        "L2 misses stay FLAT at 3: every L2 access in round 2 hits (already resident, \
         different L2 sets, no collision there)"
    );
}

/// A dirty L1 eviction that in turn evicts a dirty L2 line increments the L2
/// `write_back_victims`; a clean L2 eviction does not.
#[test]
fn l2_write_back_victims_only_on_a_dirty_l2_eviction() {
    let mut l2 = ShadowTags::new_i586_l2();
    // Fill 4 ways of L2 set 0 (4-way): lines 0, 4096, 8192, 12288 (4096 sets).
    assert!(
        !l2.probe(line_addr_586(0), WRITE),
        "write miss installs dirty"
    );
    assert!(
        !l2.probe(line_addr_586(4096), READ),
        "read miss installs clean"
    );
    assert!(
        !l2.probe(line_addr_586(8192), READ),
        "read miss installs clean"
    );
    assert!(
        !l2.probe(line_addr_586(12288), READ),
        "read miss installs clean, PLRU now full"
    );
    assert_eq!(l2.write_back_victims, 0);
    // A 5th line forces an eviction. Whichever way PLRU picks, hand-trace it
    // via the SAME sequence `four_way_pseudo_lru_matches_hand_traced_sequence`
    // uses: fill order way0,way2,way1,way3 from cold -> PLRU bits end at
    // 0b000 -> the next victim is way0 (line 0, the dirty one).
    assert!(
        !l2.probe(line_addr_586(16384), READ),
        "5th line evicts way0 (line 0, dirty)"
    );
    assert_eq!(
        l2.write_back_victims, 1,
        "evicting the dirty line must write back"
    );
}

// ---------------------------------------------------------------------------
// Class routing, disabled probe, flush, and the top-level ShadowL1Probe API.
// ---------------------------------------------------------------------------

#[test]
fn disabled_probe_counts_nothing() {
    let mut probe = probe_for(CpuPersona::I486);
    probe.enabled = false;
    probe.probe(ShadowAccessClass::DataRead, 0);
    probe.probe(ShadowAccessClass::DataRead, 0x1_0000);
    let diag = probe.diagnostics();
    assert_eq!(diag.data_read, ShadowClassCounts::default());
    assert!(!diag.enabled);
    assert_eq!(diag.persona, None, "persona is not reported while disabled");
}

#[test]
fn enabled_probe_splits_by_class_on_486() {
    let mut probe = probe_for(CpuPersona::I486);
    probe.probe(ShadowAccessClass::CodeFetch, 0);
    probe.probe(ShadowAccessClass::CodeFetch, 0); // same line: hit
    probe.probe(ShadowAccessClass::DataRead, 0x2000);
    probe.probe(ShadowAccessClass::DataWrite, 0x4000);
    let diag = probe.diagnostics();
    assert_eq!(diag.code_fetch, ShadowClassCounts { hits: 1, misses: 1 });
    assert_eq!(diag.data_read, ShadowClassCounts { hits: 0, misses: 1 });
    // A write miss counts as a miss but installs nothing (486 write-through).
    assert_eq!(diag.data_write, ShadowClassCounts { hits: 0, misses: 1 });
    assert_eq!(diag.write_back_victims, 0);
    assert_eq!(diag.persona, Some(CpuPersona::I486));
}

#[test]
fn enabled_probe_splits_by_class_on_586() {
    let mut probe = probe_for(CpuPersona::I586);
    probe.probe(ShadowAccessClass::CodeFetch, 0);
    probe.probe(ShadowAccessClass::CodeFetch, 0); // same line: hit, instruction array
    probe.probe(ShadowAccessClass::DataRead, 0x2000);
    probe.probe(ShadowAccessClass::DataWrite, 0x4000);
    let diag = probe.diagnostics();
    assert_eq!(diag.code_fetch, ShadowClassCounts { hits: 1, misses: 1 });
    assert_eq!(diag.data_read, ShadowClassCounts { hits: 0, misses: 1 });
    // A write MISS on the 586 data array allocates (write-allocate), unlike
    // 486, but still counts as a miss.
    assert_eq!(diag.data_write, ShadowClassCounts { hits: 0, misses: 1 });
    assert_eq!(diag.persona, Some(CpuPersona::I586));
}

#[test]
fn write_hit_counts_as_a_hit() {
    let mut probe = probe_for(CpuPersona::I486);
    probe.probe(ShadowAccessClass::DataRead, 0); // installs the line
    probe.probe(ShadowAccessClass::DataWrite, 0); // write hit on that line
    let diag = probe.diagnostics();
    assert_eq!(diag.data_write, ShadowClassCounts { hits: 1, misses: 0 });
}

#[test]
fn shadow_class_for_maps_page_walks_into_data_classes() {
    assert_eq!(
        shadow_class_for(BusAccessKind::PageWalkRead),
        Some(ShadowAccessClass::DataRead)
    );
    assert_eq!(
        shadow_class_for(BusAccessKind::PageWalkWrite),
        Some(ShadowAccessClass::DataWrite)
    );
    assert_eq!(shadow_class_for(BusAccessKind::IoRead), None);
    assert_eq!(shadow_class_for(BusAccessKind::IoWrite), None);
    assert_eq!(shadow_class_for(BusAccessKind::InterruptAcknowledge), None);
}

#[test]
fn enabled_probe_flush_clears_every_class_but_not_counters() {
    let mut probe = probe_for(CpuPersona::I486);
    probe.probe(ShadowAccessClass::CodeFetch, 0);
    probe.flush();
    // The tag array is cold again, but counters are cumulative and must not
    // be reset by a flush (a real cache flush does not erase perf counters;
    // it only invalidates cached lines).
    probe.probe(ShadowAccessClass::CodeFetch, 0);
    let diag = probe.diagnostics();
    assert_eq!(diag.code_fetch, ShadowClassCounts { hits: 0, misses: 2 });
}

/// A persona change re-selects the geometry (and starts fully cold): a line
/// resident under 486 must not "hit" after switching to 586, and the class
/// counters reset to a fresh persona's zero state (this mirrors
/// `CacheModel::set_mode`'s cold-start-per-mode behavior, not a mid-run
/// counter carry).
#[test]
fn persona_switch_reselects_geometry_and_starts_cold() {
    let mut probe = ShadowL1Probe::from_env(CpuPersona::I486);
    probe.enabled = true;
    probe.probe(ShadowAccessClass::DataRead, 0);
    assert_eq!(probe.diagnostics().data_read.misses, 1);
    probe.set_persona_and_flush(CpuPersona::I586);
    assert_eq!(probe.diagnostics().persona, Some(CpuPersona::I586));
    let diag = probe.diagnostics();
    assert_eq!(
        diag.data_read,
        ShadowClassCounts::default(),
        "a persona switch is a different part: counters restart at zero"
    );
}

/// Knob-off vs knob-on: with the probe disabled, `wants_native_fetch_trace`
/// must read false (the JIT keeps its normal aggregate bulk-charge shape),
/// and with it enabled, true (S1b's coverage hook).
#[test]
fn wants_native_fetch_trace_follows_the_enable_bit() {
    let mut probe = probe_for(CpuPersona::I586);
    assert!(probe.wants_native_fetch_trace());
    probe.enabled = false;
    assert!(!probe.wants_native_fetch_trace());
}
