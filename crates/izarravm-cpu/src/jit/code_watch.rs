// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Sparse 16-byte physical code watches for native stores.

use std::collections::HashMap;

use crate::U32BuildHasher;

const PAGE_SHIFT: u32 = 12;
const PAGE_COUNT: usize = 1 << (32 - PAGE_SHIFT);
const CHUNK_SHIFT: u32 = 4;
const CHUNKS_PER_PAGE: usize = 1 << (PAGE_SHIFT - CHUNK_SHIFT);
const MASK_WORDS: usize = CHUNKS_PER_PAGE / u64::BITS as usize;

type ChunkMask = [u64; MASK_WORDS];

/// A stable page-pointer table with lazily allocated per-page chunk masks. Before native code
/// needs the table, decoded chunks stay in a small sparse map to avoid an 8 MiB allocation for
/// interpreter-only CPUs.
#[derive(Default)]
pub(crate) struct NativeCodeWatch {
    table: Option<Box<[usize]>>,
    masks: HashMap<u32, Box<ChunkMask>, U32BuildHasher>,
    refcounts: HashMap<u32, Box<[u16; CHUNKS_PER_PAGE]>, U32BuildHasher>,
    dirty_pages: Vec<u32>,
}

impl NativeCodeWatch {
    pub(crate) fn mark(&mut self, physical: u32, len: u8) {
        if len == 0 {
            return;
        }
        self.mark_chunk(physical);
        let last = physical.wrapping_add(u32::from(len) - 1);
        if physical >> CHUNK_SHIFT != last >> CHUNK_SHIFT {
            self.mark_chunk(last);
        }
    }

    #[cfg(test)]
    pub(crate) fn mark_range(&mut self, physical: u32, len: u32) {
        if len == 0 {
            return;
        }
        let mut chunk = physical & !((1 << CHUNK_SHIFT) - 1);
        let last = physical.wrapping_add(len - 1) & !((1 << CHUNK_SHIFT) - 1);
        loop {
            self.mark_chunk(chunk);
            if chunk == last {
                break;
            }
            chunk = chunk.wrapping_add(1 << CHUNK_SHIFT);
        }
    }

    pub(crate) fn mark_refcounted_range(&mut self, physical: u32, len: u32) {
        self.for_each_chunk(physical, len, |watch, chunk| {
            let page = chunk >> PAGE_SHIFT;
            let index = ((chunk & 0xfff) >> CHUNK_SHIFT) as usize;
            let was_zero = {
                let counts = watch
                    .refcounts
                    .entry(page)
                    .or_insert_with(|| Box::new([0; CHUNKS_PER_PAGE]));
                let was_zero = counts[index] == 0;
                counts[index] = counts[index]
                    .checked_add(1)
                    .expect("native code-watch refcount overflow");
                was_zero
            };
            if was_zero {
                watch.mark_chunk(chunk);
            }
        });
    }

    pub(crate) fn unmark_refcounted_range(&mut self, physical: u32, len: u32) {
        self.for_each_chunk(physical, len, |watch, chunk| {
            let page = chunk >> PAGE_SHIFT;
            let index = ((chunk & 0xfff) >> CHUNK_SHIFT) as usize;
            let became_zero = {
                let counts = watch
                    .refcounts
                    .get_mut(&page)
                    .expect("refcounted code-watch page must exist");
                let count = &mut counts[index];
                assert_ne!(*count, 0, "refcounted code-watch chunk must be marked");
                *count -= 1;
                *count == 0
            };
            if became_zero {
                watch.unmark_chunk(chunk);
            }
        });
    }

    pub(crate) fn range_watched(&self, physical: u32, len: u32) -> bool {
        if len == 0 {
            return false;
        }
        let mut chunk = physical & !((1 << CHUNK_SHIFT) - 1);
        let last = physical.wrapping_add(len - 1) & !((1 << CHUNK_SHIFT) - 1);
        loop {
            if self.chunk_watched(chunk) {
                return true;
            }
            if chunk == last {
                return false;
            }
            chunk = chunk.wrapping_add(1 << CHUNK_SHIFT);
        }
    }

    pub(crate) fn table_base(&mut self) -> usize {
        if self.table.is_none() {
            let mut table = vec![0usize; PAGE_COUNT].into_boxed_slice();
            for (&page, mask) in &mut self.masks {
                if mask.iter().any(|word| *word != 0) {
                    table[page as usize] = mask.as_mut() as *mut ChunkMask as usize;
                    self.dirty_pages.push(page);
                }
            }
            self.table = Some(table);
        }
        self.table
            .as_ref()
            .expect("native code-watch table was initialized")
            .as_ptr() as usize
    }

    pub(crate) fn clear(&mut self) {
        if self.table.is_none() {
            self.masks.clear();
            self.refcounts.clear();
            self.dirty_pages.clear();
            return;
        }
        let table = self
            .table
            .as_mut()
            .expect("native code-watch table was initialized");
        for page in self.dirty_pages.drain(..) {
            table[page as usize] = 0;
            self.masks
                .get_mut(&page)
                .expect("dirty code-watch page owns a mask")
                .fill(0);
        }
        self.refcounts.clear();
    }

