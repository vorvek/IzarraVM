// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Linear-page map shared by the interpreter fill path and the direct x64 backend.
//!
//! In 486/586 modes the interpreter installs mappings from its physical direct-page cache, then
//! consumes the same pointer biases as native code. This removes the old 64-entry TLB and physical
//! page-cache bottleneck from ordinary warm RAM and canonical VGA accesses.

use izarravm_bus::{BusWidth, DirectPage};

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

/// One permission-checked interpreter hit. The raw host pointer stays inside this module; callers
/// observe the physical address, run their normal timing and SMC hooks, then commit the access.
#[derive(Clone, Copy)]
pub(crate) struct FastMapAccess {
    physical: u32,
    ptr: *mut u8,
}

impl FastMapAccess {
    pub(crate) const fn physical(self) -> u32 {
        self.physical
    }

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
    /// Resolve a populated linear mapping for the interpreter without revisiting the small TLB.
    /// A write bias exists only after the page walker has committed the PTE dirty bit, so a hit is
    /// safe to use without losing accessed/dirty side effects. Protection is checked against the
    /// current accessor because CPL can change while a mapping remains live.
    #[inline]
    pub(crate) fn lookup_access(
        &self,
        linear: u32,
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
        })
    }

    #[inline]
    pub(crate) fn lookup_physical(
        &self,
        linear: u32,
        write: bool,
        user: bool,
        write_protect: bool,
    ) -> Option<u32> {
        self.lookup_access(linear, BusWidth::Byte, write, user, write_protect)
            .map(FastMapAccess::physical)
    }

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
            storage.write_biases[index] = bias;
            flags |= HAS_WRITE_BIAS;
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
        if kind == PageKind::Mode13 && !FastMapStorage::bit(&storage.listed_vga_pages, index) {
            FastMapStorage::set_bit(&mut storage.listed_vga_pages, index);
            self.vga_pages.push(index as u32);
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

    /// Clear the compact set of linear aliases backed by the direct VGA aperture. Draining the
    /// registry lets an alias be listed again after the VGA plane or backing store changes.
    pub(crate) fn invalidate_vga_pages(&mut self) {
        let Some(storage) = self.storage.as_mut() else {
            return;
        };
        for page in self.vga_pages.drain(..) {
            let index = page as usize;
            if FastMapStorage::bit(&storage.live_pages, index)
                && storage.flags[index] & KIND_MASK == PageKind::Mode13 as u8
            {
                storage.clear_entry(index);
            }
            FastMapStorage::clear_bit(&mut storage.listed_vga_pages, index);
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
            FastMapStorage::clear_bit(&mut storage.listed_vga_pages, index);
        }
        self.vga_pages.clear();
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
    fn interpreter_lookup_requires_a_live_bias_and_current_permissions() {
        let mut bytes = Box::new([0u8; PAGE_SIZE]);
        let mut map = FastMap::default();
        let linear = 0x8123_4564;
        let physical = 0x0012_3564;
        bytes[0x564..0x568].copy_from_slice(&0x4433_2211u32.to_le_bytes());

        assert!(map.populate_read(
            linear,
            physical,
            page(&mut bytes, 0x0012_3000, false),
            PagePermissions {
                writable: false,
                user: false,
            },
        ));
        assert_eq!(
            map.lookup_physical(linear, false, false, false),
            Some(physical)
        );
        assert_eq!(map.lookup_physical(linear, false, true, false), None);
        assert_eq!(map.lookup_physical(linear, true, false, false), None);
        assert_eq!(
            map.lookup_access(linear, BusWidth::Dword, false, false, false)
                .unwrap()
                .read(BusWidth::Dword),
            0x4433_2211
        );

        assert!(map.populate_write(
            linear,
            physical,
            page(&mut bytes, 0x0012_3000, true),
            PagePermissions {
                writable: false,
                user: false,
            },
        ));
        assert_eq!(
            map.lookup_physical(linear, true, false, false),
            Some(physical)
        );
        assert_eq!(map.lookup_physical(linear, true, false, true), None);
        assert_eq!(map.lookup_physical(linear, true, true, false), None);
        map.lookup_access(linear, BusWidth::Dword, true, false, false)
            .unwrap()
            .write(BusWidth::Dword, 0xaabb_ccdd);
        assert_eq!(&bytes[0x564..0x568], &0xaabb_ccddu32.to_le_bytes());
        assert!(
            map.lookup_access(
                (linear & !PAGE_MASK) | 0xffe,
                BusWidth::Dword,
                false,
                false,
                false,
            )
            .is_none(),
            "cross-page accesses must retain the precise slow path"
        );

        map.invalidate_page(linear);
        assert_eq!(map.lookup_physical(linear, false, false, false), None);
    }

    #[test]
    fn mode13_is_distinct_and_exposes_native_store_bias() {
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
        assert_eq!(
            entry.write_ptr(linear),
            Some(bytes.as_mut_ptr().wrapping_add(0x123))
        );

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
    fn vga_invalidation_handles_aliases_and_refills_without_list_growth() {
        const PAGED_ALIAS: u32 = 0x8123_4000;
        let mut ram = Box::new([0u8; PAGE_SIZE]);
        let mut vga = Box::new([0u8; PAGE_SIZE]);
        let mut map = FastMap::default();
        assert!(map.populate_write(
            0x2000,
            0x2000,
            page(&mut ram, 0x2000, true),
            PagePermissions::UNPAGED,
        ));

        for _ in 0..3 {
            assert!(map.populate_write(
                MODE13_BASE,
                MODE13_BASE,
                page(&mut vga, MODE13_BASE, true),
                PagePermissions::UNPAGED,
            ));
            assert!(map.populate_read(
                PAGED_ALIAS,
                MODE13_BASE,
                page(&mut vga, MODE13_BASE, false),
                PagePermissions {
                    writable: true,
                    user: false,
                },
            ));
            assert_eq!(map.vga_pages.len(), 2);
            assert_eq!(map.populated_pages.len(), 3);
            assert!(map.has_write_mapping(MODE13_BASE, MODE13_BASE));
            assert!(map.has_read_mapping(PAGED_ALIAS, MODE13_BASE));

            map.invalidate_vga_pages();

            assert!(map.vga_pages.is_empty());
            assert!(map.has_write_mapping(0x2000, 0x2000));
            assert!(!map.has_write_mapping(MODE13_BASE, MODE13_BASE));
            assert!(!map.has_read_mapping(PAGED_ALIAS, MODE13_BASE));
        }
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
        let mut vga = Box::new([0u8; PAGE_SIZE]);
        let mut map = FastMap::default();
        assert!(map.populate_read(
            0xffff_f000,
            0x7000,
            page(&mut bytes, 0x7000, false),
            PagePermissions::UNPAGED,
        ));
        assert!(map.populate_write(
            MODE13_BASE,
            MODE13_BASE,
            page(&mut vga, MODE13_BASE, true),
            PagePermissions::UNPAGED,
        ));
        assert_eq!(map.vga_pages.len(), 1);

        let clone = map.clone();
        assert_eq!(clone.entry(0xffff_f000).kind(), PageKind::Unavailable);
        assert!(clone.storage.is_none());

        map.invalidate_all();
        assert_eq!(map.entry(0xffff_f000).kind(), PageKind::Unavailable);
        assert_eq!(map.entry(MODE13_BASE).kind(), PageKind::Unavailable);
        assert!(map.populated_pages.is_empty());
        assert!(map.vga_pages.is_empty());

        assert!(map.populate_write(
            MODE13_BASE,
            MODE13_BASE,
            page(&mut vga, MODE13_BASE, true),
            PagePermissions::UNPAGED,
        ));
        assert_eq!(map.populated_pages.len(), 1);
        assert_eq!(map.vga_pages.len(), 1);
    }
}
