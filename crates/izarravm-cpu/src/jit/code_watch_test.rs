// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::{HashMap, HashSet};

use super::*;

#[test]
fn sticky_decode_marks_are_idempotent_and_cover_every_touched_chunk() {
    let mut watch = StickyDecodeCodeWatch::default();
    watch.mark_range(0x10f, 34);
    watch.mark_range(0x10f, 34);

    assert!(watch.is_watched(0x100));
    assert!(watch.is_watched(0x110));
    assert!(watch.is_watched(0x120));
    assert!(watch.is_watched(0x130));
    assert!(!watch.is_watched(0x140));
    assert_eq!(watch.precise_pages(), 1);
    assert_eq!(watch.coarse_page_count(), 0);
}

#[test]
fn sticky_decode_pre_table_precise_and_coarse_marks_publish_and_clear() {
    let mut watch = StickyDecodeCodeWatch::default();
    let precise = 0x120;
    watch.mark_range(precise, 1);
    for page in 1..MAX_STICKY_PRECISE_PAGES as u32 {
        watch.mark_range(page << PAGE_SHIFT, 1);
    }
    let coarse = (MAX_STICKY_PRECISE_PAGES as u32) << PAGE_SHIFT;
    watch.mark_range(coarse, 1);
    watch.mark_range(coarse | 0x7f0, 1);
    assert_eq!(watch.precise_pages(), MAX_STICKY_PRECISE_PAGES);
    assert_eq!(watch.coarse_page_count(), 1);

    let base = watch.table_base();
    let precise_entry = unsafe { *(base as *const usize).add((precise >> PAGE_SHIFT) as usize) };
    let coarse_entry = unsafe { *(base as *const usize).add((coarse >> PAGE_SHIFT) as usize) };
    assert_ne!(precise_entry, 0);
    assert_ne!(precise_entry, StickyDecodeCodeWatch::coarse_pointer());
    assert_eq!(coarse_entry, StickyDecodeCodeWatch::coarse_pointer());

    watch.clear();
    assert_eq!(watch.table_base(), base);
    assert_eq!(unsafe { *(base as *const usize) }, 0);
    assert_eq!(
        unsafe { *(base as *const usize).add((coarse >> PAGE_SHIFT) as usize) },
        0
    );
    assert!(!watch.is_watched(precise));
    assert!(!watch.is_watched(coarse));
    assert_eq!(watch.precise_pages(), 0);
    assert_eq!(watch.coarse_page_count(), 0);
}

#[test]
fn sticky_decode_published_mask_survives_map_rehash() {
    let mut watch = StickyDecodeCodeWatch::default();
    let original = 0x10_020;
    watch.mark_range(original, 1);
    let base = watch.table_base();
    let entry = unsafe { (base as *const usize).add((original >> PAGE_SHIFT) as usize) };
    let pointer = unsafe { *entry };

    for page in 0x20..0x820u32 {
        watch.mark_range((page << PAGE_SHIFT) | 0x30, 1);
    }
    assert_eq!(unsafe { *entry }, pointer);
    assert!(watch.is_watched(original));
    watch.mark_range(original | 0xf0, 1);
    assert_eq!(unsafe { *entry }, pointer);
    assert!(watch.is_watched(original | 0xf0));
}

#[test]
fn sticky_decode_randomized_marks_and_clears_match_an_independent_model() {
    fn chunks(physical: u32, len: u32) -> HashSet<u32> {
        (0..len)
            .map(|offset| physical.wrapping_add(offset) & !0xf)
            .collect()
    }

    fn next(seed: &mut u64) -> u64 {
        *seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        *seed
    }

    let mut watch = StickyDecodeCodeWatch::default();
    let base = watch.table_base();
    let mut expected = HashSet::new();
    let mut seed = 0xa076_1d64_78bd_642f;
    for step in 0..2_000 {
        if next(&mut seed).is_multiple_of(11) {
            watch.clear();
            expected.clear();
            assert_eq!(watch.table_base(), base);
        } else {
            let physical = if step % 127 == 0 {
                0xffff_fff0
            } else {
                (next(&mut seed) as u32) & 0x000f_ffff
            };
            let len = (next(&mut seed) % 80 + 1) as u32;
            watch.mark_range(physical, len);
            expected.extend(chunks(physical, len));
        }

        for &chunk in &expected {
            assert!(watch.is_watched(chunk));
        }
        assert!(watch.precise_pages() <= MAX_STICKY_PRECISE_PAGES);
    }
}

