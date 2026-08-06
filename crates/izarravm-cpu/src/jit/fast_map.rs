// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Linear-page map filled by the interpreter and consumed by the direct x64 backend.
//!
//! The interpreter publishes a mapping only after canonical paging and direct-page lookup succeed.
//! Native code can then use the pointer bias while the mapping remains shadowed by the modeled TLB.
//! Interpreter accesses continue through the canonical translation path.

use izarravm_bus::BusWidth;
use izarravm_bus::DirectPage;

const PAGE_SHIFT: u32 = 12;
const PAGE_SIZE: usize = 1 << PAGE_SHIFT;
const PAGE_MASK: u32 = PAGE_SIZE as u32 - 1;
pub(crate) const LINEAR_PAGE_COUNT: usize = 1 << (32 - PAGE_SHIFT);
const BITSET_WORDS: usize = LINEAR_PAGE_COUNT / u64::BITS as usize;

const UNAVAILABLE_BIAS: usize = usize::MAX;
const UNAVAILABLE_PHYSICAL_PAGE: u32 = u32::MAX;

const KIND_MASK: u8 = 0b0000_0011;
const HAS_READ_BIAS: u8 = 0b0000_0100;
const HAS_WRITE_BIAS: u8 = 0b0000_1000;
const PAGE_WRITABLE: u8 = 0b0001_0000;
const PAGE_USER: u8 = 0b0010_0000;
/// The watched-page bit (design: `dev_docs/2026-08-06-watched-page-bit-design.md`). CLEAR
/// promises that NEITHER code-watch table has an entry for this linear page's physical page, so
/// an emitted store may skip the code-watch guard entirely. SET promises nothing — the full
/// guard decides. Recomputed from both watch tables on EVERY `populate` (never carried through
/// the `same_mapping` access-flags reuse), and kept honest at watch-add edges by
/// `clear_entries_of_watch_edge_page`: a stale SET bit is a missed optimization, a stale CLEAR
/// bit is a missed SMC invalidation.
const PAGE_WATCHED: u8 = 0b0100_0000;

use crate::paging::{VGA_APERTURE_END as MODE13_END, VGA_APERTURE_START as MODE13_BASE};

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageKind {
    Unavailable = 0,
    Ram = 1,
    Mode13 = 2,
}

/// Decode the two `PageKind` bits packed into a stored `flags` byte. Shared by the production
/// `lookup_access` hit path and the test-only `FastMapEntry` inspector so the two never drift.
#[inline]
fn decode_kind(flags: u8) -> PageKind {
    match flags & KIND_MASK {
        x if x == PageKind::Ram as u8 => PageKind::Ram,
        x if x == PageKind::Mode13 as u8 => PageKind::Mode13,
        _ => PageKind::Unavailable,
    }
}

/// What one invalidation discarded. Diagnostic only.
///
/// Both fields count entries that were **live at the moment they were cleared**, never list
/// membership. That distinction is the whole value of the counter: `populated_pages` and
/// `vga_pages` are registries that outlive the entries they name -- `invalidate_page` (INVLPG) and
/// `invalidate_vga_pages` both clear an entry and leave it listed -- so counting the list would
/// charge dead indices, and would charge an aperture page twice once the scoped sweep leaves it
/// behind for the next global wipe to find.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct WipeExtent {
    /// Linear pages that were live and are now gone.
    pub(crate) pages: u64,
    /// The subset of those backed by the direct VGA aperture.
    pub(crate) vga_pages: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PagePermissions {
    pub(crate) writable: bool,
    pub(crate) user: bool,
}

impl PagePermissions {
    pub(crate) const UNPAGED: Self = Self {
        writable: true,
        user: true,
    };
}

/// One permission-checked lookup used to test the same entry rules as native code. The
/// interpreter's FastMap serve path (`CpuGsw::fast_map_data_slot`) consumes this directly; tests
/// additionally use the `read`/`write` helpers below to exercise it without a full CPU.
#[derive(Clone, Copy)]
pub(crate) struct FastMapAccess {
    physical: u32,
    ptr: *mut u8,
    kind: PageKind,
}

