// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Paging, TLB, and direct page caches for fast physical memory access.
//!
//! This module holds the microarchitectural caches (TLB, DirectPageCache,
//! CodePageCache, FetchPageCache, PrefetchWindow) and the page translation
//! machinery. These are transparent to CpuGsw equality (always-equal impls)
//! and are used by both the interpreter path and the JIT paged probes.

use izarravm_bus::DirectPage;

use crate::SegmentRegister;

// The types below are pub(crate) so CpuGsw in lib.rs can name the fields.

// --- Constants ---
/// READ THE CONSUMER BEFORE READING THE SIZE. This is not only a translation cache. With paging
/// on, `fast_map_permissions` (memory.rs) refuses to publish a page into the JIT's linear fast map
/// unless `Tlb::lookup` hits, and every insert that displaces a non-same-residency entry unpublishes
/// the victim's fast-map page. So the set of pages the Direct backend can reach natively is bounded
/// by this constant, direct-mapped: `slot` is `page & (TLB_ENTRIES - 1)`, which at 64 entries
/// discriminates only linear address bits 12 through 17, and pages 256 KiB apart collide. A native
/// access to an unpublished page takes a side exit and a full round trip back into Rust.
///
/// A size sweep on Quake/586 (6.2B cycles, one discarded warmup and one observation per size,
/// each self-consistent) measured that coupling directly:
///
///   entries   unavailable-or-kind exits   side exits    direct entries   insns/entry   coverage
///        64                 39,830,978    40,708,403      157,638,200         17.80     80.54%
///       256                  3,671,362     4,548,908      125,604,995         22.71     81.56%
///      1024                    421,795     1,298,782      122,581,197         23.31     81.66%
///      4096                    184,740     1,062,059      122,216,774         23.39     81.67%
///
/// 1024 is the knee: 4096 buys 0.08 instructions per entry and 0.01 coverage points for four times
/// the memory. Removing 39.4M of the 40.7M native side exits is worth more than every opcode
/// lowering merged into this backend so far, which together moved instructions per entry 12.16 to
/// 17.80.
///
/// NOT free, and not purely microarchitectural. Guest ARCHITECTURAL state is untouched for a
/// correctly flushing guest: a hit and a walk resolve the same physical address. Two caveats keep
/// that from being the flat claim it looks like. A hit skips charged `PageWalkRead` bus cycles, so
/// charged clocks, retired instructions inside a fixed cycle budget, and in-guest frame rate all
/// move. And a walk writes the PDE and PTE accessed bit, plus the dirty bit on a write, back into
/// guest RAM, which a hit skips, so those bytes are refreshed on a different schedule at a
/// different size. That only diverges for a guest that clears A or D without an INVLPG or a CR3
/// reload, which is the undefined-behaviour case the `Tlb` caveat below describes.
///
/// Measured: Quake/586 43.2 to 43.5 fps and Doom/586 833 to 828 realtics, both still inside the
/// gate's existing bands, with frames and gametics unchanged. That is the "timing approx" half of
/// the contract, not a correctness change.
///
/// Two costs to know before raising this further. `Tlb` is an inline array inside `CpuGsw`, which is
/// itself inline in `Machine` and threaded by value, so 8192 entries overflows the main thread's
/// stack during construction (4096 was measured working). And the canonical execution payload
/// serializes every live entry, so its maximum grows with this constant.
pub const TLB_ENTRIES: usize = 1024;
// `Tlb::slot` masks with `TLB_ENTRIES - 1`, and jit/block.rs emits that same mask into native code.
// Both are silently wrong for a non-power-of-two, and the sweep table above invites tuning this.
const _: () = assert!(TLB_ENTRIES.is_power_of_two());
pub(crate) const PREFETCH_WINDOW_BYTES: usize = 32;
pub(crate) const TRACKED_WRITE_PAGES: usize = 8;
pub(crate) const DIRECT_PAGE_CACHE_LINES: usize = 64;
pub(crate) const FETCH_PAGE_CACHE_ENTRIES: usize = 4;