#[test]
fn marks_every_chunk_touched_and_clears_without_moving_the_table() {
    let mut watch = NativeCodeWatch::default();
    watch.acquire_range(0x1f, 2);
    assert!(watch.is_watched(0x10));
    assert!(watch.is_watched(0x20));
    assert!(!watch.is_watched(0x30));

    let base = watch.table_base();
    watch.clear();
    assert_eq!(watch.table_base(), base);
    assert!(!watch.is_watched(0x10));
    assert!(!watch.is_watched(0x20));

    watch.acquire_range(0x20, 1);
    assert_eq!(watch.table_base(), base);
    assert!(watch.is_watched(0x20));

    watch.acquire_range(0x2f, 33);
    assert!(watch.range_watched(0x20, 1));
    assert!(watch.range_watched(0x40, 1));
    assert!(!watch.range_watched(0x50, 1));
}

#[test]
fn empty_and_inactive_native_watch_ranges_are_unwatched() {
    let mut watch = NativeCodeWatch::default();
    assert!(!watch.range_watched(0x12_340, 1));
    assert!(!watch.range_watched(0x12_340, 0));

    let base = watch.table_base();
    assert!(!watch.range_watched(0x12_340, 1));

    watch.acquire_range(0x12_340, 1);
    assert!(watch.range_watched(0x12_340, 1));
    watch.release_range(0x12_340, 1);
    assert_eq!(watch.inactive_pages(), 1);
    assert!(!watch.range_watched(0x12_340, 1));

    watch.clear();
    assert_eq!(watch.table_base(), base);
    assert!(!watch.has_resident_pages());
    assert!(!watch.range_watched(0x12_340, 1));
}

#[test]
fn refcounted_ranges_keep_shared_chunks_until_the_last_owner_leaves() {
    let mut watch = NativeCodeWatch::default();
    watch.acquire_range(0x100, 16);
    watch.acquire_range(0x108, 16);
    assert!(watch.is_watched(0x100));
    assert!(watch.is_watched(0x110));
    assert_eq!(watch.refcount(0x100), 2);
    assert_eq!(watch.refcount(0x110), 1);

    watch.release_range(0x100, 16);
    assert!(watch.is_watched(0x100));
    assert!(watch.is_watched(0x110));
    assert_eq!(watch.refcount(0x100), 1);

    watch.release_range(0x108, 16);
    assert!(!watch.is_watched(0x100));
    assert!(!watch.is_watched(0x110));
    assert_eq!(watch.active_pages(), 0);
}

#[test]
fn clear_removes_top_level_pointer_and_mark_republishes_it() {
    let mut watch = NativeCodeWatch::default();
    let physical = 0x12_310;
    let page = (physical >> PAGE_SHIFT) as usize;
    watch.acquire_range(physical, 1);
    let base = watch.table_base();
    // The returned base owns PAGE_COUNT entries for the lifetime of `watch`.
    let entry = unsafe { (base as *const usize).add(page) };
    assert_ne!(unsafe { *entry }, 0);

    watch.clear();
    assert_eq!(unsafe { *entry }, 0);

    watch.acquire_range(physical, 1);
    assert_eq!(watch.table_base(), base);
    assert_ne!(unsafe { *entry }, 0);
}