impl FastMapAccess {
    pub(crate) const fn physical(self) -> u32 {
        self.physical
    }

    pub(crate) const fn ptr(self) -> *mut u8 {
        self.ptr
    }

    /// Whether this hit resolved to the Mode 13h VGA aperture rather than plain RAM. The
    /// interpreter's fast-path tail uses this to decide whether it may take the flat RAM charge
    /// or must defer to the full `charge_direct_memory` for its video wait states.
    pub(crate) fn is_mode13(self) -> bool {
        self.kind == PageKind::Mode13
    }

    #[cfg(test)]
    #[inline]
    pub(crate) fn read(self, width: BusWidth) -> u32 {
        match width {
            BusWidth::Byte => unsafe { u32::from(*self.ptr) },
            BusWidth::Word => unsafe {
                u32::from(u16::from_le(std::ptr::read_unaligned(
                    self.ptr.cast::<u16>(),
                )))
            },
            BusWidth::Dword => unsafe {
                u32::from_le(std::ptr::read_unaligned(self.ptr.cast::<u32>()))
            },
        }
    }

    #[cfg(test)]
    #[inline]
    pub(crate) fn write(self, width: BusWidth, value: u32) {
        match width {
            BusWidth::Byte => unsafe { *self.ptr = value as u8 },
            BusWidth::Word => unsafe {
                std::ptr::write_unaligned(self.ptr.cast::<u16>(), (value as u16).to_le());
            },
            BusWidth::Dword => unsafe {
                std::ptr::write_unaligned(self.ptr.cast::<u32>(), value.to_le());
            },
        }
    }
}

struct FastMapStorage {
    read_biases: Box<[usize]>,
    write_biases: Box<[usize]>,
    physical_pages: Box<[u32]>,
    mapping_epochs: Box<[u64]>,
    flags: Box<[u8]>,
    live_pages: Box<[u64]>,
    listed_pages: Box<[u64]>,
    listed_vga_pages: Box<[u64]>,
}

impl FastMapStorage {
    fn new() -> Self {
        Self {
            read_biases: vec![UNAVAILABLE_BIAS; LINEAR_PAGE_COUNT].into_boxed_slice(),
            write_biases: vec![UNAVAILABLE_BIAS; LINEAR_PAGE_COUNT].into_boxed_slice(),
            physical_pages: vec![UNAVAILABLE_PHYSICAL_PAGE; LINEAR_PAGE_COUNT].into_boxed_slice(),
            mapping_epochs: vec![0; LINEAR_PAGE_COUNT].into_boxed_slice(),
            flags: vec![PageKind::Unavailable as u8; LINEAR_PAGE_COUNT].into_boxed_slice(),
            live_pages: vec![0; BITSET_WORDS].into_boxed_slice(),
            listed_pages: vec![0; BITSET_WORDS].into_boxed_slice(),
            listed_vga_pages: vec![0; BITSET_WORDS].into_boxed_slice(),
        }
    }

    #[inline]
    fn bit(bits: &[u64], index: usize) -> bool {
        bits[index / u64::BITS as usize] & (1u64 << (index % u64::BITS as usize)) != 0
    }

    #[inline]
    fn set_bit(bits: &mut [u64], index: usize) {
        bits[index / u64::BITS as usize] |= 1u64 << (index % u64::BITS as usize);
    }

    #[inline]
    fn clear_bit(bits: &mut [u64], index: usize) {
        bits[index / u64::BITS as usize] &= !(1u64 << (index % u64::BITS as usize));
    }

