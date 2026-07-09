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
pub(crate) const TLB_ENTRIES: usize = 64;
pub(crate) const PREFETCH_WINDOW_BYTES: usize = 32;
pub(crate) const TRACKED_WRITE_PAGES: usize = 8;
pub(crate) const DIRECT_PAGE_CACHE_LINES: usize = 64;
pub(crate) const FETCH_PAGE_CACHE_ENTRIES: usize = 4;

// --- TLB ---

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

/// Direct-mapped linear->physical translation cache. `generation` bumps to flush
/// in O(1); an entry is live only while its `generation` matches. Contents are
/// microarchitectural, so the TLB is transparent to CpuGsw equality and prints
/// terse. Non-snooping, which matches real x86: a guest must INVLPG or reload CR3
/// after editing a page-table entry, and IzarraVM flushes on exactly those events.
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
    pub(crate) fn insert(&mut self, page: u32, phys: u32, writable: bool, user: bool, dirty: bool) {
        self.entries[Self::slot(page)] = TlbEntry {
            tag: page,
            phys,
            generation: self.generation,
            writable,
            user,
            dirty,
        };
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

// --- Direct Page Cache (repr(C) for JIT probe compatibility) ---

// repr(C) pins the field layout (physical_page at +0, ptr at +8, size 16) that the native memory
// probe's emitted deref hardcodes; default repr may reorder these, which in a release build (no
// debug_assert) would silently read the wrong field. See jit::block::emit_load_u8_probe.
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
}

impl Default for DirectPageCache {
    fn default() -> Self {
        Self {
            entries: [DirectPageCacheEntry::default(); DIRECT_PAGE_CACHE_LINES],
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
        if entry.physical_page == page {
            Some(entry)
        } else {
            None
        }
    }

    #[inline]
    pub(crate) fn insert(&mut self, page: DirectPage) {
        self.entries[Self::slot(page.physical_page)] = DirectPageCacheEntry {
            physical_page: page.physical_page,
            ptr: page.ptr,
        };
    }

    #[inline]
    pub(crate) fn invalidate(&mut self) {
        self.entries.fill(DirectPageCacheEntry::default());
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