#[test]
fn wrapping_range_owns_and_releases_both_end_pages() {
    let mut watch = NativeCodeWatch::default();
    let base = watch.table_base();
    watch.acquire_range(0xffff_fff8, 16);
    assert!(watch.is_watched(0xffff_fff0));
    assert!(watch.is_watched(0));
    assert!(watch.range_watched(0xffff_fff8, 16));
    assert_eq!(watch.active_pages(), 2);

    watch.release_range(0xffff_fff8, 16);
    assert!(!watch.is_watched(0xffff_fff0));
    assert!(!watch.is_watched(0));
    assert_eq!(watch.active_pages(), 0);
    assert_eq!(watch.table_base(), base);
}

#[test]
fn refcount_exceeds_u16_and_releases_exactly_once_per_owner() {
    let mut watch = NativeCodeWatch::default();
    for _ in 0..=u16::MAX {
        watch.acquire_range(0x230, 1);
    }
    assert_eq!(watch.refcount(0x230), u32::from(u16::MAX) + 1);
    for _ in 0..u16::MAX {
        watch.release_range(0x230, 1);
    }
    assert!(watch.is_watched(0x230));
    assert_eq!(watch.refcount(0x230), 1);

    watch.release_range(0x230, 1);
    assert!(!watch.is_watched(0x230));
    assert_eq!(watch.active_pages(), 0);
}

#[test]
fn final_release_unpublishes_page_and_reactivation_keeps_table_base() {
    let mut watch = NativeCodeWatch::default();
    let physical = 0x45_670;
    let page = (physical >> PAGE_SHIFT) as usize;
    watch.acquire_range(physical, 1);
    let base = watch.table_base();
    let entry = unsafe { (base as *const usize).add(page) };
    let pointer = unsafe { *entry };
    assert_ne!(pointer, 0);

    watch.release_range(physical, 1);
    assert_eq!(unsafe { *entry }, 0);
    assert_eq!(watch.active_pages(), 0);
    assert_eq!(watch.inactive_pages(), 1);
    assert_eq!(watch.pages.len(), 1);

    watch.acquire_range(physical, 1);
    assert_eq!(watch.table_base(), base);
    assert_eq!(unsafe { *entry }, pointer);
    assert_eq!(watch.inactive_pages(), 0);
    watch.clear();
    assert_eq!(unsafe { *entry }, 0);
    assert_eq!(watch.active_pages(), 0);
    assert_eq!(watch.inactive_pages(), 0);
    assert!(!watch.has_resident_pages());

    watch.acquire_range(physical, 1);
    watch.release_range(physical, 1);
    assert_eq!(watch.table_base(), base);
    assert_eq!(unsafe { *entry }, 0);
    assert_eq!(watch.inactive_pages(), 1);
}

#[test]
fn final_release_before_table_initialization_retains_no_page_identity() {
    let mut watch = NativeCodeWatch::default();
    watch.acquire_range(0x12_340, 1);
    watch.release_range(0x12_340, 1);

    assert_eq!(watch.active_pages(), 0);
    assert_eq!(watch.inactive_pages(), 0);
    assert!(!watch.has_resident_pages());
    assert_eq!(watch.recycled_pages(), 1);
    assert!(watch.table.is_none());
}

#[test]
fn partial_release_keeps_the_page_published_and_clears_only_its_bit() {
    let mut watch = NativeCodeWatch::default();
    let first = 0x45_610;
    let second = 0x45_630;
    watch.acquire_range(first, 1);
    watch.acquire_range(second, 1);
    let base = watch.table_base();
    let entry = unsafe { (base as *const usize).add((first >> PAGE_SHIFT) as usize) };
    assert_ne!(unsafe { *entry }, 0);

    watch.release_range(first, 1);
    assert_ne!(unsafe { *entry }, 0);
    assert!(!watch.is_watched(first));
    assert!(watch.is_watched(second));
    assert_eq!(watch.active_pages(), 1);
    assert_eq!(watch.active_chunks(), 1);

    watch.release_range(second, 1);
    assert_eq!(unsafe { *entry }, 0);
    assert_eq!(watch.active_pages(), 0);
}

