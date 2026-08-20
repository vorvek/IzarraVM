// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use std::collections::HashMap as Model;

fn sector(seed: u8) -> [u8; SECTOR] {
    let mut s = [0u8; SECTOR];
    for (i, b) in s.iter_mut().enumerate() {
        *b = seed.wrapping_add(i as u8);
    }
    s
}

/// A deterministic stand-in for a random LBA stream: cheap, reproducible, and
/// spread wide enough to touch many chunks.
fn scatter(i: u32) -> u32 {
    (i.wrapping_mul(2_654_435_761)) % 200_000
}

/// The store must be byte-for-byte the `HashMap<u32, [u8; 512]>` it replaces, for
/// any interleaving of writes, rewrites and reads, across eviction. The shadow
/// model is literally the old implementation.
#[test]
fn matches_an_unbounded_map_across_eviction() {
    let mut store = SectorStore::with_capacity(16);
    let mut model: Model<u32, [u8; SECTOR]> = Model::new();

    for i in 0..600u32 {
        let lba = scatter(i);
        let data = sector(i as u8);
        store.insert(lba, &data);
        model.insert(lba, data);

        // A read of something written long ago, i.e. certainly evicted.
        if i > 20 {
            let old = scatter(i - 20);
            assert_eq!(
                store.get(old).unwrap(),
                model.get(&old).copied(),
                "evicted sector {old} must read back exactly"
            );
        }
        // Rewrite an older sector: the store must return the new bytes, not the
        // spilled ones.
        if i % 7 == 0 && i > 30 {
            let lba = scatter(i - 30);
            let data = sector((i as u8).wrapping_add(99));
            store.insert(lba, &data);
            model.insert(lba, data);
        }
    }

    for (&lba, want) in &model {
        assert_eq!(store.get(lba).unwrap(), Some(*want), "sector {lba}");
        assert!(store.was_written(lba), "presence for {lba}");
    }
    // Never-written sectors stay absent, so the caller serves the base view.
    assert_eq!(store.get(999_999).unwrap(), None);
    assert!(!store.was_written(999_999));
}

/// The issue itself: RAM must track the cache, not the write volume.
#[test]
fn ram_stays_bounded_under_a_large_write_volume() {
    let mut store = SectorStore::with_capacity(64);
    for i in 0..20_000u32 {
        store.insert(i, &sector(i as u8));
        assert!(
            store.cache_len() <= 64,
            "cache exceeded capacity at write {i}: {}",
            store.cache_len()
        );
    }

    // 20000 sectors written is 10 MiB of payload. The old overlay would hold all
    // of it; the bound here is the cache plus one Chunk per 128 KiB touched.
    let written = 20_000 * SECTOR;
    let bound = 64 * (SECTOR + 16) + 64 * 24 + (20_000 / 256 + 2) * 88;
    assert!(
        store.ram_bytes() <= bound,
        "ram {} exceeds bound {bound} after writing {written} bytes",
        store.ram_bytes()
    );
    assert!(
        store.ram_bytes() * 100 < written,
        "ram {} should be a small fraction of the {written} bytes written",
        store.ram_bytes()
    );
    // Every sector is still exactly readable.
    for i in (0..20_000u32).step_by(97) {
        assert_eq!(store.get(i).unwrap(), Some(sector(i as u8)), "sector {i}");
    }
}

/// A rewritten sector must move to the back of the eviction order, and must not
/// leave a stale entry behind. Without the `order.remove(&old_seq)` in `insert`,
/// `order` grows per rewrite and the hot set gets evicted by first-write time.
#[test]
fn rewriting_a_hot_set_neither_leaks_nor_evicts_it() {
    let mut store = SectorStore::with_capacity(8);
    for round in 0..500u32 {
        // Four hot sectors, rewritten every round, as the FAT and directory
        // sectors are in a real session.
        for hot in 0..4u32 {
            store.insert(hot, &sector(round as u8));
        }
        // A cold stream flowing past them.
        store.insert(1000 + round, &sector(round as u8));
    }

    assert!(store.cache_len() <= 8, "cache {} > 8", store.cache_len());
    assert_eq!(
        store.order.len(),
        store.cache_len(),
        "order must hold exactly one entry per cached sector"
    );
    // The hot set is still resident: never spilled, never re-read from disk.
    for hot in 0..4u32 {
        assert!(
            store.cache.contains_key(&hot),
            "hot sector {hot} was evicted despite being rewritten every round"
        );
        assert_eq!(store.get(hot).unwrap(), Some(sector(499u32 as u8)));
    }
}