    fn for_each_chunk(&mut self, physical: u32, len: u32, mut f: impl FnMut(&mut Self, u32)) {
        if len == 0 {
            return;
        }
        let mut chunk = physical & !((1 << CHUNK_SHIFT) - 1);
        let last = physical.wrapping_add(len - 1) & !((1 << CHUNK_SHIFT) - 1);
        loop {
            f(self, chunk);
            if chunk == last {
                break;
            }
            chunk = chunk.wrapping_add(1 << CHUNK_SHIFT);
        }
    }

    fn mark_chunk(&mut self, physical: u32) {
        let page = physical >> PAGE_SHIFT;
        let chunk = (physical & 0xfff) >> CHUNK_SHIFT;
        let mask = self
            .masks
            .entry(page)
            .or_insert_with(|| Box::new([0; MASK_WORDS]));
        if mask.iter().all(|word| *word == 0) {
            if let Some(table) = self.table.as_mut() {
                table[page as usize] = mask.as_mut() as *mut ChunkMask as usize;
                if !self.dirty_pages.contains(&page) {
                    self.dirty_pages.push(page);
                }
            }
        }
        mask[(chunk / 64) as usize] |= 1u64 << (chunk & 63);
    }

    fn unmark_chunk(&mut self, physical: u32) {
        let page = physical >> PAGE_SHIFT;
        let chunk = (physical & 0xfff) >> CHUNK_SHIFT;
        let mask = self
            .masks
            .get_mut(&page)
            .expect("marked code-watch page must own a mask");
        mask[(chunk / 64) as usize] &= !(1u64 << (chunk & 63));
        if mask.iter().all(|word| *word == 0)
            && let Some(table) = self.table.as_mut()
        {
            table[page as usize] = 0;
        }
    }

    #[cfg(test)]
    pub(crate) fn is_watched(&self, physical: u32) -> bool {
        self.chunk_watched(physical)
    }

    fn chunk_watched(&self, physical: u32) -> bool {
        let page = physical >> PAGE_SHIFT;
        let chunk = (physical & 0xfff) >> CHUNK_SHIFT;
        let word = (chunk / 64) as usize;
        let bit = 1u64 << (chunk & 63);
        if let Some(table) = &self.table {
            let pointer = table[page as usize];
            if pointer == 0 {
                return false;
            }
            // `masks` owns boxed arrays whose addresses stay fixed until this watch is dropped.
            // The table is updated when each box is created and read only while `self` owns it.
            let mask = unsafe { &*(pointer as *const ChunkMask) };
            return mask[word] & bit != 0;
        }
        self.masks
            .get(&page)
            .is_some_and(|mask| mask[word] & bit != 0)
    }
}

impl Clone for NativeCodeWatch {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl PartialEq for NativeCodeWatch {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for NativeCodeWatch {}

impl std::fmt::Debug for NativeCodeWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NativeCodeWatch {{ {} masks }}", self.masks.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_every_chunk_touched_and_clears_without_moving_the_table() {
        let mut watch = NativeCodeWatch::default();
        watch.mark(0x1f, 2);
        assert!(watch.is_watched(0x10));
        assert!(watch.is_watched(0x20));
        assert!(!watch.is_watched(0x30));

        let base = watch.table_base();
        watch.clear();
        assert_eq!(watch.table_base(), base);
        assert!(!watch.is_watched(0x10));
        assert!(!watch.is_watched(0x20));

        watch.mark(0x20, 1);
        assert_eq!(watch.table_base(), base);
        assert!(watch.is_watched(0x20));

        watch.mark_range(0x2f, 33);
        assert!(watch.range_watched(0x20, 1));
        assert!(watch.range_watched(0x40, 1));
        assert!(!watch.range_watched(0x50, 1));
    }

    #[test]
    fn refcounted_ranges_keep_shared_chunks_until_the_last_owner_leaves() {
        let mut watch = NativeCodeWatch::default();
        watch.mark_refcounted_range(0x100, 16);
        watch.mark_refcounted_range(0x108, 16);
        assert!(watch.is_watched(0x100));
        assert!(watch.is_watched(0x110));

        watch.unmark_refcounted_range(0x100, 16);
        assert!(watch.is_watched(0x100));
        assert!(watch.is_watched(0x110));

        watch.unmark_refcounted_range(0x108, 16);
        assert!(!watch.is_watched(0x100));
        assert!(!watch.is_watched(0x110));
    }

    #[test]
    fn clear_removes_top_level_pointer_and_mark_republishes_it() {
        let mut watch = NativeCodeWatch::default();
        let physical = 0x12_310;
        let page = (physical >> PAGE_SHIFT) as usize;
        watch.mark_refcounted_range(physical, 1);
        let base = watch.table_base();
        // The returned base owns PAGE_COUNT entries for the lifetime of `watch`.
        let entry = unsafe { (base as *const usize).add(page) };
        assert_ne!(unsafe { *entry }, 0);

        watch.clear();
        assert_eq!(unsafe { *entry }, 0);

        watch.mark_refcounted_range(physical, 1);
        assert_eq!(watch.table_base(), base);
        assert_ne!(unsafe { *entry }, 0);
    }
}