#[test]
fn clear_unpublishes_every_active_page() {
    let mut watch = NativeCodeWatch::default();
    let physical = [0x1_010, 0x23_020, 0xffff_f030];
    for &address in &physical {
        watch.acquire_range(address, 1);
    }
    let base = watch.table_base();
    let entries = physical
        .map(|address| unsafe { (base as *const usize).add((address >> PAGE_SHIFT) as usize) });
    for &entry in &entries {
        assert_ne!(unsafe { *entry }, 0);
    }

    watch.clear();
    for &entry in &entries {
        assert_eq!(unsafe { *entry }, 0);
    }
    assert_eq!(watch.active_pages(), 0);
    assert_eq!(watch.active_chunks(), 0);
    assert_eq!(watch.table_base(), base);
}

#[test]
fn clear_drains_mixed_active_and_inactive_pages() {
    let mut watch = NativeCodeWatch::default();
    let inactive = 0x12_340;
    let active = 0x45_670;
    watch.acquire_range(inactive, 1);
    watch.acquire_range(active, 1);
    let base = watch.table_base();
    let inactive_entry = unsafe { (base as *const usize).add((inactive >> PAGE_SHIFT) as usize) };
    let active_entry = unsafe { (base as *const usize).add((active >> PAGE_SHIFT) as usize) };
    watch.release_range(inactive, 1);
    assert_eq!(unsafe { *inactive_entry }, 0);
    assert_ne!(unsafe { *active_entry }, 0);
    assert_eq!(watch.active_pages(), 1);
    assert_eq!(watch.inactive_pages(), 1);

    watch.clear();
    assert_eq!(unsafe { *inactive_entry }, 0);
    assert_eq!(unsafe { *active_entry }, 0);
    assert_eq!(watch.active_pages(), 0);
    assert_eq!(watch.inactive_pages(), 0);
    assert!(!watch.has_resident_pages());
    assert_eq!(watch.recycled_pages(), 2);
    assert_eq!(watch.table_base(), base);
}

#[test]
fn published_page_base_is_the_native_mask_base() {
    assert_eq!(std::mem::offset_of!(WatchPage, mask), 0);
    let mut watch = NativeCodeWatch::default();
    let physical = 0x34_560;
    watch.acquire_range(physical, 1);
    let base = watch.table_base();
    let pointer = unsafe { *(base as *const usize).add((physical >> PAGE_SHIFT) as usize) };
    let page = watch
        .pages
        .get(&(physical >> PAGE_SHIFT))
        .expect("active page");
    assert_eq!(pointer, std::ptr::from_ref(&**page).expose_provenance());
    assert_eq!(pointer, std::ptr::from_ref(&page.mask).expose_provenance());
}

#[test]
fn published_pointer_handles_existing_acquire_and_release_without_moving() {
    let mut watch = NativeCodeWatch::default();
    let physical = 0x56_780;
    watch.acquire_range(physical, 1);
    let base = watch.table_base();
    let entry = unsafe { (base as *const usize).add((physical >> PAGE_SHIFT) as usize) };
    let pointer = unsafe { *entry };

    watch.acquire_range(physical, 1);
    assert_eq!(unsafe { *entry }, pointer);
    assert_eq!(watch.refcount(physical), 2);
    watch.release_range(physical, 1);
    assert_eq!(unsafe { *entry }, pointer);
    assert_eq!(watch.refcount(physical), 1);
    watch.release_range(physical, 1);
    assert_eq!(unsafe { *entry }, 0);
}

#[test]
fn published_pointer_survives_page_map_rehash() {
    let mut watch = NativeCodeWatch::default();
    let original = 0x10_020;
    watch.acquire_range(original, 1);
    let base = watch.table_base();
    let entry = unsafe { (base as *const usize).add((original >> PAGE_SHIFT) as usize) };
    let pointer = unsafe { *entry };

    for page in 0x20..0x220u32 {
        watch.acquire_range((page << PAGE_SHIFT) | 0x30, 1);
    }
    assert_eq!(unsafe { *entry }, pointer);
    watch.release_range(original, 1);
    assert_eq!(unsafe { *entry }, 0);
    watch.clear();
}

