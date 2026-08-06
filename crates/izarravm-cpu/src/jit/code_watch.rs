// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Sparse physical code watches for native stores, at `CHUNK_SHIFT` granularity.

use std::collections::{HashMap, hash_map::Entry};

use crate::U32BuildHasher;

const PAGE_SHIFT: u32 = 12;
const PAGE_COUNT: usize = 1 << (32 - PAGE_SHIFT);
/// Bytes per watched code granule, as a shift. The native store guard tests one bit per granule,
/// so this is the resolution at which a guest store is judged to have hit code.
///
/// **Lowered from 4 (16 bytes) to 2 (4 bytes) on measurement, and then to 0 (one byte) once the
/// store guard stopped costing a probe per granule.** At 16 bytes a store landing merely NEAR a
/// compiled instruction is admitted to `invalidate_physical_range`, which then walks every block
/// key on the page to find no overlap. Measured on NASCAR Racing 1 at 586: 5,702,773 such calls
/// examining 4,471,919,398 keys, of which 473,846 overlapped, so 99.99% of that work was wasted
/// and the function was 57.3% of wall.
///
/// Simulated skip rate against granularity, taken inside the real scan, counting the calls a watch
/// of each width would never have admitted:
///
/// | bytes | 16 | 8 | 4 | 2 | 1 |
/// |---|---|---|---|---|---|
/// | NASCAR | 0.00% | 33.90% | 75.42% | 83.27% | **94.08%** |
/// | Quake | 0.00% | 0.02% | 15.83% | 40.14% | 68.52% |
/// | Doom | 0.00% | 0.00% | 0.00% | 0.00% | 0.00% |
///
/// The last column was then measured rather than argued. NASCAR's real-time factor went 0.358 to
/// 0.432, another 21% on top of what 4 bytes had already taken. Doom moved +0.8% in wall with its
/// timedemo realtics EXACT, and Quake and GP2 stayed inside noise. Peak working set grew by 1.8 MB
/// on the worst fixture. Doom reads zero skip at every width because its invalidations are genuine
/// renderer self-patches; there is nothing there to skip, so it pays the footprint and gains
/// nothing. That is the trade, and at 1.8 MB it is a cheap one.
///
/// Read the 1.8 MB as a fixture measurement, not as a bound. Worst-case RETENTION at one byte is
/// about 20 MiB: `MAX_INACTIVE_PAGES` pages at roughly 16.9 KiB each, plus the recycled pool, plus
/// the sticky precise masks. No fixture has come near it, but nothing in the code caps it lower.
///
/// What unlocked the last step was the emitted guard's SHAPE, not the table. The guard used to
/// probe one mask bit per granule an access spans, so at one-byte granules `FSTP TBYTE` became ten
/// probes and blocks blew the one-host-page install limit: the full Doom loop emitted 4039 bytes
/// and five size tests failed, which on the oracles would have meant blocks silently REFUSED for
/// size. `emit_code_watch_table_branch` now tests one 32-bit window over the mask for any span of
/// two or more granules, which is constant-size in the access width, so byte granularity became
/// strictly better instead of a trade against emitted size.
///
/// `WatchPage` scales with this: `refs` is `[u32; CHUNKS_PER_PAGE]`, so a watched page goes from
/// about 1 KiB at 16-byte granules to about 16.9 KiB at one byte. `acquire_range` and
/// `release_range` also step one granule per BYTE per install and retire, which is compile-time
/// work rather than per-store work.
const CHUNK_SHIFT: u32 = 0;

/// `CHUNK_SHIFT` for the emitter, which bakes it into the native store guard's bit index. Exported
/// so there is ONE definition: the guard used to write the shift as a literal, and a literal that
/// silently disagrees with the table it indexes is a miscompile rather than a build failure.
/// Mirrors how `fast_map` exports `NATIVE_PAGE_SHIFT` for the same reason.
pub(crate) const NATIVE_CHUNK_SHIFT: u32 = CHUNK_SHIFT;
const CHUNKS_PER_PAGE: usize = 1 << (PAGE_SHIFT - CHUNK_SHIFT);
const MASK_WORDS: usize = CHUNKS_PER_PAGE / u64::BITS as usize;
const MAX_RECYCLED_PAGES: usize = 64;
const MAX_INACTIVE_PAGES: usize = 1024;
const MAX_STICKY_PRECISE_PAGES: usize = 4096;
const STICKY_LOW_PRECISE_PAGES: u32 = (2 << 20) >> PAGE_SHIFT;
const MAX_STICKY_HIGH_PRECISE_PAGES: usize =
    MAX_STICKY_PRECISE_PAGES - STICKY_LOW_PRECISE_PAGES as usize;
