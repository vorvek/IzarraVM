// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn direct_page_cache_entries_remain_compact() {
    assert_eq!(core::mem::size_of::<DirectPageCacheEntry>(), 16);
}

#[test]
fn direct_mapped_tlb_replaces_only_the_colliding_slot() {
    let mut tlb = Tlb::default();
    let first = 3;
    let collision = first + TLB_ENTRIES as u32;

    assert!(tlb.insert(first, 0x5000, true, false, true).is_none());
    let first_entry = tlb.lookup(first).unwrap();
    assert_eq!(first_entry.phys, 0x5000);
    assert!(first_entry.writable);
    assert!(!first_entry.user);
    assert!(first_entry.dirty);

    let evicted = tlb
        .insert(collision, 0x9000, false, true, false)
        .expect("colliding insert must expose the live entry");
    assert_eq!(evicted.tag, first);
    assert_eq!(evicted.phys, 0x5000);
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

    assert!(tlb.insert(collision, 0x9000, false, true, false).is_none());

    tlb.flush();
    assert!(tlb.lookup(collision).is_none());
}

#[test]
fn tlb_insert_exposes_same_tag_replacements_and_dirty_upgrades() {
    let mut tlb = Tlb::default();
    let page = 7;

    assert!(tlb.insert(page, 0x5000, true, true, false).is_none());
    let clean = tlb
        .insert(page, 0x5000, true, true, true)
        .expect("dirty upgrade must expose the clean entry");
    assert_eq!(clean.tag, page);
    assert_eq!(clean.phys, 0x5000);
    assert!(!clean.dirty);

    let old_mapping = tlb
        .insert(page, 0x9000, false, false, false)
        .expect("same-tag remap must expose the old mapping");
    assert_eq!(old_mapping.phys, 0x5000);
    assert!(old_mapping.writable);
    assert!(old_mapping.user);
    assert!(old_mapping.dirty);
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
        mapping_epoch: 1,
    });
    cache.insert(DirectPage {
        physical_page: 0xA_0000,
        ptr: vga.as_mut_ptr(),
        len: 0x1000,
        writable: true,
        mapping_epoch: 1,
    });

    cache.invalidate_physical_range(0xA_0000, 0xC_0000);

    assert!(cache.get(0x2000).is_some());
    assert!(cache.get(0xA_0000).is_none());
}

#[test]
fn physical_page_cache_tags_collisions_and_invalidates_all_entries() {
    let mut first_bytes = Box::new([0u8; 4096]);
    let mut other_bytes = Box::new([0u8; 4096]);
    let mut collision_bytes = Box::new([0u8; 4096]);
    let first = DirectPage {
        physical_page: 0x1000,
        ptr: first_bytes.as_mut_ptr(),
        len: first_bytes.len(),
        writable: true,
        mapping_epoch: 1,
    };
    let collision = DirectPage {
        physical_page: first.physical_page + (DIRECT_PAGE_CACHE_LINES as u32 * 0x1000),
        ptr: collision_bytes.as_mut_ptr(),
        len: collision_bytes.len(),
        writable: true,
        mapping_epoch: 2,
    };
    let other = DirectPage {
        physical_page: 0x2000,
        ptr: other_bytes.as_mut_ptr(),
        len: other_bytes.len(),
        writable: true,
        mapping_epoch: 1,
    };
    let mut pages = DirectPageCache::default();

    pages.insert(first);
    pages.insert(other);
    let cached_first = pages.get(first.physical_page).unwrap();
    assert_eq!(cached_first.ptr, first.ptr);
    assert_eq!(pages.mapping_epoch(), 1);
    assert!(pages.get(other.physical_page).is_some());

    pages.insert(collision);
    assert!(pages.get(first.physical_page).is_none());
    assert!(pages.get(other.physical_page).is_none());
    let cached_collision = pages.get(collision.physical_page).unwrap();
    assert_eq!(cached_collision.ptr, collision.ptr);
    assert_eq!(pages.mapping_epoch(), 2);

    pages.insert(first);
    assert!(pages.get(collision.physical_page).is_none());
    assert_eq!(pages.get(first.physical_page).unwrap().ptr, first.ptr);

    pages.invalidate();
    assert!(pages.get(first.physical_page).is_none());
    assert!(pages.get(collision.physical_page).is_none());
}