    #[inline]
    fn clear_entry(&mut self, index: usize) {
        self.read_biases[index] = UNAVAILABLE_BIAS;
        self.write_biases[index] = UNAVAILABLE_BIAS;
        self.physical_pages[index] = UNAVAILABLE_PHYSICAL_PAGE;
        self.mapping_epochs[index] = 0;
        self.flags[index] = PageKind::Unavailable as u8;
        Self::clear_bit(&mut self.live_pages, index);
    }
}

/// A 4 GiB linear address-space map with one structure-of-arrays slot per 4 KiB page.
/// Storage is lazy because accurate-timing 386 modes never consume the lookup arrays.
#[derive(Default)]
pub(crate) struct FastMap {
    storage: Option<FastMapStorage>,
    populated_pages: Vec<u32>,
    vga_pages: Vec<u32>,
}

/// Stable SoA base addresses for the direct backend. Keeping these behind accessors avoids
/// coupling emitted code to Rust's layout for `FastMap` or `Option<FastMapStorage>`.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct NativeMapBases {
    read_biases: usize,
    write_biases: usize,
    physical_pages: usize,
    mapping_epochs: usize,
    flags: usize,
}

#[allow(dead_code)]
impl NativeMapBases {
    pub(crate) const fn read_biases(self) -> usize {
        self.read_biases
    }

    pub(crate) const fn write_biases(self) -> usize {
        self.write_biases
    }

    pub(crate) const fn physical_pages(self) -> usize {
        self.physical_pages
    }

    pub(crate) const fn mapping_epochs(self) -> usize {
        self.mapping_epochs
    }

    pub(crate) const fn flags(self) -> usize {
        self.flags
    }
}

#[allow(dead_code)]
pub(crate) const NATIVE_PAGE_SHIFT: u32 = PAGE_SHIFT;
#[allow(dead_code)]
pub(crate) const NATIVE_UNAVAILABLE_BIAS: usize = UNAVAILABLE_BIAS;
#[allow(dead_code)]
pub(crate) const NATIVE_KIND_MASK: u8 = KIND_MASK;
#[allow(dead_code)]
pub(crate) const NATIVE_RAM_KIND: u8 = PageKind::Ram as u8;
#[allow(dead_code)]
pub(crate) const NATIVE_MODE13_KIND: u8 = PageKind::Mode13 as u8;
#[allow(dead_code)]
pub(crate) const NATIVE_PAGE_WRITABLE: u8 = PAGE_WRITABLE;
#[allow(dead_code)]
pub(crate) const NATIVE_PAGE_USER: u8 = PAGE_USER;
#[allow(dead_code)]
pub(crate) const NATIVE_PAGE_WATCHED: u8 = PAGE_WATCHED;

impl FastMap {
    /// Resolve a populated linear mapping for the interpreter without revisiting the small TLB.
    /// A write bias exists only after the page walker has committed the PTE dirty bit, so a hit is
    /// safe to use without losing accessed/dirty side effects. Protection is checked against the
    /// current accessor because CPL can change while a mapping remains live.
    #[inline]
    pub(crate) fn lookup_access(
        &self,
        linear: u32,
        mapping_epoch: u64,
        width: BusWidth,
        write: bool,
        user: bool,
        write_protect: bool,
    ) -> Option<FastMapAccess> {
        let offset = linear & PAGE_MASK;
        if offset
            .checked_add(width.bytes())
            .is_none_or(|end| end > PAGE_SIZE as u32)
            || matches!(width, BusWidth::Word) && linear & 1 != 0
            || matches!(width, BusWidth::Dword) && linear & 3 != 0
        {
            return None;
        }
        let index = (linear >> PAGE_SHIFT) as usize;
        let storage = self.storage.as_ref()?;
        if !FastMapStorage::bit(&storage.live_pages, index) {
            return None;
        }
        if storage.mapping_epochs[index] != mapping_epoch {
            return None;
        }
        let flags = storage.flags[index];
        let bias_available = if write {
            flags & HAS_WRITE_BIAS != 0 && storage.write_biases[index] != UNAVAILABLE_BIAS
        } else {
            flags & HAS_READ_BIAS != 0 && storage.read_biases[index] != UNAVAILABLE_BIAS
        };
        if !bias_available
            || user && flags & PAGE_USER == 0
            || write && (user || write_protect) && flags & PAGE_WRITABLE == 0
        {
            return None;
        }
        let physical_page = storage.physical_pages[index];
        if physical_page == UNAVAILABLE_PHYSICAL_PAGE {
            return None;
        }
        let bias = if write {
            storage.write_biases[index]
        } else {
            storage.read_biases[index]
        };
        Some(FastMapAccess {
            physical: physical_page | offset,
            ptr: bias.wrapping_add(linear as usize) as *mut u8,
            kind: decode_kind(flags),
        })
    }

