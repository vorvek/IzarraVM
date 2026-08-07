// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const MAPPING_EPOCH: u64 = 7;

fn page(bytes: &mut [u8; PAGE_SIZE], physical_page: u32, writable: bool) -> DirectPage {
    DirectPage {
        physical_page,
        ptr: bytes.as_mut_ptr(),
        len: bytes.len(),
        writable,
        mapping_epoch: MAPPING_EPOCH,
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
        // Bare-map tests have no code watches; the PAGE_WATCHED bit's own coverage lives with the
        // watched-page-bit fixtures.
        false,
    ));
    let read = map.entry(linear);
    assert_eq!(read.kind(), PageKind::Ram);
    assert_eq!(read.physical_page, 0x0012_3000);
    assert_eq!(read.mapping_epoch, MAPPING_EPOCH);
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
        false,
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
        false,
    ));
    assert_eq!(
        map.lookup_physical(linear, MAPPING_EPOCH, false, false, false),
        Some(physical)
    );
    assert_eq!(
        map.lookup_physical(linear, MAPPING_EPOCH, false, true, false),
        None
    );
    assert_eq!(
        map.lookup_physical(linear, MAPPING_EPOCH, true, false, false),
        None
    );
    assert_eq!(
        map.lookup_access(linear, MAPPING_EPOCH, BusWidth::Dword, false, false, false,)
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
        false,
    ));
    assert_eq!(
        map.lookup_physical(linear, MAPPING_EPOCH, true, false, false),
        Some(physical)
    );
    assert_eq!(
        map.lookup_physical(linear, MAPPING_EPOCH, true, false, true),
        None
    );
    assert_eq!(
        map.lookup_physical(linear, MAPPING_EPOCH, true, true, false),
        None
    );
    map.lookup_access(linear, MAPPING_EPOCH, BusWidth::Dword, true, false, false)
        .unwrap()
        .write(BusWidth::Dword, 0xaabb_ccdd);
    assert_eq!(&bytes[0x564..0x568], &0xaabb_ccddu32.to_le_bytes());
    assert!(
        map.lookup_access(
            (linear & !PAGE_MASK) | 0xffe,
            MAPPING_EPOCH,
            BusWidth::Dword,
            false,
            false,
            false,
        )
        .is_none(),
        "cross-page accesses must retain the precise slow path"
    );

    map.invalidate_page(linear);
    assert_eq!(
        map.lookup_physical(linear, MAPPING_EPOCH, false, false, false),
        None
    );
}

#[test]
fn access_rejects_a_mapping_from_an_old_bus_epoch() {
    let mut bytes = Box::new([0u8; PAGE_SIZE]);
    let mut map = FastMap::default();
    let linear = 0x7123_4000;
    let physical = 0x0023_4000;

    assert!(map.populate_read(
        linear,
        physical,
        page(&mut bytes, physical, false),
        PagePermissions::UNPAGED,
        false,
    ));
    assert!(map.has_read_mapping_at_epoch(linear, physical, MAPPING_EPOCH));
    assert!(!map.has_read_mapping_at_epoch(linear, physical, MAPPING_EPOCH + 1));
    assert!(
        map.lookup_access(
            linear,
            MAPPING_EPOCH + 1,
            BusWidth::Byte,
            false,
            false,
            false,
        )
        .is_none()
    );
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
        false,
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
        false,
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
        false,
    ));

    for _ in 0..3 {
        assert!(map.populate_write(
            MODE13_BASE,
            MODE13_BASE,
            page(&mut vga, MODE13_BASE, true),
            PagePermissions::UNPAGED,
            false,
        ));
        assert!(map.populate_read(
            PAGED_ALIAS,
            MODE13_BASE,
            page(&mut vga, MODE13_BASE, false),
            PagePermissions {
                writable: true,
                user: false,
            },
            false,
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
        false,
    ));
    assert!(map.populate_read(
        0x2000,
        0x4000,
        page(&mut second, 0x4000, false),
        PagePermissions::UNPAGED,
        false,
    ));
    map.invalidate_page(0x1fff);
    assert_eq!(map.entry(0x1000).kind(), PageKind::Unavailable);
    assert_eq!(map.entry(0x2000).kind(), PageKind::Ram);

    assert!(map.populate_read(
        0x1000,
        0x3000,
        page(&mut first, 0x3000, false),
        PagePermissions::UNPAGED,
        false,
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
        false,
    ));
    assert!(map.populate_write(
        MODE13_BASE,
        MODE13_BASE,
        page(&mut vga, MODE13_BASE, true),
        PagePermissions::UNPAGED,
        false,
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
        false,
    ));
    assert_eq!(map.populated_pages.len(), 1);
    assert_eq!(map.vga_pages.len(), 1);
}
