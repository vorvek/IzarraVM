// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const READ: bool = false;
const WRITE: bool = true;

fn line_addr(line: u32) -> u32 {
    line << LINE_SHIFT
}

/// Known-pattern proof of the 4-way pseudo-LRU replacement: fill one set,
/// evict its pseudo-LRU victim, and confirm the victim -- not just "a"
/// miss -- is exactly the one Intel's 3-bit tree algorithm predicts. Every
/// access here is a READ, so every miss allocates (no-write-allocate is
/// exercised separately below).
///
/// Four lines alias into set 0 (line numbers 128 apart, since `SETS` = 128),
/// then a fifth forces an eviction. Every step below is hand-traced against
/// `plru_victim`/`plru_touch`, not just re-derived from the code under test:
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
    let mut tags = ShadowTags::new();
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
        let hit = tags.probe(line_addr(line), READ);
        assert_eq!(
            hit, expect_hit,
            "step {step}: probing line {line} expected hit={expect_hit}, got {hit}"
        );
    }
}

/// A write MISS must install nothing (i486 DX2 Data Book Sec 5.3: "cache
/// allocations are not made on write misses"). A read of the same line must
/// still miss afterward, proving the write miss installed nothing.
#[test]
fn write_miss_does_not_allocate() {
    let mut tags = ShadowTags::new();
    assert!(!tags.probe(line_addr(0), WRITE), "write miss must miss");
    assert!(
        !tags.probe(line_addr(0), WRITE),
        "a write miss must not have installed the line: the same write misses again"
    );
    assert!(
        !tags.probe(line_addr(0), READ),
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
fn write_hit_touches_lru_like_a_read_hit() {
    let mut tags = ShadowTags::new();
    assert!(
        !tags.probe(line_addr(0), READ),
        "cold miss installs line 0 at way 0"
    );
    assert!(
        !tags.probe(line_addr(128), READ),
        "installs line 128 at way 2"
    );
    assert!(
        !tags.probe(line_addr(256), READ),
        "installs line 256 at way 1"
    );
    assert!(
        !tags.probe(line_addr(384), READ),
        "installs line 384 at way 3"
    );
    // All four ways are now full and PLRU bits are 0b000 (matching the
    // read-only sequence's step 4). A WRITE HIT on line 0 is the touch under
    // test: it must move the PLRU bits exactly as a read hit would.
    assert!(
        tags.probe(line_addr(0), WRITE),
        "a write to an already-resident line is a hit"
    );
    // A 5th line now evicts way 2 (line 128), the same victim the read-only
    // sequence's step 6 evicts, because the write hit updated PLRU
    // identically to a read hit would have.
    assert!(!tags.probe(line_addr(512), READ), "5th line misses");
    assert!(
        tags.probe(line_addr(0), READ),
        "line 0 must have survived: the write hit kept it out of the victim path"
    );
    assert!(
        !tags.probe(line_addr(128), READ),
        "line 128 (way 2) is the one that should have been evicted"
    );
}

#[test]
fn flush_clears_tags_and_lru() {
    let mut tags = ShadowTags::new();
    assert!(
        !tags.probe(line_addr(0), READ),
        "cold miss installs the line"
    );
    assert!(tags.probe(line_addr(0), READ), "resident line is now a hit");
    tags.flush();
    assert!(
        !tags.probe(line_addr(0), READ),
        "a flushed line must miss again"
    );
}

#[test]
fn disabled_probe_counts_nothing() {
    let mut probe = ShadowL1Probe {
        tags: ShadowTags::new(),
        counts: [ShadowClassCounts::default(); CLASS_COUNT],
        enabled: false,
    };
    probe.probe(ShadowAccessClass::DataRead, 0);
    probe.probe(ShadowAccessClass::DataRead, 0x1_0000);
    let diag = probe.diagnostics();
    assert_eq!(diag.data_read, ShadowClassCounts::default());
}

#[test]
fn enabled_probe_splits_by_class() {
    let mut probe = ShadowL1Probe {
        tags: ShadowTags::new(),
        counts: [ShadowClassCounts::default(); CLASS_COUNT],
        enabled: true,
    };
    probe.probe(ShadowAccessClass::CodeFetch, 0);
    probe.probe(ShadowAccessClass::CodeFetch, 0); // same line: hit
    probe.probe(ShadowAccessClass::DataRead, 0x2000);
    probe.probe(ShadowAccessClass::DataWrite, 0x4000);
    let diag = probe.diagnostics();
    assert_eq!(diag.code_fetch, ShadowClassCounts { hits: 1, misses: 1 });
    assert_eq!(diag.data_read, ShadowClassCounts { hits: 0, misses: 1 });
    // A write miss counts as a miss but installs nothing.
    assert_eq!(diag.data_write, ShadowClassCounts { hits: 0, misses: 1 });
}

/// The write-allocate fix must not corrupt the write counters themselves: a
/// write hit still counts as a hit.
#[test]
fn write_hit_counts_as_a_hit() {
    let mut probe = ShadowL1Probe {
        tags: ShadowTags::new(),
        counts: [ShadowClassCounts::default(); CLASS_COUNT],
        enabled: true,
    };
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
fn enabled_probe_flush_clears_every_class() {
    let mut probe = ShadowL1Probe {
        tags: ShadowTags::new(),
        counts: [ShadowClassCounts::default(); CLASS_COUNT],
        enabled: true,
    };
    probe.probe(ShadowAccessClass::CodeFetch, 0);
    probe.flush();
    // The tag array is cold again, but counters are cumulative and must not
    // be reset by a flush (a real cache flush does not erase perf counters;
    // it only invalidates cached lines).
    probe.probe(ShadowAccessClass::CodeFetch, 0);
    let diag = probe.diagnostics();
    assert_eq!(diag.code_fetch, ShadowClassCounts { hits: 0, misses: 2 });
}