#[test]
fn recycled_page_changes_identity_without_retaining_bits_or_counts() {
    let mut watch = NativeCodeWatch::default();
    let old = 0x12_340;
    let new = 0x45_670;
    watch.acquire_range(old, 1);
    watch.acquire_range(old, 1);
    let base = watch.table_base();
    let old_entry = unsafe { (base as *const usize).add((old >> PAGE_SHIFT) as usize) };
    let old_pointer = unsafe { *old_entry };

    watch.clear();
    assert_eq!(unsafe { *old_entry }, 0);
    assert_eq!(watch.recycled_pages(), 1);
    watch.acquire_range(new, 1);
    let new_entry = unsafe { (base as *const usize).add((new >> PAGE_SHIFT) as usize) };
    assert_eq!(unsafe { *new_entry }, old_pointer);
    assert_eq!(watch.refcount(new), 1);
    let stale_offset_on_new_page = (new & !0xfff) | (old & 0xfff);
    assert_eq!(watch.refcount(stale_offset_on_new_page), 0);
    assert!(!watch.is_watched(stale_offset_on_new_page));
    assert!(!watch.is_watched(old));
    assert!(watch.is_watched(new));
    watch.release_range(new, 1);
    assert_eq!(unsafe { *new_entry }, 0);
}

#[test]
fn recycled_pool_is_bounded_and_clone_starts_empty() {
    let mut watch = NativeCodeWatch::default();
    for page in 0..MAX_RECYCLED_PAGES as u32 + 17 {
        watch.acquire_range((page << PAGE_SHIFT) | 0x10, 1);
    }
    watch.table_base();
    watch.clear();
    assert_eq!(watch.recycled_pages(), MAX_RECYCLED_PAGES);

    let clone = watch.clone();
    assert_eq!(clone.active_pages(), 0);
    assert_eq!(clone.active_chunks(), 0);
    assert_eq!(clone.inactive_pages(), 0);
    assert_eq!(clone.recycled_pages(), 0);
    assert!(clone.table.is_none());
}

#[test]
fn inactive_page_cache_is_bounded_without_limiting_active_pages() {
    let mut watch = NativeCodeWatch::default();
    let base = watch.table_base();
    for page in 0..=MAX_INACTIVE_PAGES as u32 {
        let physical = (page << PAGE_SHIFT) | 0x10;
        watch.acquire_range(physical, 1);
        watch.release_range(physical, 1);
        let entry = unsafe { (base as *const usize).add(page as usize) };
        assert_eq!(unsafe { *entry }, 0);
    }
    assert_eq!(watch.active_pages(), 0);
    assert_eq!(watch.inactive_pages(), MAX_INACTIVE_PAGES);
    assert_eq!(watch.pages.len(), MAX_INACTIVE_PAGES);
    assert_eq!(watch.recycled_pages(), 1);

    let active_start = 0x2_000u32;
    for page in active_start..active_start + MAX_INACTIVE_PAGES as u32 + 1 {
        watch.acquire_range((page << PAGE_SHIFT) | 0x20, 1);
    }
    assert_eq!(watch.active_pages(), MAX_INACTIVE_PAGES + 1);
    assert_eq!(watch.inactive_pages(), MAX_INACTIVE_PAGES);
    assert_eq!(
        watch.pages.len(),
        watch.active_pages() + watch.inactive_pages()
    );

    for page in active_start..active_start + MAX_INACTIVE_PAGES as u32 + 1 {
        watch.release_range((page << PAGE_SHIFT) | 0x20, 1);
    }
    assert_eq!(watch.active_pages(), 0);
    assert_eq!(watch.inactive_pages(), MAX_INACTIVE_PAGES);
    assert_eq!(watch.pages.len(), MAX_INACTIVE_PAGES);
    assert_eq!(watch.recycled_pages(), MAX_RECYCLED_PAGES);
}