    #[cfg(test)]
    #[inline]
    pub(crate) fn lookup_physical(
        &self,
        linear: u32,
        mapping_epoch: u64,
        write: bool,
        user: bool,
        write_protect: bool,
    ) -> Option<u32> {
        self.lookup_access(
            linear,
            mapping_epoch,
            BusWidth::Byte,
            write,
            user,
            write_protect,
        )
        .map(FastMapAccess::physical)
    }

    /// Whether the map has EVER been populated. `storage` allocates lazily inside `populate`
    /// (`get_or_insert_with`) and is never freed afterward -- `invalidate_page`/`invalidate_all`
    /// clear entries but leave `storage` itself `Some`. So this is `false` only before the FIRST
    /// successful population on this CPU; it stays `true` for the rest of the CPU's life after
    /// that, through every later invalidation and through a live GSW mode switch into a persona
    /// that can never populate again. It is NOT a live "can this persona hit right now" test --
    /// that condition is `CpuGsw::fast_map_serve_enabled` (memory.rs), a separately cached mirror
    /// of `fast_map_population_enabled()` refreshed at every state change that predicate depends
    /// on. Do not use `has_storage()` to gate the interpreter's serve path; it is kept as a
    /// coarser diagnostic ("has population ever run at all") for tests. No non-test caller
    /// currently exists; `#[allow(dead_code)]` matches this file's other test/native-only helpers.
    #[allow(dead_code)]
    #[inline]
    pub(crate) fn has_storage(&self) -> bool {
        self.storage.is_some()
    }

    /// Return stable array bases for native code generation after the first map fill.
    #[allow(dead_code)]
    pub(crate) fn native_bases(&self) -> Option<NativeMapBases> {
        self.storage.as_ref().map(|storage| NativeMapBases {
            read_biases: storage.read_biases.as_ptr() as usize,
            write_biases: storage.write_biases.as_ptr() as usize,
            physical_pages: storage.physical_pages.as_ptr() as usize,
            mapping_epochs: storage.mapping_epochs.as_ptr() as usize,
            flags: storage.flags.as_ptr() as usize,
        })
    }

    /// `page_watched` is the caller's answer to "does EITHER code-watch table hold an entry for
    /// this physical page" (`CpuGsw::physical_page_watched`). It is a parameter rather than a
    /// query this type makes itself because the watches live on `DecodeCache` and `JitState`,
    /// and because every caller being forced to answer is what keeps the test fixtures honest
    /// (design hazard H7): a fixture that populates around a watched page with `false` here is
    /// wiring the exact miscompile the production sweep exists to prevent.
    pub(crate) fn populate_read(
        &mut self,
        linear: u32,
        physical: u32,
        page: DirectPage,
        permissions: PagePermissions,
        page_watched: bool,
    ) -> bool {
        self.populate(linear, physical, page, permissions, false, page_watched)
    }

    pub(crate) fn populate_write(
        &mut self,
        linear: u32,
        physical: u32,
        page: DirectPage,
        permissions: PagePermissions,
        page_watched: bool,
    ) -> bool {
        self.populate(linear, physical, page, permissions, true, page_watched)
    }

