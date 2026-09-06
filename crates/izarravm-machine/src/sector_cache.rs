// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Bounded LRU cache of backing sectors. Hits avoid host reads while preserving
//! guest-visible bytes and hardware timing. Residency is excluded from canonical
//! state because it changes neither. Guest writes update the cached sector.

use std::collections::HashMap;

/// One 512-byte sector.
pub(crate) const SECTOR: usize = 512;

/// How many sectors stay resident: 32,768, i.e. 16 MiB of host memory.
///
/// Sized against the workload that motivated the cache rather than against a
/// round number. Duke's Atomic GRP is 44 MB but the demo touches a small
/// fraction of it (the level's art and the speech lumps), and the whole
/// synthesized FAT at 4 KiB clusters is ~1,040 sectors, so 16 MiB holds the FAT
/// plus a real streaming working set with room over. It is host memory, not
/// guest memory: nothing in the guest's 64 MiB is spent here.
pub(crate) const CAPACITY_SECTORS: usize = 32_768;

/// A cache slot. `prev`/`next` are indices into `entries`, forming the LRU list;
/// [`NIL`] terminates it.
#[derive(Debug)]
struct Entry {
    lba: u32,
    data: [u8; SECTOR],
    prev: usize,
    next: usize,
}

const NIL: usize = usize::MAX;

/// LRU sector cache with O(1) lookup, insert and eviction.
#[derive(Debug)]
pub(crate) struct SectorCache {
    /// LBA to slot index. Empty when the cache is disabled.
    index: HashMap<u32, usize>,
    entries: Vec<Entry>,
    /// Most and least recently used slots.
    head: usize,
    tail: usize,
    /// Off means every read is a miss and nothing is ever stored, so the charge
    /// model collapses back to the uncached one. This is the A/B switch.
    enabled: bool,
    hits: u64,
    misses: u64,
}

impl SectorCache {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            index: HashMap::new(),
            entries: Vec::new(),
            head: NIL,
            tail: NIL,
            enabled,
            hits: 0,
            misses: 0,
        }
    }

    pub(crate) fn hits(&self) -> u64 {
        self.hits
    }

    pub(crate) fn misses(&self) -> u64 {
        self.misses
    }

    /// Resident sectors right now.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Look `lba` up, counting the hit or the miss. A hit also refreshes the
    /// entry's LRU position, which is why this takes `&mut self`.
    pub(crate) fn get(&mut self, lba: u32) -> Option<[u8; SECTOR]> {
        if !self.enabled {
            self.misses = self.misses.saturating_add(1);
            return None;
        }
        match self.index.get(&lba).copied() {
            Some(slot) => {
                self.hits = self.hits.saturating_add(1);
                self.touch(slot);
                Some(self.entries[slot].data)
            }
            None => {
                self.misses = self.misses.saturating_add(1);
                None
            }
        }
    }

    /// Make `lba` resident with `data`, evicting the least recently used sector
    /// if the cache is full. Used both to fill after a miss and to keep the
    /// cache coherent on a write (write-through: the guest's bytes ARE the new
    /// truth for that sector, so storing them is both the invalidation and the
    /// refill).
    pub(crate) fn put(&mut self, lba: u32, data: &[u8; SECTOR]) {
        if !self.enabled {
            return;
        }
        if let Some(&slot) = self.index.get(&lba) {
            self.entries[slot].data = *data;
            self.touch(slot);
            return;
        }
        if self.entries.len() < CAPACITY_SECTORS {
            let slot = self.entries.len();
            self.entries.push(Entry {
                lba,
                data: *data,
                prev: NIL,
                next: NIL,
            });
            self.index.insert(lba, slot);
            self.push_front(slot);
            return;
        }
        // Full: reuse the least recently used slot in place, so the cache never
        // reallocates and the eviction order is exactly the guest's access order.
        let slot = self.tail;
        debug_assert_ne!(slot, NIL, "a full cache has a tail");
        self.unlink(slot);
        let old = self.entries[slot].lba;
        self.index.remove(&old);
        self.entries[slot].lba = lba;
        self.entries[slot].data = *data;
        self.index.insert(lba, slot);
        self.push_front(slot);
    }

    fn touch(&mut self, slot: usize) {
        if self.head == slot {
            return;
        }
        self.unlink(slot);
        self.push_front(slot);
    }

    fn unlink(&mut self, slot: usize) {
        let (prev, next) = (self.entries[slot].prev, self.entries[slot].next);
        if prev != NIL {
            self.entries[prev].next = next;
        } else if self.head == slot {
            self.head = next;
        }
        if next != NIL {
            self.entries[next].prev = prev;
        } else if self.tail == slot {
            self.tail = prev;
        }
        self.entries[slot].prev = NIL;
        self.entries[slot].next = NIL;
    }

    fn push_front(&mut self, slot: usize) {
        self.entries[slot].prev = NIL;
        self.entries[slot].next = self.head;
        if self.head != NIL {
            self.entries[self.head].prev = slot;
        }
        self.head = slot;
        if self.tail == NIL {
            self.tail = slot;
        }
    }
}

#[cfg(test)]
#[path = "sector_cache_test.rs"]
mod tests;