// --- TLB ---

// The JIT's paged memory probe emits the entry stride and field offsets directly.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct TlbEntry {
    /// Linear page number (linear >> 12). Meaningful only when `generation` is current.
    pub(crate) tag: u32,
    /// Physical page base (pte & 0xffff_f000).
    pub(crate) phys: u32,
    /// Live only while equal to the owning Tlb's `generation`.
    pub(crate) generation: u32,
    /// Cached combined PDE&PTE R/W bit (page is writable).
    pub(crate) writable: bool,
    /// Cached combined PDE&PTE U/S bit (page is user-accessible).
    pub(crate) user: bool,
    /// PTE dirty bit already set, so a write hit needs no page-table update.
    pub(crate) dirty: bool,
}

impl TlbEntry {
    pub(crate) const EMPTY: Self = Self {
        tag: 0,
        phys: 0,
        generation: 0,
        writable: false,
        user: false,
        dirty: false,
    };
}

/// Direct-mapped linear-to-physical translation cache. `generation` bumps to flush
/// in O(1); an entry is live only while its tag and `generation` match. Contents are
/// microarchitectural, so the TLB is transparent to CpuGsw equality and prints
/// terse. Non-snooping, which matches real x86: a guest must INVLPG or reload CR3
/// after editing a page-table entry, and IzarraVM flushes on exactly those events.
///
/// Size caveat on that last sentence. The POLICY matches real x86; the CAPACITY does not. A 386 or
/// 486 has on the order of 32 entries and a Pentium 64, set-associative, while this models 1024
/// direct-mapped for the reason in `TLB_ENTRIES` above. A guest that edits a page-table entry and
/// omits the required INVLPG is already relying on undefined behaviour, but on real hardware it
/// often survives because the small TLB evicts the stale entry within a few hundred pages. Here it
/// may not. If a paged title regresses in a way that looks like stale translations, this is the
/// first place to look; the corpus pass for this constant covered Quake under CWSDPMI and Doom
/// under JEMMEX.
///
/// One half of that hazard is closed at the consumer: `translate_linear_checked` never raises a
/// page fault from a hit, only from the walk, so an entry that is stale in the RESTRICTIVE
/// direction (the guest relaxed a PTE and skipped its flush) costs a walk instead of a spurious
/// #PF. The permissive direction -- the guest tightened a PTE and skipped its flush -- is still
/// live, and is the one this capacity note is about.
#[derive(Clone)]
pub(crate) struct Tlb {
    pub(crate) entries: [TlbEntry; TLB_ENTRIES],
    pub(crate) generation: u32,
}

impl Default for Tlb {
    fn default() -> Self {
        Self {
            entries: [TlbEntry::EMPTY; TLB_ENTRIES],
            generation: 1,
        }
    }
}

impl Tlb {
    #[inline]
    pub(crate) fn slot(page: u32) -> usize {
        (page as usize) & (TLB_ENTRIES - 1)
    }

    #[inline]
    pub(crate) fn lookup(&self, page: u32) -> Option<TlbEntry> {
        let e = self.entries[Self::slot(page)];
        (e.generation == self.generation && e.tag == page).then_some(e)
    }

    #[inline]
    pub(crate) fn insert(
        &mut self,
        page: u32,
        phys: u32,
        writable: bool,
        user: bool,
        dirty: bool,
    ) -> Option<TlbEntry> {
        let slot = Self::slot(page);
        let previous = self.entries[slot];
        self.entries[slot] = TlbEntry {
            tag: page,
            phys,
            generation: self.generation,
            writable,
            user,
            dirty,
        };
        (previous.generation == self.generation).then_some(previous)
    }

    #[inline]
    pub(crate) fn invalidate(&mut self, page: u32) {
        let slot = Self::slot(page);
        let entry = self.entries[slot];
        if entry.generation == self.generation && entry.tag == page {
            self.entries[slot] = TlbEntry::EMPTY;
        }
    }