    #[inline]
    pub(crate) fn has_read_mapping(&self, linear: u32, physical: u32) -> bool {
        let index = (linear >> PAGE_SHIFT) as usize;
        let Some(storage) = self.storage.as_ref() else {
            return false;
        };
        FastMapStorage::bit(&storage.live_pages, index)
            && storage.physical_pages[index] == physical & !PAGE_MASK
            && storage.flags[index] & HAS_READ_BIAS != 0
            && storage.read_biases[index] != UNAVAILABLE_BIAS
    }

    #[inline]
    pub(crate) fn has_write_mapping(&self, linear: u32, physical: u32) -> bool {
        let index = (linear >> PAGE_SHIFT) as usize;
        let Some(storage) = self.storage.as_ref() else {
            return false;
        };
        FastMapStorage::bit(&storage.live_pages, index)
            && storage.physical_pages[index] == physical & !PAGE_MASK
            && storage.flags[index] & HAS_WRITE_BIAS != 0
            && storage.write_biases[index] != UNAVAILABLE_BIAS
    }

    #[inline]
    pub(crate) fn has_read_mapping_at_epoch(
        &self,
        linear: u32,
        physical: u32,
        mapping_epoch: u64,
    ) -> bool {
        let index = (linear >> PAGE_SHIFT) as usize;
        self.has_read_mapping(linear, physical)
            && self
                .storage
                .as_ref()
                .is_some_and(|storage| storage.mapping_epochs[index] == mapping_epoch)
    }

    #[inline]
    pub(crate) fn has_write_mapping_at_epoch(
        &self,
        linear: u32,
        physical: u32,
        mapping_epoch: u64,
    ) -> bool {
        let index = (linear >> PAGE_SHIFT) as usize;
        self.has_write_mapping(linear, physical)
            && self
                .storage
                .as_ref()
                .is_some_and(|storage| storage.mapping_epochs[index] == mapping_epoch)
    }

    fn populate(
        &mut self,
        linear: u32,
        physical: u32,
        page: DirectPage,
        permissions: PagePermissions,
        write: bool,
        page_watched: bool,
    ) -> bool {
        let physical_page = physical & !PAGE_MASK;
        if page.ptr.is_null()
            || page.len < PAGE_SIZE
            || page.physical_page != physical_page
            || write && !page.writable
        {
            return false;
        }

        let index = (linear >> PAGE_SHIFT) as usize;
        let linear_page = linear & !PAGE_MASK;
        let kind = if (MODE13_BASE..MODE13_END).contains(&physical_page) {
            PageKind::Mode13
        } else {
            PageKind::Ram
        };
        let bias = (page.ptr as usize).wrapping_sub(linear_page as usize);
        let storage = self.storage.get_or_insert_with(FastMapStorage::new);

        let same_mapping = FastMapStorage::bit(&storage.live_pages, index)
            && storage.physical_pages[index] == physical_page
            && storage.mapping_epochs[index] == page.mapping_epoch
            && storage.flags[index] & KIND_MASK == kind as u8;
        let access_flags = if same_mapping {
            storage.flags[index] & (HAS_READ_BIAS | HAS_WRITE_BIAS)
        } else {
            storage.read_biases[index] = UNAVAILABLE_BIAS;
            storage.write_biases[index] = UNAVAILABLE_BIAS;
            0
        };

        let mut flags = kind as u8 | access_flags;
        if permissions.writable {
            flags |= PAGE_WRITABLE;
        }
        if permissions.user {
            flags |= PAGE_USER;
        }
        // Recomputed on EVERY populate, never carried through `access_flags` (design H4): a
        // same-mapping refill after an edge sweep's `clear_entry` must observe the CURRENT watch
        // state, not the byte the sweep just invalidated.
        if page_watched {
            flags |= PAGE_WATCHED;
        }
        if write {
            storage.write_biases[index] = bias;
            flags |= HAS_WRITE_BIAS;
        } else {
            storage.read_biases[index] = bias;
            flags |= HAS_READ_BIAS;
        }

        storage.physical_pages[index] = physical_page;
        storage.mapping_epochs[index] = page.mapping_epoch;
        storage.flags[index] = flags;
        FastMapStorage::set_bit(&mut storage.live_pages, index);
        if !FastMapStorage::bit(&storage.listed_pages, index) {
            FastMapStorage::set_bit(&mut storage.listed_pages, index);
            self.populated_pages.push(index as u32);
        }
        if kind == PageKind::Mode13 && !FastMapStorage::bit(&storage.listed_vga_pages, index) {
            FastMapStorage::set_bit(&mut storage.listed_vga_pages, index);
            self.vga_pages.push(index as u32);
        }
        true
    }

