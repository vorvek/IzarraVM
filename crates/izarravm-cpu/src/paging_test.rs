// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn direct_mapped_tlb_replaces_only_the_colliding_slot() {
    let mut tlb = Tlb::default();
    let first = 3;
    let collision = first + TLB_ENTRIES as u32;

    tlb.insert(first, 0x5000, true, false, true);
    let first_entry = tlb.lookup(first).unwrap();
    assert_eq!(first_entry.phys, 0x5000);
    assert!(first_entry.writable);
    assert!(!first_entry.user);
    assert!(first_entry.dirty);

    tlb.insert(collision, 0x9000, false, true, false);
    assert!(tlb.lookup(first).is_none());
    let collision_entry = tlb.lookup(collision).unwrap();
    assert_eq!(collision_entry.phys, 0x9000);
    assert!(!collision_entry.writable);
    assert!(collision_entry.user);
    assert!(!collision_entry.dirty);

    tlb.invalidate(first);
    assert!(tlb.lookup(first).is_none());
    assert!(tlb.lookup(collision).is_some());

    tlb.invalidate(collision);
    assert!(tlb.lookup(collision).is_none());

    tlb.insert(collision, 0x9000, false, true, false);

    tlb.flush();
    assert!(tlb.lookup(collision).is_none());
}

#[test]
fn direct_page_cache_range_invalidation_preserves_other_pages() {
    let mut low = [0u8; 0x1000];
    let mut vga = [0u8; 0x1000];
    let mut cache = DirectPageCache::default();
    cache.insert(DirectPage {
        physical_page: 0x2000,
        ptr: low.as_mut_ptr(),
        len: 0x1000,
        writable: true,
    });
    cache.insert(DirectPage {
        physical_page: 0xA_0000,
        ptr: vga.as_mut_ptr(),
        len: 0x1000,
        writable: true,
    });

    cache.invalidate_physical_range(0xA_0000, 0xC_0000);

    assert!(cache.get(0x2000).is_some());
    assert!(cache.get(0xA_0000).is_none());
}

#[test]
fn physical_page_cache_tags_collisions_and_invalidates_all_entries() {
    let mut first_bytes = Box::new([0u8; 4096]);
    let mut collision_bytes = Box::new([0u8; 4096]);
    let first = DirectPage {
        physical_page: 0x1000,
        ptr: first_bytes.as_mut_ptr(),
        len: first_bytes.len(),
        writable: true,
    };
    let collision = DirectPage {
        physical_page: first.physical_page + (DIRECT_PAGE_CACHE_LINES as u32 * 0x1000),
        ptr: collision_bytes.as_mut_ptr(),
        len: collision_bytes.len(),
        writable: true,
    };
    let mut pages = DirectPageCache::default();

    pages.insert(first);
    assert_eq!(pages.get(first.physical_page).unwrap().ptr, first.ptr);

    pages.insert(collision);
    assert!(pages.get(first.physical_page).is_none());
    assert_eq!(
        pages.get(collision.physical_page).unwrap().ptr,
        collision.ptr
    );

    pages.insert(first);
    assert!(pages.get(collision.physical_page).is_none());
    assert_eq!(pages.get(first.physical_page).unwrap().ptr, first.ptr);

    pages.invalidate();
    assert!(pages.get(first.physical_page).is_none());
    assert!(pages.get(collision.physical_page).is_none());
}
