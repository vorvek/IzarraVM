// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Sparse 16-byte physical code watches for native stores.

use std::collections::{HashMap, hash_map::Entry};

use crate::U32BuildHasher;

const PAGE_SHIFT: u32 = 12;
const PAGE_COUNT: usize = 1 << (32 - PAGE_SHIFT);
const CHUNK_SHIFT: u32 = 4;
const CHUNKS_PER_PAGE: usize = 1 << (PAGE_SHIFT - CHUNK_SHIFT);
const MASK_WORDS: usize = CHUNKS_PER_PAGE / u64::BITS as usize;
const MAX_RECYCLED_PAGES: usize = 64;
const MAX_INACTIVE_PAGES: usize = 1024;
const MAX_STICKY_PRECISE_PAGES: usize = 4096;
const STICKY_LOW_PRECISE_PAGES: u32 = (2 << 20) >> PAGE_SHIFT;
const MAX_STICKY_HIGH_PRECISE_PAGES: usize =
    MAX_STICKY_PRECISE_PAGES - STICKY_LOW_PRECISE_PAGES as usize;
const PAGE_BITMAP_WORDS: usize = PAGE_COUNT / u64::BITS as usize;

type ChunkMask = [u64; MASK_WORDS];

static ALL_CHUNKS_WATCHED: ChunkMask = [u64::MAX; MASK_WORDS];

/// Generation-scoped decode marks for native stores. Marks are deliberately sticky until the
/// decode cache is invalidated: an evicted line can leave a conservative false positive, but no
/// live decoded byte can become unwatched. This type has no release operation so it cannot be
/// confused with the exactly owned block watch below.
pub(crate) struct StickyDecodeCodeWatch {
    table: Option<Box<[usize]>>,
    precise: HashMap<u32, Box<ChunkMask>, U32BuildHasher>,
    precise_high_pages: usize,
    coarse: Box<[u64]>,
    coarse_pages: Vec<u32>,
    #[allow(clippy::vec_box)]
    // Box addresses remain stable while native code can observe them.
    recycled: Vec<Box<ChunkMask>>,
}

impl Default for StickyDecodeCodeWatch {
    fn default() -> Self {
        Self {
            table: None,
            precise: HashMap::default(),
            precise_high_pages: 0,
            coarse: vec![0; PAGE_BITMAP_WORDS].into_boxed_slice(),
            coarse_pages: Vec::new(),
            recycled: Vec::new(),
        }
    }
}

impl StickyDecodeCodeWatch {
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

    pub(crate) fn table_base(&mut self) -> usize {
        if self.table.is_none() {
            let mut table = vec![0usize; PAGE_COUNT].into_boxed_slice();
            for (&page, mask) in &mut self.precise {
                table[page as usize] = std::ptr::from_mut(&mut **mask).expose_provenance();
            }
            let coarse = Self::coarse_pointer();
            for &page in &self.coarse_pages {
                table[page as usize] = coarse;
            }
            self.table = Some(table);
        }
        self.table
            .as_ref()
            .expect("native decode-watch table was initialized")
            .as_ptr() as usize
    }

    pub(crate) fn clear(&mut self) {
        if let Some(table) = self.table.as_mut() {
            for &page in self.precise.keys() {
                table[page as usize] = 0;
            }
            for &page in &self.coarse_pages {
                table[page as usize] = 0;
            }
        }

        // Published pointers are gone before any precise mask is reset or recycled.
        let (precise, recycled) = (&mut self.precise, &mut self.recycled);
        for (_, mut mask) in precise.drain() {
            mask.fill(0);
            if recycled.len() < MAX_RECYCLED_PAGES {
                recycled.push(mask);
            }
        }
        self.precise_high_pages = 0;

        for page in self.coarse_pages.drain(..) {
            let word = (page >> 6) as usize;
            self.coarse[word] &= !(1u64 << (page & 63));
        }
    }