/// An unusable spill path must cost the bound, never a sector. This is the
/// disk-full and read-only-temp case.
#[test]
fn an_unusable_spill_path_keeps_every_sector_exact() {
    let dir = std::env::temp_dir().join(format!("katea_store_broken_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut store = SectorStore::with_capacity(4);
    // A directory where the spill file should be: opening it always fails.
    store.set_spill_path(&dir);

    for i in 0..50u32 {
        store.insert(i, &sector(i as u8));
    }
    assert!(store.broken, "a failed spill must mark the store broken");
    for i in 0..50u32 {
        assert_eq!(
            store.get(i).unwrap(),
            Some(sector(i as u8)),
            "sector {i} must survive a broken spill"
        );
        assert!(store.was_written(i));
    }
    assert_eq!(
        store.read_errors(),
        0,
        "nothing was spilled, so no read failed"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Presence is the "was this touched" oracle reconcile relies on, so it must
/// outlive the payload's residency.
#[test]
fn presence_survives_eviction() {
    let mut store = SectorStore::with_capacity(2);
    for i in 0..100u32 {
        store.insert(i, &sector(1));
    }
    for i in 0..100u32 {
        assert!(store.was_written(i), "sector {i} lost its presence bit");
    }
    assert!(store.cache_len() <= 2);
}

/// A write anywhere in a chunk dates the whole chunk, and an untouched span
/// stays at 0. Reconcile skips re-reading a file only while this stays put.
#[test]
fn max_seq_tracks_writes_per_chunk() {
    let mut store = SectorStore::with_capacity(64);
    assert_eq!(store.max_seq_in(0, 256), 0, "untouched span");

    store.insert(5, &sector(1));
    let after_first = store.max_seq_in(0, 256);
    assert!(after_first > 0);
    // A different chunk: 256 sectors per chunk, so LBA 300 is chunk 1.
    assert_eq!(
        store.max_seq_in(300, 1),
        0,
        "a neighbouring chunk is untouched"
    );

    store.insert(300, &sector(2));
    assert!(store.max_seq_in(300, 1) > after_first);
    assert_eq!(
        store.max_seq_in(0, 256),
        after_first,
        "writing chunk 1 must not date chunk 0"
    );

    // A span crossing both chunks takes the max.
    assert_eq!(store.max_seq_in(0, 512), store.max_seq_in(300, 1));
}

/// The spill file is scratch: it must not outlive the store. On Windows this only
/// works because `Drop` closes the handle before unlinking.
#[test]
fn drop_removes_the_spill_file() {
    let path;
    {
        let mut store = SectorStore::with_capacity(2);
        path = store.path.clone();
        for i in 0..20u32 {
            store.insert(i, &sector(i as u8));
        }
        assert!(path.exists(), "eviction should have created the spill");
    }
    assert!(!path.exists(), "the spill file outlived its store");
}

/// A session that fits in RAM must not touch the filesystem at all.
#[test]
fn no_spill_file_without_eviction() {
    let mut store = SectorStore::with_capacity(64);
    for i in 0..64u32 {
        store.insert(i, &sector(i as u8));
    }
    assert!(
        !store.path.exists(),
        "created a spill file without evicting"
    );
    assert!(store.spill.is_none());
}

/// `was_written_span` MUST BE the per-sector loop it replaces.
///
/// The reconcile pass asks "did the guest touch this cluster?" once per cluster
/// of every live file's chain, and it used to do that with `spc` separate hashed
/// probes. The span form hashes once per 128 KiB chunk instead. It is only
/// allowed to be faster, never different: a `false` where the loop says `true`
/// would make reconcile skip a file the guest actually wrote, and a host file
/// would silently keep its stale bytes.
///
/// NON-VACUOUS: the sweep covers spans that straddle chunk boundaries, start and
/// end inside untouched chunks, and cross chunks that exist but hold no written
/// bit in range -- the three shapes a chunk-granular short-circuit gets wrong.
/// Returning `self.chunks.contains_key(&chunk_id(first))` fails it immediately.
#[test]
fn was_written_span_agrees_with_the_per_sector_loop() {
    let mut store = SectorStore::with_capacity(256);
    // Writes clustered around two chunk boundaries, plus a scattered tail.
    for lba in [0u32, 1, 255, 256, 257, 511, 512, 1000, 1023, 1024, 4096] {
        store.insert(lba, &sector(lba as u8));
    }
    for i in 0..64u32 {
        store.insert(scatter(i), &sector(i as u8));
    }

    let mut saw_true = 0;
    let mut saw_false = 0;
    for first in 0..2_100u32 {
        for count in [0u32, 1, 2, 7, 8, 9, 255, 256, 257, 512] {
            let want = (first..first + count).any(|l| store.was_written(l));
            assert_eq!(
                store.was_written_span(first, count),
                want,
                "span [{first}, {}) disagreed",
                first + count
            );
            if want {
                saw_true += 1;
            } else {
                saw_false += 1;
            }
        }
    }
    assert!(
        saw_true > 100 && saw_false > 100,
        "the sweep must exercise both answers, got {saw_true} true / {saw_false} false"
    );
    assert!(!store.was_written_span(4_096, 0), "an empty span is clean");
    assert!(!store.was_written_span(900_000, 4_096), "past every write");
    assert!(store.was_written_span(4_000, 200), "reaches the last write");
}