    /// Drop every cached translation (CR0/CR3 write, task switch, INVLPG). The rare
    /// generation wrap clears the table so stale gen-0 entries cannot alias.
    pub(crate) fn flush(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.entries = [TlbEntry::EMPTY; TLB_ENTRIES];
            self.generation = 1;
        }
    }
}

impl PartialEq for Tlb {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for Tlb {}

impl std::fmt::Debug for Tlb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Tlb {{ {TLB_ENTRIES} entries }}")
    }
}

// --- Direct Page Cache ---

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct DirectPageCacheEntry {
    pub(crate) physical_page: u32,
    pub(crate) ptr: *mut u8,
}

impl Default for DirectPageCacheEntry {
    fn default() -> Self {
        Self {
            physical_page: u32::MAX,
            ptr: std::ptr::null_mut(),
        }
    }
}

pub(crate) struct DirectPageCache {
    pub(crate) entries: [DirectPageCacheEntry; DIRECT_PAGE_CACHE_LINES],
    mapping_epoch: u64,
}

impl Default for DirectPageCache {
    fn default() -> Self {
        Self {
            entries: [DirectPageCacheEntry::default(); DIRECT_PAGE_CACHE_LINES],
            mapping_epoch: 0,
        }
    }
}

impl DirectPageCache {
    #[inline]
    pub(crate) fn slot(page: u32) -> usize {
        ((page >> 12) as usize) & (DIRECT_PAGE_CACHE_LINES - 1)
    }

    #[inline]
    pub(crate) fn get(&self, physical: u32) -> Option<DirectPageCacheEntry> {
        let page = physical & !0x0fff;
        let entry = self.entries[Self::slot(page)];
        (entry.physical_page == page).then_some(entry)
    }

    #[inline]
    pub(crate) fn insert(&mut self, page: DirectPage) {
        if self.mapping_epoch != page.mapping_epoch {
            self.entries.fill(DirectPageCacheEntry::default());
            self.mapping_epoch = page.mapping_epoch;
        }
        self.entries[Self::slot(page.physical_page)] = DirectPageCacheEntry {
            physical_page: page.physical_page,
            ptr: page.ptr,
        };
    }

    #[inline]
    pub(crate) fn mapping_epoch(&self) -> u64 {
        self.mapping_epoch
    }

    #[inline]
    pub(crate) fn invalidate(&mut self) {
        self.entries.fill(DirectPageCacheEntry::default());
        self.mapping_epoch = 0;
    }

    #[inline]
    #[cfg(test)]
    pub(crate) fn invalidate_physical_range(&mut self, start: u32, end: u32) {
        for entry in &mut self.entries {
            if (start..end).contains(&entry.physical_page) {
                *entry = DirectPageCacheEntry::default();
            }
        }
    }
}

impl Clone for DirectPageCache {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl PartialEq for DirectPageCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for DirectPageCache {}

impl std::fmt::Debug for DirectPageCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DirectPageCache")
    }
}

#[cfg(test)]
#[path = "paging_test.rs"]
mod page_map_tests;

// --- Code Page Cache (for fetch) ---

#[derive(Clone, Default)]
pub(crate) struct CodePageCache {
    pub(crate) valid: bool,
    pub(crate) cs: SegmentRegister,
    pub(crate) linear_page: u32,
    pub(crate) physical_page: u32,
}

impl PartialEq for CodePageCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for CodePageCache {}

impl std::fmt::Debug for CodePageCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CodePageCache")
    }
}

// --- Prefetch Window ---

#[derive(Clone)]
pub(crate) struct PrefetchWindow {
    pub(crate) bytes: [u8; PREFETCH_WINDOW_BYTES],
    pub(crate) cs: SegmentRegister,
    pub(crate) linear_base: u32,
    pub(crate) physical_base: u32,
    pub(crate) len: u8,
}

impl Default for PrefetchWindow {
    fn default() -> Self {
        Self {
            bytes: [0; PREFETCH_WINDOW_BYTES],
            cs: SegmentRegister::default(),
            linear_base: 0,
            physical_base: 0,
            len: 0,
        }
    }
}