    fn mark_chunk(&mut self, physical: u32) {
        let page = physical >> PAGE_SHIFT;
        let chunk = (physical & 0xfff) >> CHUNK_SHIFT;
        let word = (chunk / u64::BITS) as usize;
        let bit = 1u64 << (chunk & (u64::BITS - 1));
        let published = self.table.as_ref().map(|table| table[page as usize]);
        if let Some(pointer) = published.filter(|pointer| *pointer != 0) {
            if pointer == Self::coarse_pointer() {
                return;
            }
            debug_assert_eq!(
                self.precise
                    .get(&page)
                    .map(|owner| std::ptr::from_ref(&**owner).expose_provenance()),
                Some(pointer)
            );
            // `precise` owns this box. Native execution and cache mutation are serialized, and
            // the owner check precedes construction of the mutable raw borrow.
            let mask = unsafe { &mut *std::ptr::with_exposed_provenance_mut::<ChunkMask>(pointer) };
            mask[word] |= bit;
            return;
        }

        if self.coarse_page(page) {
            debug_assert!(published.is_none());
            return;
        }

        if let Some(mask) = self.precise.get_mut(&page) {
            mask[word] |= bit;
            if let Some(table) = self.table.as_mut() {
                table[page as usize] = std::ptr::from_mut(&mut **mask).expose_provenance();
            }
            return;
        }

        let low_page = page < STICKY_LOW_PRECISE_PAGES;
        if low_page || self.precise_high_pages < MAX_STICKY_HIGH_PRECISE_PAGES {
            let mut mask = self
                .recycled
                .pop()
                .unwrap_or_else(|| Box::new([0; MASK_WORDS]));
            debug_assert!(mask.iter().all(|word| *word == 0));
            mask[word] |= bit;
            let pointer = std::ptr::from_mut(&mut *mask).expose_provenance();
            self.precise.insert(page, mask);
            self.precise_high_pages += usize::from(!low_page);
            if let Some(table) = self.table.as_mut() {
                // Publish only after the owned mask contains the new mark.
                table[page as usize] = pointer;
            }
            return;
        }

        let bitmap_word = (page >> 6) as usize;
        let bitmap_bit = 1u64 << (page & 63);
        debug_assert_eq!(self.coarse[bitmap_word] & bitmap_bit, 0);
        self.coarse[bitmap_word] |= bitmap_bit;
        self.coarse_pages.push(page);
        if let Some(table) = self.table.as_mut() {
            table[page as usize] = Self::coarse_pointer();
        }
    }

    fn coarse_page(&self, page: u32) -> bool {
        self.coarse[(page >> 6) as usize] & (1u64 << (page & 63)) != 0
    }

    fn coarse_pointer() -> usize {
        std::ptr::from_ref(&ALL_CHUNKS_WATCHED).expose_provenance()
    }

    #[cfg(test)]
    pub(crate) fn is_watched(&self, physical: u32) -> bool {
        let page = physical >> PAGE_SHIFT;
        if self.coarse_page(page) {
            return true;
        }
        let chunk = (physical & 0xfff) >> CHUNK_SHIFT;
        let word = (chunk / u64::BITS) as usize;
        self.precise
            .get(&page)
            .is_some_and(|mask| mask[word] & (1u64 << (chunk & (u64::BITS - 1))) != 0)
    }

    #[cfg(test)]
    pub(crate) fn precise_pages(&self) -> usize {
        self.precise.len()
    }

    #[cfg(test)]
    pub(crate) fn coarse_page_count(&self) -> usize {
        self.coarse_pages.len()
    }
}

impl Clone for StickyDecodeCodeWatch {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl PartialEq for StickyDecodeCodeWatch {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for StickyDecodeCodeWatch {}

impl std::fmt::Debug for StickyDecodeCodeWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "StickyDecodeCodeWatch {{ {} precise pages, {} coarse pages }}",
            self.precise.len(),
            self.coarse_pages.len()
        )
    }
}

#[repr(C)]
struct WatchPage {
    mask: ChunkMask,
    refs: [u32; CHUNKS_PER_PAGE],
    active_chunks: u16,
}

const _: () = assert!(std::mem::offset_of!(WatchPage, mask) == 0);

impl Default for WatchPage {
    fn default() -> Self {
        Self {
            mask: [0; MASK_WORDS],
            refs: [0; CHUNKS_PER_PAGE],
            active_chunks: 0,
        }
    }
}

impl WatchPage {
    fn reset(&mut self) {
        for (word_index, word) in self.mask.iter_mut().enumerate() {
            let mut live = *word;
            while live != 0 {
                let bit = live.trailing_zeros() as usize;
                self.refs[word_index * u64::BITS as usize + bit] = 0;
                live &= live - 1;
            }
            *word = 0;
        }
        self.active_chunks = 0;
        debug_assert!(self.refs.iter().all(|count| *count == 0));
    }
}

/// A stable page-pointer table with lazily allocated per-page chunk masks. Before native code
/// needs the table, decoded chunks stay in a small sparse map to avoid an 8 MiB allocation for
/// interpreter-only CPUs.
#[derive(Default)]
pub(crate) struct NativeCodeWatch {
    table: Option<Box<[usize]>>,
    pages: HashMap<u32, Box<WatchPage>, U32BuildHasher>,
    inactive_pages: usize,
    /// Identity-free zeroed pages retained across hot global clears. Sixty-four pages cost about
    /// 68 KiB per watch and cover the observed code working set without unbounded growth.
    #[allow(clippy::vec_box)]
    // Box addresses remain stable while native code can observe them.
    recycled_pages: Vec<Box<WatchPage>>,
}

