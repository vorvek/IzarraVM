// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn sector(byte: u8) -> [u8; SECTOR] {
    [byte; SECTOR]
}

/// The basic contract: a miss then a hit, and the hit returns what was stored.
///
/// NON-VACUOUS: deleting the `self.index.get` branch in `get` (always reporting a
/// miss) fails the second assertion; storing without copying `data` in `put`
/// fails the content assertion.
#[test]
fn a_stored_sector_reads_back_as_a_hit() {
    let mut cache = SectorCache::new(true);
    assert_eq!(cache.get(7), None, "cold lookup is a miss");
    cache.put(7, &sector(0xAB));
    assert_eq!(cache.get(7), Some(sector(0xAB)), "warm lookup is a hit");
    assert_eq!(cache.hits(), 1);
    assert_eq!(cache.misses(), 1);
}

/// A write-through `put` over a resident sector replaces its bytes. This is the
/// invalidation path: the disk calls `put` after every successful `write_lba`, so
/// a stale sector can never be served.
///
/// NON-VACUOUS: dropping the `self.entries[slot].data = *data` assignment from
/// `put`'s already-resident branch returns the OLD bytes and fails.
#[test]
fn a_write_over_a_resident_sector_replaces_its_bytes() {
    let mut cache = SectorCache::new(true);
    cache.put(3, &sector(0x11));
    assert_eq!(cache.get(3), Some(sector(0x11)));
    cache.put(3, &sector(0x22));
    assert_eq!(
        cache.get(3),
        Some(sector(0x22)),
        "the second write is what a later read must see"
    );
    assert_eq!(
        cache.len(),
        1,
        "an overwrite does not consume a second slot"
    );
}

/// Eviction is strict LRU and the capacity is a hard bound. A cache that grew
/// without limit would be a host-memory leak on a large volume; one that evicted
/// by anything other than access order would make the CHARGE a function of
/// something outside the guest's history.
///
/// NON-VACUOUS: replacing the `self.tail` eviction victim with `self.head` makes
/// the refreshed sector the victim and fails the third assertion; removing the
/// `self.entries.len() < CAPACITY_SECTORS` guard fails the length assertion.
#[test]
fn eviction_is_lru_and_bounded_by_capacity() {
    let mut cache = SectorCache::new(true);
    for lba in 0..CAPACITY_SECTORS as u32 {
        cache.put(lba, &sector(lba as u8));
    }
    assert_eq!(cache.len(), CAPACITY_SECTORS, "the cache filled");

    // Refresh sector 0 so it is the MOST recent, then insert one past capacity.
    assert!(cache.get(0).is_some());
    cache.put(CAPACITY_SECTORS as u32, &sector(0xFF));

    assert_eq!(cache.len(), CAPACITY_SECTORS, "capacity is a hard bound");
    assert!(
        cache.get(0).is_some(),
        "the refreshed sector survives: eviction follows access order"
    );
    assert_eq!(
        cache.get(1),
        None,
        "the least recently used sector is the one evicted"
    );
}

/// The disabled cache must be inert: never store, never hit. This is the A/B
/// control leg, and a control leg that quietly cached would make every
/// measurement against it meaningless.
///
/// NON-VACUOUS: removing the `if !self.enabled` guard from `put` makes the second
/// `get` a hit and fails.
#[test]
fn a_disabled_cache_never_stores_and_never_hits() {
    let mut cache = SectorCache::new(false);
    cache.put(5, &sector(0x5A));
    assert_eq!(cache.get(5), None, "a disabled cache has nothing to serve");
    assert_eq!(cache.get(5), None, "and still has nothing on a second look");
    assert_eq!(cache.hits(), 0);
    assert_eq!(cache.misses(), 2, "both lookups counted as misses");
    assert_eq!(cache.len(), 0, "the put stored nothing");
}

/// Residency, and therefore the charge, is a pure function of the access
/// sequence. Two caches driven by the same sequence must agree sector for
/// sector, including on which sectors were evicted.
///
/// NON-VACUOUS: this is the determinism claim the charge model rests on. Seeding
/// eviction from anything host-side (a hash iteration order, an address, a clock)
/// makes the two runs disagree once the sequence overflows capacity.
#[test]
fn the_same_access_sequence_produces_the_same_residency() {
    // A sequence long enough to overflow capacity and force eviction decisions,
    // with re-touches so LRU order actually matters.
    let sequence: Vec<u32> = (0..CAPACITY_SECTORS as u32 + 5_000)
        .map(|i| if i % 7 == 0 { i / 3 } else { i })
        .collect();

    let run = |()| {
        let mut cache = SectorCache::new(true);
        let mut trace = Vec::with_capacity(sequence.len());
        for &lba in &sequence {
            let hit = cache.get(lba).is_some();
            if !hit {
                cache.put(lba, &sector(lba as u8));
            }
            trace.push(hit);
        }
        (trace, cache.hits(), cache.misses())
    };

    let first = run(());
    let second = run(());
    assert_eq!(
        first.0, second.0,
        "the hit/miss trace must be identical, not merely the totals"
    );
    assert_eq!((first.1, first.2), (second.1, second.2));
    assert!(
        first.1 > 0,
        "the sequence must actually produce hits, or this proves nothing"
    );
}