#[test]
fn randomized_operations_match_a_chunk_refcount_model() {
    fn chunks(physical: u32, len: u32) -> Vec<u32> {
        let mut result = (0..len)
            .map(|offset| physical.wrapping_add(offset) & !0xf)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        result.sort_unstable();
        result
    }

    fn next(seed: &mut u64) -> u64 {
        *seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        *seed
    }

    let mut watch = NativeCodeWatch::default();
    let table_base = watch.table_base();
    let mut seed = 0x9e37_79b9_7f4a_7c15;
    let mut owners = Vec::<(u32, u32)>::new();
    let mut expected = HashMap::<u32, u32>::new();
    let mut touched_pages = HashSet::<u32>::new();

    for step in 0..2_000 {
        match next(&mut seed) % 10 {
            0..=5 => {
                let (physical, len) = if step % 19 == 0 && !owners.is_empty() {
                    owners[(next(&mut seed) as usize) % owners.len()]
                } else if step % 127 == 0 {
                    (0xffff_fff0, 48)
                } else {
                    (next(&mut seed) as u32, (next(&mut seed) % 80 + 1) as u32)
                };
                watch.acquire_range(physical, len);
                owners.push((physical, len));
                for chunk in chunks(physical, len) {
                    *expected.entry(chunk).or_default() += 1;
                    touched_pages.insert(chunk >> PAGE_SHIFT);
                }
            }
            6..=8 if !owners.is_empty() => {
                let owner = (next(&mut seed) as usize) % owners.len();
                let (physical, len) = owners.swap_remove(owner);
                watch.release_range(physical, len);
                for chunk in chunks(physical, len) {
                    let count = expected.get_mut(&chunk).expect("owned reference");
                    *count -= 1;
                    if *count == 0 {
                        expected.remove(&chunk);
                    }
                }
            }
            _ => {
                watch.clear();
                owners.clear();
                expected.clear();
            }
        }

        let expected_pages = expected
            .keys()
            .map(|chunk| chunk >> PAGE_SHIFT)
            .collect::<HashSet<_>>();
        assert_eq!(watch.active_pages(), expected_pages.len());
        assert_eq!(watch.active_chunks(), expected.len());
        assert_eq!(
            watch.pages.len(),
            watch.active_pages() + watch.inactive_pages()
        );
        assert!(watch.inactive_pages() <= MAX_INACTIVE_PAGES);
        assert_eq!(watch.table_base(), table_base);

        for (&page, page_watch) in &watch.pages {
            let mut active_chunks = 0;
            for index in 0..CHUNKS_PER_PAGE {
                let chunk = (page << PAGE_SHIFT) | ((index as u32) << CHUNK_SHIFT);
                let count = expected.get(&chunk).copied().unwrap_or(0);
                assert_eq!(page_watch.refs[index], count);
                assert_eq!(watch.is_watched(chunk), count != 0);
                let word = index / u64::BITS as usize;
                let bit = index % u64::BITS as usize;
                assert_eq!(page_watch.mask[word] & (1u64 << bit) != 0, count != 0);
                active_chunks += usize::from(count != 0);
            }
            assert_eq!(usize::from(page_watch.active_chunks), active_chunks);
        }

        let table = watch.table.as_ref().expect("initialized table");
        for &page in &touched_pages {
            let published = table[page as usize];
            if let Some(page_watch) = watch.pages.get(&page) {
                if page_watch.active_chunks == 0 {
                    assert_eq!(published, 0);
                } else {
                    assert_eq!(
                        published,
                        std::ptr::from_ref(&**page_watch).expose_provenance()
                    );
                }
            } else {
                assert_eq!(published, 0);
            }
        }
    }
}