const PAGE_BITMAP_WORDS: usize = PAGE_COUNT / u64::BITS as usize;

// The emitted store guard indexes a page's mask with `(address & 0xfff) >> CHUNK_SHIFT`, so the
// mask has to hold every granule a page can contain. Both hold for any shift in 0..=6 and both
// exist so a shift change that broke them would fail the build rather than index off the end of a
// `WatchPage` at runtime, which is unobservable until the wrong bit answers.
const _: () = assert!(
    MASK_WORDS >= 1,
    "a page's chunk mask needs at least one word"
);
const _: () = assert!(
    (0xfff_usize >> CHUNK_SHIFT) < MASK_WORDS * u64::BITS as usize,
    "the last byte of a page must index a bit inside the chunk mask"
);

// The guard counts the granules an access spans at EMIT time, from the width alone:
// `n = ((width.bytes() - 1) >> CHUNK_SHIFT) + 1`. That count is independent of the access's offset
// only while the backend's alignment guarantee keeps every legal access inside one granule walk.
// `emit_wide_page_guard` refuses unaligned accesses and the strongest alignment it promises is 4
// bytes, so at a shift of 3 a 4-aligned Qword sitting at offset 4 mod 8 spans TWO granules while
// the formula says one, and the second granule's bit is never tested. That is a missed
// invalidation: no fault, no counter, nothing until the overwritten code runs. Pin the
// precondition here, next to the shift it constrains, rather than in the emitter that consumes it.
// Written against the granule SIZE rather than as `CHUNK_SHIFT <= 2` because at a shift of 0 the
// latter compares against the type's minimum and clippy rejects it as always true.
const _: () = assert!(
    (1_usize << CHUNK_SHIFT) <= 4,
    "the emitted guard's granule count is offset-independent only up to 4-byte granules"
);

/// A page's granule bits, plus ONE trailing word of padding that is never set.
///
/// The pad exists for the emitted guard. For any access spanning two or more granules the guard
/// loads a 32-bit window over the mask starting at the byte holding the first granule's bit, so an
/// access in the page's last granules reads up to three bytes past the last real mask byte; the
/// pad makes that read land in storage this type owns. Nothing can write it: every mark path
/// derives its bit index from `(physical & 0xfff) >> CHUNK_SHIFT`, which the const assert above
/// pins inside the first `MASK_WORDS` words. Reading zeros out there can only report a MISS on
/// bits that do not exist, and every real bit of the access is inside the window by construction.
type ChunkMask = [u64; MASK_WORDS + 1];

/// Every real granule of a page watched, with the pad word left CLEAR.
///
/// A coarse page is matched by pointer identity and the guard's window only ever tests bit
/// positions that correspond to real granules, so the pad's value cannot change an answer either
/// way. Zero is chosen so that EVERY `ChunkMask` in the program has a zero pad, which is what lets
/// the `mask.iter().all(|word| *word == 0)` sweeps over recycled and reset masks keep iterating
/// the whole array rather than carrying an exception for the last word. Written out by hand
/// because the obvious `[u64::MAX; MASK_WORDS + 1]` would set the pad as well.
static ALL_CHUNKS_WATCHED: ChunkMask = all_chunks_watched();

const fn all_chunks_watched() -> ChunkMask {
    let mut mask = [u64::MAX; MASK_WORDS + 1];
    mask[MASK_WORDS] = 0;
    mask
}

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
                .unwrap_or_else(|| Box::new([0; MASK_WORDS + 1]));
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
            mask: [0; MASK_WORDS + 1],
            refs: [0; CHUNKS_PER_PAGE],
            active_chunks: 0,
        }
    }
}

impl WatchPage {
    fn reset(&mut self) {
        // Iterates the padded length, which is safe precisely because the pad word is always
        // zero: the `word_index * 64 + bit` index into `refs` would be out of bounds for it, and
        // the inner loop never runs on a zero word, so it is never computed.
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
    /// Identity-free zeroed pages retained across hot global clears. Sixty-four pages cover the
    /// observed code working set without unbounded growth. Their cost scales with `CHUNK_SHIFT`:
    /// about 1.1 MiB per watch at one-byte granules, against 271 KiB at 4 bytes and 68 KiB when
    /// granules were 16 bytes.
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
        if len == 0 || self.pages.is_empty() {
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