impl PrefetchWindow {
    pub(crate) fn invalidate(&mut self) {
        self.len = 0;
    }

    pub(crate) fn get(&self, cs: SegmentRegister, linear: u32) -> Option<(u8, u32)> {
        if self.cs != cs {
            return None;
        }
        let offset = linear.checked_sub(self.linear_base)? as usize;
        if offset < usize::from(self.len) {
            Some((self.bytes[offset], self.physical_base + offset as u32))
        } else {
            None
        }
    }

    pub(crate) fn physical_page(&self) -> Option<u32> {
        (self.len != 0).then_some(self.physical_base >> 12)
    }

    /// Whether a wrapping physical write range touches bytes held in this snapshot. Device writes
    /// use this to preserve an unrelated prefetch window while still observing DMA over code.
    pub(crate) fn overlaps_physical_range(&self, physical: u32, width: u32) -> bool {
        self.len != 0
            && width != 0
            && (0..u32::from(self.len)).any(|offset| {
                self.physical_base
                    .wrapping_add(offset)
                    .wrapping_sub(physical)
                    < width
            })
    }
}

impl PartialEq for PrefetchWindow {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for PrefetchWindow {}

impl std::fmt::Debug for PrefetchWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PrefetchWindow {{ {} bytes }}", self.len)
    }
}

// --- Fetch Page Cache ---

/// Number of entries in the instruction-fetch page cache (direct-mapped by linear page).
/// A single entry thrashed at every EIP page-boundary crossing (60M misses on Doom demo3);
/// 4 entries cover a function that spans up to 3 page boundaries (common at >4 KiB or when
/// code sits near a page edge) without eviction. Power of two for the slot mask.
#[derive(Clone, Copy)]
pub(crate) struct FetchPageCacheEntry {
    pub(crate) valid: bool,
    pub(crate) cs: SegmentRegister,
    pub(crate) linear_page: u32,
    pub(crate) physical_page: u32,
    pub(crate) ptr: *mut u8,
    pub(crate) len: usize,
}

impl Default for FetchPageCacheEntry {
    fn default() -> Self {
        Self {
            valid: false,
            cs: SegmentRegister::default(),
            linear_page: 0,
            physical_page: 0,
            ptr: std::ptr::null_mut(),
            len: 0,
        }
    }
}

#[derive(Default)]
pub(crate) struct FetchPageCache {
    pub(crate) entries: [FetchPageCacheEntry; FETCH_PAGE_CACHE_ENTRIES],
}

impl FetchPageCache {
    #[inline]
    pub(crate) fn slot(linear: u32) -> usize {
        ((linear >> 12) as usize) & (FETCH_PAGE_CACHE_ENTRIES - 1)
    }

    #[inline]
    pub(crate) fn get(&self, cs: SegmentRegister, linear: u32) -> Option<(u8, u32)> {
        let offset = (linear & 0x0fff) as usize;
        let entry = &self.entries[Self::slot(linear)];
        if entry.valid
            && entry.cs == cs
            && entry.linear_page == (linear & !0x0fff)
            && offset < entry.len
        {
            let value = unsafe { *entry.ptr.add(offset) };
            Some((value, entry.physical_page + offset as u32))
        } else {
            None
        }
    }

    #[inline]
    pub(crate) fn put(&mut self, cs: SegmentRegister, linear: u32, page: DirectPage) {
        self.entries[Self::slot(linear)] = FetchPageCacheEntry {
            valid: true,
            cs,
            linear_page: linear & !0x0fff,
            physical_page: page.physical_page,
            ptr: page.ptr,
            len: page.len,
        };
    }

    pub(crate) fn invalidate(&mut self) {
        for e in &mut self.entries {
            e.valid = false;
        }
    }
}

impl PartialEq for FetchPageCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for FetchPageCache {}

impl std::fmt::Debug for FetchPageCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FetchPageCache")
    }
}

impl Clone for FetchPageCache {
    fn clone(&self) -> Self {
        Self::default()
    }
}