    /// The strict-edge sweep (watched-page-bit design D4, edges E1/E2): `physical_page_base`
    /// just crossed unwatched -> watched in one of the code-watch tables, so every LIVE entry
    /// mapping it whose `PAGE_WATCHED` bit is CLEAR must be invalidated before native code runs
    /// again — the next emitted store through such an entry would skip the guard the new watch
    /// needs. Entries whose bit is already SET are skipped (INV-W already holds for them),
    /// which is what makes doom's once-per-generation re-mark churn affordable when the lazy
    /// edges leave bits set. Invalidation, not patching: a cleared entry routes the in-flight
    /// block's next store to the `unavailable_or_kind` side exit, and the refill recomputes the
    /// bit (H4). Returns the number of entries cleared, for the edge counters.
    ///
    /// Dead listed entries carry `physical_pages == u32::MAX`, which never equals a real page
    /// base, so the physical compare is the liveness filter as well as the match.
    pub(crate) fn clear_unwatched_entries_of_physical_page(
        &mut self,
        physical_page_base: u32,
    ) -> u64 {
        let Some(storage) = self.storage.as_mut() else {
            return 0;
        };
        let mut cleared = 0;
        for &page in &self.populated_pages {
            let index = page as usize;
            if storage.physical_pages[index] != physical_page_base
                || storage.flags[index] & PAGE_WATCHED != 0
            {
                continue;
            }
            storage.clear_entry(index);
            cleared += 1;
        }
        cleared
    }

    /// Invalidate exactly one linear page. Its list membership remains until the next global
    /// invalidation so repeated INVLPG/refill cycles do not grow the populated-page list.
    pub(crate) fn invalidate_page(&mut self, linear: u32) {
        let index = (linear >> PAGE_SHIFT) as usize;
        if let Some(storage) = self.storage.as_mut()
            && FastMapStorage::bit(&storage.live_pages, index)
        {
            storage.clear_entry(index);
        }
    }

    /// Clear the compact set of linear aliases backed by the direct VGA aperture. Draining the
    /// registry lets an alias be listed again after the VGA plane or backing store changes.
    ///
    /// A linear page that has since been re-pointed away from the aperture is skipped rather than
    /// cleared -- its entry is a RAM mapping now and the aperture move says nothing about it -- but
    /// its registry bit is still dropped, so it is re-listed if it ever comes back.
    ///
    /// List membership in `populated_pages` is deliberately NOT dropped: an entry cleared here can
    /// be re-populated without being pushed twice, and the next global invalidation still finds it.
    pub(crate) fn invalidate_vga_pages(&mut self) -> WipeExtent {
        let Some(storage) = self.storage.as_mut() else {
            return WipeExtent::default();
        };
        let mut cleared = 0;
        for page in self.vga_pages.drain(..) {
            let index = page as usize;
            if FastMapStorage::bit(&storage.live_pages, index)
                && storage.flags[index] & KIND_MASK == PageKind::Mode13 as u8
            {
                storage.clear_entry(index);
                cleared += 1;
            }
            FastMapStorage::clear_bit(&mut storage.listed_vga_pages, index);
        }
        WipeExtent {
            pages: cleared,
            vga_pages: cleared,
        }
    }

