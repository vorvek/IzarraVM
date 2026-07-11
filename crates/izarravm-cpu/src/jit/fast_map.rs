// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Linear-page map shared by the interpreter fill path and the direct x64 backend.
//!
//! The interpreter does not probe this map. It only installs a mapping when its existing
//! physical direct-page cache misses and the bus supplies a page pointer. Native memory
//! lowering can therefore use a flat linear lookup without adding work to interpreter hits.

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

const MODE13_BASE: u32 = 0x000a_0000;
const MODE13_END: u32 = 0x000b_0000;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageKind {
    Unavailable = 0,
    Ram = 1,
    Mode13 = 2,
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

struct FastMapStorage {
    read_biases: Box<[usize]>,
    write_biases: Box<[usize]>,
    physical_pages: Box<[u32]>,
    flags: Box<[u8]>,
    live_pages: Box<[u64]>,
    listed_pages: Box<[u64]>,
}

impl FastMapStorage {
    fn new() -> Self {
        Self {
            read_biases: vec![UNAVAILABLE_BIAS; LINEAR_PAGE_COUNT].into_boxed_slice(),
            write_biases: vec![UNAVAILABLE_BIAS; LINEAR_PAGE_COUNT].into_boxed_slice(),
            physical_pages: vec![UNAVAILABLE_PHYSICAL_PAGE; LINEAR_PAGE_COUNT].into_boxed_slice(),
            flags: vec![PageKind::Unavailable as u8; LINEAR_PAGE_COUNT].into_boxed_slice(),
            live_pages: vec![0; BITSET_WORDS].into_boxed_slice(),
            listed_pages: vec![0; BITSET_WORDS].into_boxed_slice(),
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
        self.flags[index] = PageKind::Unavailable as u8;
        Self::clear_bit(&mut self.live_pages, index);
    }
}

/// A 4 GiB linear address-space map with one structure-of-arrays slot per 4 KiB page.
/// Storage is lazy because an interpreter-only CPU never consumes the native lookup arrays.
#[derive(Default)]
pub(crate) struct FastMap {
    storage: Option<FastMapStorage>,
    populated_pages: Vec<u32>,
}

/// Stable SoA base addresses for the direct backend. Keeping these behind accessors avoids
/// coupling emitted code to Rust's layout for `FastMap` or `Option<FastMapStorage>`.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub(super) struct NativeMapBases {
    read_biases: usize,
    write_biases: usize,
    physical_pages: usize,
    flags: usize,
}

#[allow(dead_code)]
impl NativeMapBases {
    pub(super) const fn read_biases(self) -> usize {
        self.read_biases
    }

    pub(super) const fn write_biases(self) -> usize {
        self.write_biases
    }

    pub(super) const fn physical_pages(self) -> usize {
        self.physical_pages
    }

    pub(super) const fn flags(self) -> usize {
        self.flags
    }
}

#[allow(dead_code)]
pub(super) const NATIVE_PAGE_SHIFT: u32 = PAGE_SHIFT;
#[allow(dead_code)]
pub(super) const NATIVE_UNAVAILABLE_BIAS: usize = UNAVAILABLE_BIAS;
#[allow(dead_code)]
pub(super) const NATIVE_KIND_MASK: u8 = KIND_MASK;
#[allow(dead_code)]
pub(super) const NATIVE_RAM_KIND: u8 = PageKind::Ram as u8;
#[allow(dead_code)]
pub(super) const NATIVE_MODE13_KIND: u8 = PageKind::Mode13 as u8;
#[allow(dead_code)]
pub(super) const NATIVE_PAGE_WRITABLE: u8 = PAGE_WRITABLE;
#[allow(dead_code)]
pub(super) const NATIVE_PAGE_USER: u8 = PAGE_USER;

impl FastMap {
    /// Return stable array bases for native code generation after the first map fill.
    #[allow(dead_code)]
    pub(super) fn native_bases(&self) -> Option<NativeMapBases> {
        self.storage.as_ref().map(|storage| NativeMapBases {
            read_biases: storage.read_biases.as_ptr() as usize,
            write_biases: storage.write_biases.as_ptr() as usize,
            physical_pages: storage.physical_pages.as_ptr() as usize,
            flags: storage.flags.as_ptr() as usize,
        })
    }

    pub(crate) fn populate_read(
        &mut self,
        linear: u32,
        physical: u32,
        page: DirectPage,
        permissions: PagePermissions,
    ) -> bool {
        self.populate(linear, physical, page, permissions, false)
    }

    pub(crate) fn populate_write(
        &mut self,
        linear: u32,
        physical: u32,
        page: DirectPage,
        permissions: PagePermissions,
    ) -> bool {
        self.populate(linear, physical, page, permissions, true)
    }

    fn populate(
        &mut self,
        linear: u32,
        physical: u32,
        page: DirectPage,
        permissions: PagePermissions,
        write: bool,
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
        if write {
            // Canonical mode 13h has its own kind, but native stores remain disabled until
            // generated stores also perform VGA dirty tracking and frame-generation updates.
            if kind == PageKind::Ram {
                storage.write_biases[index] = bias;
                flags |= HAS_WRITE_BIAS;
            }
        } else {
            storage.read_biases[index] = bias;
            flags |= HAS_READ_BIAS;
        }

        storage.physical_pages[index] = physical_page;
        storage.flags[index] = flags;
        FastMapStorage::set_bit(&mut storage.live_pages, index);
        if !FastMapStorage::bit(&storage.listed_pages, index) {
            FastMapStorage::set_bit(&mut storage.listed_pages, index);
            self.populated_pages.push(index as u32);
        }
        true
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

    /// Clear only pages installed since the previous global invalidation.
    pub(crate) fn invalidate_all(&mut self) {
        let Some(storage) = self.storage.as_mut() else {
            return;
        };
        for page in self.populated_pages.drain(..) {
            let index = page as usize;
            storage.clear_entry(index);
            FastMapStorage::clear_bit(&mut storage.listed_pages, index);
        }
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
            flags: storage.flags[index],
        }
    }

    #[cfg(test)]
    pub(crate) fn has_read_mapping(&self, linear: u32) -> bool {
        self.entry(linear).read_bias != UNAVAILABLE_BIAS
    }

    #[cfg(test)]
    pub(crate) fn has_write_mapping(&self, linear: u32) -> bool {
        self.entry(linear).write_bias != UNAVAILABLE_BIAS
    }

    #[cfg(test)]
    pub(crate) fn is_allocated(&self) -> bool {
        self.storage.is_some()
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
            .finish()
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy)]
struct FastMapEntry {
    read_bias: usize,
    write_bias: usize,
    physical_page: u32,
    flags: u8,
}

#[cfg(test)]
impl FastMapEntry {
    const fn unavailable() -> Self {
        Self {
            read_bias: UNAVAILABLE_BIAS,
            write_bias: UNAVAILABLE_BIAS,
            physical_page: UNAVAILABLE_PHYSICAL_PAGE,
            flags: PageKind::Unavailable as u8,
        }
    }

    fn kind(self) -> PageKind {
        match self.flags & KIND_MASK {
            1 => PageKind::Ram,
            2 => PageKind::Mode13,
            _ => PageKind::Unavailable,
        }
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
mod tests {
    use super::*;

    fn page(bytes: &mut [u8; PAGE_SIZE], physical_page: u32, writable: bool) -> DirectPage {
        DirectPage {
            physical_page,
            ptr: bytes.as_mut_ptr(),
            len: bytes.len(),
            writable,
        }
    }

    #[test]
    fn ram_read_and_write_fill_independent_biases() {
        let mut bytes = Box::new([0u8; PAGE_SIZE]);
        let mut map = FastMap::default();
        let linear = 0xc123_4567;
        let physical = 0x0012_3567;

        assert!(map.populate_read(
            linear,
            physical,
            page(&mut bytes, 0x0012_3000, false),
            PagePermissions {
                writable: false,
                user: true,
            },
        ));
        let read = map.entry(linear);
        assert_eq!(read.kind(), PageKind::Ram);
        assert_eq!(read.physical_page, 0x0012_3000);
        assert_eq!(
            read.read_ptr(linear),
            Some(bytes.as_mut_ptr().wrapping_add(0x567))
        );
        assert_eq!(read.write_ptr(linear), None);
        assert!(!read.writable());
        assert!(read.user());

        assert!(map.populate_write(
            linear,
            physical,
            page(&mut bytes, 0x0012_3000, true),
            PagePermissions {
                writable: true,
                user: false,
            },
        ));
        let write = map.entry(linear);
        assert_eq!(
            write.read_ptr(linear),
            Some(bytes.as_mut_ptr().wrapping_add(0x567))
        );
        assert_eq!(
            write.write_ptr(linear),
            Some(bytes.as_mut_ptr().wrapping_add(0x567))
        );
        assert!(write.writable());
        assert!(!write.user());
    }

    #[test]
    fn mode13_is_distinct_and_does_not_expose_native_store_bias() {
        let mut bytes = Box::new([0u8; PAGE_SIZE]);
        let mut map = FastMap::default();
        let linear = 0x00aa_0123;
        let physical = MODE13_BASE + 0x123;

        assert!(map.populate_write(
            linear,
            physical,
            page(&mut bytes, MODE13_BASE, true),
            PagePermissions::UNPAGED,
        ));
        let entry = map.entry(linear);
        assert_eq!(entry.kind(), PageKind::Mode13);
        assert_eq!(entry.write_ptr(linear), None);

        assert!(map.populate_read(
            linear,
            physical,
            page(&mut bytes, MODE13_BASE, false),
            PagePermissions::UNPAGED,
        ));
        assert_eq!(
            map.entry(linear).read_ptr(linear),
            Some(bytes.as_mut_ptr().wrapping_add(0x123))
        );
    }

    #[test]
    fn invlpg_is_exact_and_refill_does_not_duplicate_population_list() {
        let mut first = Box::new([0u8; PAGE_SIZE]);
        let mut second = Box::new([0u8; PAGE_SIZE]);
        let mut map = FastMap::default();

        assert!(map.populate_read(
            0x1000,
            0x3000,
            page(&mut first, 0x3000, false),
            PagePermissions::UNPAGED,
        ));
        assert!(map.populate_read(
            0x2000,
            0x4000,
            page(&mut second, 0x4000, false),
            PagePermissions::UNPAGED,
        ));
        map.invalidate_page(0x1fff);
        assert_eq!(map.entry(0x1000).kind(), PageKind::Unavailable);
        assert_eq!(map.entry(0x2000).kind(), PageKind::Ram);

        assert!(map.populate_read(
            0x1000,
            0x3000,
            page(&mut first, 0x3000, false),
            PagePermissions::UNPAGED,
        ));
        assert_eq!(map.populated_pages.len(), 2);
    }

    #[test]
    fn global_invalidation_and_clone_leave_no_live_entries() {
        let mut bytes = Box::new([0u8; PAGE_SIZE]);
        let mut map = FastMap::default();
        assert!(map.populate_read(
            0xffff_f000,
            0x7000,
            page(&mut bytes, 0x7000, false),
            PagePermissions::UNPAGED,
        ));

        let clone = map.clone();
        assert_eq!(clone.entry(0xffff_f000).kind(), PageKind::Unavailable);
        assert!(clone.storage.is_none());

        map.invalidate_all();
        assert_eq!(map.entry(0xffff_f000).kind(), PageKind::Unavailable);
        assert!(map.populated_pages.is_empty());
    }
}