impl NativeCodeWatch {
    pub(crate) fn acquire_range(&mut self, physical: u32, len: u32) {
        self.for_each_chunk(physical, len, |watch, chunk| {
            let page = chunk >> PAGE_SHIFT;
            let index = ((chunk & 0xfff) >> CHUNK_SHIFT) as usize;
            let published = watch.table.as_ref().map(|table| table[page as usize]);
            let publish = if let Some(pointer) = published.filter(|pointer| *pointer != 0) {
                debug_assert_eq!(
                    watch
                        .pages
                        .get(&page)
                        .map(|owner| std::ptr::from_ref(&**owner).expose_provenance()),
                    Some(pointer)
                );
                // `pages` owns this boxed allocation. Native execution and cache mutation are
                // serialized, so the published address can be borrowed mutably here.
                let page_watch =
                    unsafe { &mut *std::ptr::with_exposed_provenance_mut::<WatchPage>(pointer) };
                Self::acquire_chunk(page_watch, index);
                None
            } else {
                let table_initialized = published.is_some();
                let (pages, recycled_pages, inactive_pages) = (
                    &mut watch.pages,
                    &mut watch.recycled_pages,
                    &mut watch.inactive_pages,
                );
                match pages.entry(page) {
                    Entry::Occupied(mut entry) => {
                        let page_watch = entry.get_mut();
                        if table_initialized {
                            debug_assert_eq!(
                                page_watch.active_chunks, 0,
                                "unpublished code-watch page must be inactive"
                            );
                            debug_assert_ne!(
                                *inactive_pages, 0,
                                "retained code-watch page must be counted"
                            );
                            debug_assert!(page_watch.mask.iter().all(|word| *word == 0));
                            debug_assert!(page_watch.refs.iter().all(|count| *count == 0));
                            let first = Self::acquire_chunk(page_watch, index);
                            debug_assert!(first, "retained code-watch page must reactivate once");
                            *inactive_pages -= 1;
                            Some(std::ptr::from_mut(&mut **page_watch).expose_provenance())
                        } else {
                            debug_assert_ne!(page_watch.active_chunks, 0);
                            Self::acquire_chunk(page_watch, index);
                            None
                        }
                    }
                    Entry::Vacant(entry) => {
                        let mut page_watch = recycled_pages
                            .pop()
                            .unwrap_or_else(|| Box::new(WatchPage::default()));
                        debug_assert_eq!(page_watch.active_chunks, 0);
                        debug_assert!(page_watch.mask.iter().all(|word| *word == 0));
                        debug_assert!(page_watch.refs.iter().all(|count| *count == 0));
                        let first = Self::acquire_chunk(&mut page_watch, index);
                        debug_assert!(first);
                        let pointer = std::ptr::from_mut(&mut *page_watch).expose_provenance();
                        entry.insert(page_watch);
                        table_initialized.then_some(pointer)
                    }
                }
            };
            if let Some(pointer) = publish
                && let Some(table) = watch.table.as_mut()
            {
                // Native execution and watch mutation are serialized by `&mut CpuGsw`. Publish
                // only after the boxed page owns an initialized mask.
                table[page as usize] = pointer;
            }
        });
    }