    /// Clear only pages installed since the previous global invalidation. Returns what it threw
    /// away, so the audit counters can price a wipe: `pages` is the whole cost, `vga_pages` the
    /// part a VGA-scoped invalidation would still have had to pay.
    pub(crate) fn invalidate_all(&mut self) -> WipeExtent {
        let Some(storage) = self.storage.as_mut() else {
            return WipeExtent::default();
        };
        let mut extent = WipeExtent::default();
        for page in self.populated_pages.drain(..) {
            let index = page as usize;
            // Liveness, not list membership: an index the scoped aperture sweep or an INVLPG
            // already cleared is still listed here, and charging it again would inflate the very
            // counter the aperture-scoping work is judged by.
            if FastMapStorage::bit(&storage.live_pages, index) {
                extent.pages += 1;
                if storage.flags[index] & KIND_MASK == PageKind::Mode13 as u8 {
                    extent.vga_pages += 1;
                }
            }
            storage.clear_entry(index);
            FastMapStorage::clear_bit(&mut storage.listed_pages, index);
            FastMapStorage::clear_bit(&mut storage.listed_vga_pages, index);
        }
        self.vga_pages.clear();
        extent
    }

    /// Whether the live entry for `linear` carries `PAGE_WATCHED` — the watched-page-bit
    /// battery's inspector (a dead entry reads false).
    #[cfg(test)]
    pub(crate) fn page_watched_bit_for_test(&self, linear: u32) -> bool {
        let index = (linear >> PAGE_SHIFT) as usize;
        self.storage
            .as_ref()
            .is_some_and(|storage| storage.flags[index] & PAGE_WATCHED != 0)
    }

    #[cfg(test)]
    fn entry(&self, linear: u32) -> FastMapEntry {
        let index = (linear >> PAGE_SHIFT) as usize;
        let Some(storage) = self.storage.as_ref() else {
            return FastMapEntry::unavailable();
        };
        FastMapEntry {
            read_bias: storage.read_biases[index],
            write_bias: storage.write_biases[index],
            physical_page: storage.physical_pages[index],
            mapping_epoch: storage.mapping_epochs[index],
            flags: storage.flags[index],
        }
    }
}

impl Clone for FastMap {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl PartialEq for FastMap {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for FastMap {}

impl std::fmt::Debug for FastMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FastMap")
            .field("populated_pages", &self.populated_pages.len())
            .field("vga_pages", &self.vga_pages.len())
            .finish()
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy)]
struct FastMapEntry {
    read_bias: usize,
    write_bias: usize,
    physical_page: u32,
    mapping_epoch: u64,
    flags: u8,
}

#[cfg(test)]
impl FastMapEntry {
    const fn unavailable() -> Self {
        Self {
            read_bias: UNAVAILABLE_BIAS,
            write_bias: UNAVAILABLE_BIAS,
            physical_page: UNAVAILABLE_PHYSICAL_PAGE,
            mapping_epoch: 0,
            flags: PageKind::Unavailable as u8,
        }
    }

    fn kind(self) -> PageKind {
        decode_kind(self.flags)
    }

    fn read_ptr(self, linear: u32) -> Option<*mut u8> {
        (self.read_bias != UNAVAILABLE_BIAS)
            .then(|| self.read_bias.wrapping_add(linear as usize) as *mut u8)
    }

    fn write_ptr(self, linear: u32) -> Option<*mut u8> {
        (self.write_bias != UNAVAILABLE_BIAS)
            .then(|| self.write_bias.wrapping_add(linear as usize) as *mut u8)
    }

    fn writable(self) -> bool {
        self.flags & PAGE_WRITABLE != 0
    }

    fn user(self) -> bool {
        self.flags & PAGE_USER != 0
    }
}

#[cfg(test)]
#[path = "fast_map_test.rs"]
mod tests;