    pub(crate) fn release_range(&mut self, physical: u32, len: u32) {
        self.for_each_chunk(physical, len, |watch, chunk| {
            let page = chunk >> PAGE_SHIFT;
            let index = ((chunk & 0xfff) >> CHUNK_SHIFT) as usize;
            let published = watch.table.as_ref().map(|table| table[page as usize]);
            let remove_page = if let Some(pointer) = published {
                assert_ne!(
                    pointer, 0,
                    "refcounted code-watch page must remain published"
                );
                debug_assert_eq!(
                    watch
                        .pages
                        .get(&page)
                        .map(|owner| std::ptr::from_ref(&**owner).expose_provenance()),
                    Some(pointer)
                );
                // The raw borrow ends before a final owner removes and recycles the box.
                let page_watch =
                    unsafe { &mut *std::ptr::with_exposed_provenance_mut::<WatchPage>(pointer) };
                Self::release_chunk(page_watch, index)
            } else {
                let page_watch = watch
                    .pages
                    .get_mut(&page)
                    .expect("refcounted code-watch page must exist");
                Self::release_chunk(page_watch, index)
            };
            if remove_page {
                if let Some(table) = watch.table.as_mut() {
                    // Clear the published pointer before retaining or freeing its boxed owner.
                    table[page as usize] = 0;
                }
                if published.is_some() && watch.inactive_pages < MAX_INACTIVE_PAGES {
                    debug_assert!(watch.pages.get(&page).is_some_and(|retained| {
                        retained.active_chunks == 0
                            && retained.mask.iter().all(|word| *word == 0)
                            && retained.refs.iter().all(|count| *count == 0)
                    }));
                    watch.inactive_pages += 1;
                    return;
                }
                let removed = watch
                    .pages
                    .remove(&page)
                    .expect("inactive code-watch page must still exist");
                if let Some(pointer) = published {
                    assert_eq!(
                        std::ptr::from_ref(&*removed).expose_provenance(),
                        pointer,
                        "removed code-watch page must own its published pointer"
                    );
                }
                debug_assert!(removed.mask.iter().all(|word| *word == 0));
                debug_assert!(removed.refs.iter().all(|count| *count == 0));
                watch.recycle_page(removed);
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
            for (&page, page_watch) in &mut self.pages {
                debug_assert_ne!(page_watch.active_chunks, 0);
                table[page as usize] = std::ptr::from_mut(&mut **page_watch).expose_provenance();
            }
            self.table = Some(table);
        }
        self.table
            .as_ref()
            .expect("native code-watch table was initialized")
            .as_ptr() as usize
    }

    pub(crate) fn clear(&mut self) {
        if let Some(table) = self.table.as_mut() {
            for &page in self.pages.keys() {
                table[page as usize] = 0;
            }
        }
        let (pages, recycled_pages) = (&mut self.pages, &mut self.recycled_pages);
        for (_, mut page) in pages.drain() {
            if recycled_pages.len() < MAX_RECYCLED_PAGES {
                page.reset();
                recycled_pages.push(page);
            }
        }
        self.inactive_pages = 0;
    }

    fn acquire_chunk(page_watch: &mut WatchPage, index: usize) -> bool {
        let count = &mut page_watch.refs[index];
        let was_zero = *count == 0;
        *count = count
            .checked_add(1)
            .expect("native code-watch refcount overflow");
        if was_zero {
            let word = index / u64::BITS as usize;
            let bit = index % u64::BITS as usize;
            page_watch.mask[word] |= 1u64 << bit;
            page_watch.active_chunks += 1;
        }
        was_zero && page_watch.active_chunks == 1
    }

    fn release_chunk(page_watch: &mut WatchPage, index: usize) -> bool {
        let count = &mut page_watch.refs[index];
        assert_ne!(*count, 0, "refcounted code-watch chunk must be marked");
        *count -= 1;
        if *count == 0 {
            let word = index / u64::BITS as usize;
            let bit = index % u64::BITS as usize;
            page_watch.mask[word] &= !(1u64 << bit);
            page_watch.active_chunks -= 1;
        }
        page_watch.active_chunks == 0
    }

    fn recycle_page(&mut self, page: Box<WatchPage>) {
        if self.recycled_pages.len() < MAX_RECYCLED_PAGES {
            debug_assert!(page.mask.iter().all(|word| *word == 0));
            debug_assert!(page.refs.iter().all(|count| *count == 0));
            debug_assert_eq!(page.active_chunks, 0);
            self.recycled_pages.push(page);
        }
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

    #[cfg(test)]
    pub(crate) fn is_watched(&self, physical: u32) -> bool {
        self.chunk_watched(physical)
    }

    #[cfg(test)]
    pub(crate) fn active_pages(&self) -> usize {
        self.pages
            .values()
            .filter(|watch| watch.active_chunks != 0)
            .count()
    }

    #[cfg(test)]
    pub(crate) fn active_chunks(&self) -> usize {
        self.pages
            .values()
            .map(|watch| usize::from(watch.active_chunks))
            .sum()
    }

    #[cfg(test)]
    pub(crate) fn refcount(&self, physical: u32) -> u32 {
        let page = physical >> PAGE_SHIFT;
        let index = ((physical & 0xfff) >> CHUNK_SHIFT) as usize;
        self.pages.get(&page).map_or(0, |watch| watch.refs[index])
    }

    #[cfg(test)]
    pub(crate) fn recycled_pages(&self) -> usize {
        self.recycled_pages.len()
    }

    #[cfg(test)]
    pub(crate) fn inactive_pages(&self) -> usize {
        self.inactive_pages
    }

    pub(crate) fn has_resident_pages(&self) -> bool {
        !self.pages.is_empty()
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
            // `pages` owns boxed masks whose addresses stay fixed until their table entry is
            // cleared. Native execution and watch mutation never overlap.
            let page_watch = unsafe { &*std::ptr::with_exposed_provenance::<WatchPage>(pointer) };
            return page_watch.mask[word] & bit != 0;
        }
        self.pages
            .get(&page)
            .is_some_and(|watch| watch.mask[word] & bit != 0)
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
        write!(f, "NativeCodeWatch {{ {} pages }}", self.pages.len())
    }
}

#[cfg(test)]
#[path = "code_watch_test.rs"]
mod tests;
