// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn key(linear: u32) -> BlockKey {
    BlockKey::new(linear, 0x20_000 + (linear & 0xfff), 7)
}

fn cell_portal(cell: &LinkCell) -> &BlockPortal {
    let address = cell.portal.load(Ordering::Acquire);
    assert_ne!(address, 0);
    unsafe { &*(address as *const BlockPortal) }
}

fn cell_body(cell: &LinkCell) -> usize {
    cell_portal(cell).body.load(Ordering::Acquire)
}

fn trivial_compilation(span: BlockSpan) -> Compilation {
    let mut fetch_lens = [0; MAX_BLOCK_INSTRUCTIONS];
    fetch_lens[0] = u8::try_from(span.guest_len).expect("test instruction length must fit");
    Compilation {
        span,
        decode_residency_epoch: 0,
        fetch_lens,
        raw_clocks: 1,
        weighted_fp_clocks: 0,
        byte_reads: 0,
        word_reads: 0,
        dword_reads: 0,
        byte_stores: 0,
        word_stores: 0,
        dword_stores: 0,
        segment_layout: SegmentLayout::capture(&CpuGsw::default(), 0, 0)
            .expect("default segment layout"),
        memory_cpl3: false,
        has_wide_accesses: false,
        self_loop: false,
        has_x87: false,
        x87_entry_top: 0,
        x87_exit_top: 0,
        dynamic_successor: false,
        successors: [None, None],
        link_cells: [Arc::new(LinkCell::new()), Arc::new(LinkCell::new())],
        body_offset: 0,
        code: vec![0xc3],
    }
}

fn compilation_with_fetch_lens(key: BlockKey, fetch_lens: &[u8]) -> Compilation {
    assert!(!fetch_lens.is_empty());
    let guest_len = fetch_lens.iter().map(|len| usize::from(*len)).sum();
    let span = BlockSpan::new(key, guest_len, fetch_lens.len()).expect("test block span");
    let mut compilation = trivial_compilation(span);
    compilation.fetch_lens.fill(0);
    compilation.fetch_lens[..fetch_lens.len()].copy_from_slice(fetch_lens);
    compilation
}

fn reject(cache: &mut BlockCache, key: BlockKey, guest_len: usize) {
    cache.reject(RejectedSpan::new(key, guest_len).expect("rejected test span"));
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
fn install_trivial(cache: &mut BlockCache, key: BlockKey, guest_len: usize) -> BlockId {
    assert!(matches!(cache.probe(key), BlockProbe::Interpret));
    assert!(matches!(cache.probe(key), BlockProbe::Compile));
    let span = BlockSpan::new(key, guest_len, 1).expect("test block must be page local");
    cache
        .install(&trivial_compilation(span))
        .expect("test block must install")
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
fn install_dynamic_trivial(cache: &mut BlockCache, key: BlockKey) -> BlockId {
    assert!(matches!(cache.probe(key), BlockProbe::Interpret));
    assert!(matches!(cache.probe(key), BlockProbe::Compile));
    let span = BlockSpan::new(key, 1, 1).expect("test block must be page local");
    let mut compilation = trivial_compilation(span);
    compilation.dynamic_successor = true;
    cache
        .install(&compilation)
        .expect("dynamic test block must install")
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
fn install_with_fetch_lens(cache: &mut BlockCache, key: BlockKey, fetch_lens: &[u8]) -> BlockId {
    assert!(matches!(cache.probe(key), BlockProbe::Interpret));
    assert!(matches!(cache.probe(key), BlockProbe::Compile));
    cache
        .install(&compilation_with_fetch_lens(key, fetch_lens))
        .expect("test block must install")
}

#[test]
fn span_is_bounded_and_page_local() {
    assert!(BlockSpan::new(key(0x1234), 64, MAX_BLOCK_INSTRUCTIONS).is_some());
    assert!(BlockSpan::new(key(0x1ff0), 17, 1).is_none());
    assert!(BlockSpan::new(key(0x1234), 1, MAX_BLOCK_INSTRUCTIONS + 1).is_none());
    assert!(BlockSpan::new(key(0x1234), 0, 1).is_none());
}

#[test]
fn default_metadata_is_bounded_above_the_executable_arena() {
    let cache = BlockCache::default();
    assert_eq!(cache.entry_cap, DEFAULT_ENTRY_CAP);
    let arena_slots =
        super::super::exec_mem::EXECUTABLE_ARENA_LEN / super::super::exec_mem::host_page_len();
    assert!(cache.entry_cap > arena_slots);
}

#[test]
fn clone_preserves_cache_shape_but_resets_runtime_admission() {
    let mut cache = BlockCache::new(16);
    cache.entry_cap = 7;
    cache.backend_enabled = false;
    cache.admission_heat = 11;
    cache.auto_admit = true;
    cache.defer_short_for_test = true;
    cache.fast_map_enabled_for_test = true;

    let clone = cache.clone();

    assert_eq!(clone.decode_slot_count(), 16);
    assert_eq!(clone.entry_cap, 7);
    assert!(!clone.backend_enabled);
    assert_eq!(clone.admission_heat, 11);
    assert!(!clone.auto_admit);
    assert!(clone.defer_short_for_test);
    assert!(clone.fast_map_enabled_for_test);
}

#[test]
fn dynamic_counter_mask_tracks_only_reachable_outputs() {
    let addr = DirectAddr {
        segment: SegmentIndex::Ds,
        base: None,
        index: None,
        scale: 1,
        disp: 0,
    };
    let slot = |kind| DirectInsn {
        lin: 0,
        len: 1,
        weighted_fp_clocks: 0,
        kind,
    };
    let byte_store = slot(DirectKind::Store {
        source: StoreSource::Reg(0),
        width: MemoryWidth::Byte,
        addr,
        raw_clocks: 1,
    });
    let dword_store = slot(DirectKind::Store {
        source: StoreSource::Reg(0),
        width: MemoryWidth::Dword,
        addr,
        raw_clocks: 1,
    });
    let rmw = slot(DirectKind::RmwIncDec {
        is_dec: false,
        width: MemoryWidth::Dword,
        addr,
    });
    let byte_alu = slot(DirectKind::AluMemDest {
        op: 0,
        source: StoreSource::Imm(1),
        width: MemoryWidth::Byte,
        addr,
    });
    let dword_cmp = slot(DirectKind::AluMemDest {
        op: 7,
        source: StoreSource::Reg(0),
        width: MemoryWidth::Dword,
        addr,
    });

    assert_eq!(
        dynamic_counter_mask(&[byte_store]),
        COUNTER_RAM_BYTE_WRITE | COUNTER_MODE13_BYTE_WRITE | COUNTER_MODE13_DIRTY
    );
    assert_eq!(
        dynamic_counter_mask(&[dword_store]),
        COUNTER_RAM_DWORD_WRITE | COUNTER_MODE13_DWORD_WRITE | COUNTER_MODE13_DIRTY
    );
    assert_eq!(dynamic_counter_mask(&[rmw]), COUNTER_RAM_DWORD_WRITE);
    assert_eq!(
        dynamic_counter_mask(&[byte_alu]),
        COUNTER_MODE13_BYTE_READ
            | COUNTER_RAM_BYTE_WRITE
            | COUNTER_MODE13_BYTE_WRITE
            | COUNTER_MODE13_DIRTY
    );
    assert_eq!(
        dynamic_counter_mask(&[dword_cmp]),
        COUNTER_MODE13_DWORD_READ
    );
    assert_eq!(
        dynamic_counter_mask(&[byte_store, dword_store, rmw]),
        COUNTER_RAM_BYTE_WRITE
            | COUNTER_RAM_DWORD_WRITE
            | COUNTER_MODE13_BYTE_WRITE
            | COUNTER_MODE13_DWORD_WRITE
            | COUNTER_MODE13_DIRTY
    );
    assert_eq!(
        dynamic_counter_mask(&[slot(DirectKind::MovImm { dst: 0, imm: 0 })]),
        0
    );
}

#[test]
fn first_observation_interprets_and_second_compiles() {
    let mut cache = BlockCache::default();
    let key = key(0x1234);
    assert!(matches!(cache.probe(key), BlockProbe::Interpret));
    assert!(matches!(cache.probe(key), BlockProbe::Compile));
    reject(&mut cache, key, 1);
    assert!(matches!(cache.probe(key), BlockProbe::Rejected));
}

#[test]
fn empty_cache_clear_drains_retained_code_watch_pages() {
    let mut cache = BlockCache::default();
    let key = key(0x1234);
    let table_base = cache.native_code_watch_table();
    assert!(matches!(cache.probe(key), BlockProbe::Interpret));
    assert!(matches!(cache.probe(key), BlockProbe::Compile));
    reject(&mut cache, key, 1);
    assert_eq!(cache.invalidate_physical_range(key.physical, 1), 1);
    assert!(cache.entries.is_empty());
    assert_eq!(cache.code_watch.active_pages(), 0);
    assert_eq!(cache.code_watch.inactive_pages(), 1);
    assert!(cache.code_watch.has_resident_pages());

    cache.clear();
    assert!(!cache.code_watch.has_resident_pages());
    assert_eq!(cache.code_watch.inactive_pages(), 0);
    assert_eq!(cache.native_code_watch_table(), table_base);
}

#[test]
fn capacity_pressure_clears_seen_entries() {
    let mut cache = BlockCache::with_entry_cap(2);
    let first = key(0x1000);
    assert!(matches!(cache.probe(first), BlockProbe::Interpret));
    assert!(matches!(cache.probe(key(0x1100)), BlockProbe::Interpret));
    assert!(matches!(cache.probe(key(0x1200)), BlockProbe::Interpret));
    assert!(matches!(cache.probe(first), BlockProbe::Interpret));
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn reset_ignores_stale_hot_entries_without_clearing_the_table() {
    let mut cache = BlockCache::with_entry_cap(2);
    let first = key(0x1000);
    assert!(matches!(cache.probe(first), BlockProbe::Interpret));
    assert!(matches!(cache.probe(first), BlockProbe::Compile));
    let span = BlockSpan::new(first, 1, 1).expect("one byte is page local");
    cache
        .install(&trivial_compilation(span))
        .expect("block must install");
    let hot_index = first.hot_index();
    let stale = cache.hot[hot_index].expect("install fills the hot slot");

    cache.clear();

    assert!(
        cache.hot[hot_index].is_some(),
        "reset must not scan the hot table"
    );
    assert_ne!(stale.generation, cache.hot_generation);
    assert!(matches!(cache.probe(first), BlockProbe::Interpret));
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn hash_fallback_preserves_hot_slot_collisions() {
    let mut cache = BlockCache::default();
    let first = key(0x1000);
    let second = (0x1001..)
        .map(key)
        .find(|candidate| candidate.hot_index() == first.hot_index())
        .expect("the finite hot table must collide");

    for candidate in [first, second] {
        assert!(matches!(cache.probe(candidate), BlockProbe::Interpret));
        assert!(matches!(cache.probe(candidate), BlockProbe::Compile));
        let span = BlockSpan::new(candidate, 1, 1).expect("one byte is page local");
        cache
            .install(&trivial_compilation(span))
            .expect("block must install");
    }

    assert!(matches!(cache.probe(first), BlockProbe::Ready(_)));
    assert!(matches!(cache.probe(second), BlockProbe::Ready(_)));
    assert_eq!(cache.len(), 2);
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn both_successor_cells_resolve_unlink_recompile_and_reset() {
    let mut cache = BlockCache::default();
    let source = key(0x1000);
    let fallthrough = key(0x1100);
    let taken = key(0x1200);
    assert!(matches!(cache.probe(source), BlockProbe::Interpret));
    let mut source_compilation =
        trivial_compilation(BlockSpan::new(source, 1, 1).expect("source span"));
    source_compilation.successors = [
        Some(LinkTarget {
            linear: fallthrough.linear,
            mode_key: source.mode_key,
        }),
        Some(LinkTarget {
            linear: taken.linear,
            mode_key: source.mode_key,
        }),
    ];
    let source_id = cache.install(&source_compilation).expect("source install");
    assert_eq!(cache.outbound[source_id.index()], [None, None]);

    install_trivial(&mut cache, taken, 1);
    assert!(cache.outbound[source_id.index()][0].is_none());
    assert!(cache.outbound[source_id.index()][1].is_some());
    install_trivial(&mut cache, fallthrough, 1);
    assert!(
        cache.outbound[source_id.index()]
            .iter()
            .all(Option::is_some)
    );

    let cells = cache.link_cells[source_id.index()].clone();
    assert_eq!(cache.invalidate_physical_range(taken.physical, 1), 1);
    assert!(cells[0].linked());
    assert!(!cells[1].linked());
    assert!(matches!(cache.probe(taken), BlockProbe::Interpret));
    let replacement = trivial_compilation(BlockSpan::new(taken, 1, 1).unwrap());
    cache.install(&replacement).expect("replacement install");
    assert!(cells[1].linked());

    cache.clear();
    assert!(!cells[0].linked());
    assert!(!cells[1].linked());
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn dynamic_ret_pic_keeps_two_targets_and_unlinks_replaced_or_retired_blocks() {
    let mut cache = BlockCache::default();
    let source = key(0x1000);
    let first = key(0x1100);
    let second = key(0x1200);
    let third = key(0x1300);
    let source_id = install_dynamic_trivial(&mut cache, source);
    install_trivial(&mut cache, first, 1);
    install_trivial(&mut cache, second, 1);
    install_trivial(&mut cache, third, 1);
    let cells = cache.link_cells[source_id.index()].clone();
    let site_cell = cells[0].address();

    assert!(cache.bind_dynamic_successor(site_cell, first.linear, first.linear, first.mode_key));
    assert!(cache.bind_dynamic_successor(site_cell, second.linear, second.linear, second.mode_key));
    assert!(cells[0].linked());
    assert!(cells[1].linked());
    assert_eq!(cells[0].target_eip.load(Ordering::Acquire), first.linear);
    assert_eq!(cells[1].target_eip.load(Ordering::Acquire), second.linear);
    let cell_addresses = [cells[0].address(), cells[1].address()];
    let old_bodies = [cell_body(&cells[0]), cell_body(&cells[1])];
    assert!(cache.compact_arena());
    assert_eq!([cells[0].address(), cells[1].address()], cell_addresses);
    assert_ne!([cell_body(&cells[0]), cell_body(&cells[1]),], old_bodies);
    assert_eq!(cells[0].target_eip.load(Ordering::Acquire), first.linear);
    assert_eq!(cells[1].target_eip.load(Ordering::Acquire), second.linear);

    assert!(cache.bind_dynamic_successor(site_cell, third.linear, third.linear, third.mode_key));
    assert_eq!(cells[0].target_eip.load(Ordering::Acquire), third.linear);
    assert_eq!(cells[1].target_eip.load(Ordering::Acquire), second.linear);
    assert!(cells[0].linked());
    assert!(cells[1].linked());

    assert_eq!(cache.invalidate_physical_range(first.physical, 1), 1);
    assert!(cells[0].linked());
    assert!(cells[1].linked());
    assert_eq!(cache.invalidate_physical_range(second.physical, 1), 1);
    assert!(cells[0].linked());
    assert!(!cells[1].linked());
    assert_eq!(cache.invalidate_physical_range(third.physical, 1), 1);
    assert!(!cells[0].linked());

    assert_eq!(cache.invalidate_physical_range(source.physical, 1), 1);
    assert!(!cache.bind_dynamic_successor(site_cell, first.linear, first.linear, first.mode_key));
    let stats = cache.take_stats();
    assert_eq!(stats.links, 3);
    assert_eq!(stats.unlinks, 3);
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn dynamic_ret_does_not_rebind_when_a_target_portal_slot_is_reused() {
    let mut cache = BlockCache::new(8);
    let source = key(0x2000);
    let old_target = key(0x2100);
    let replacement = key(0x2201);
    let source_id = install_dynamic_trivial(&mut cache, source);
    let old_target_id = install_trivial(&mut cache, old_target, 1);
    let cell = cache.link_cells[source_id.index()][0].clone();
    let site_cell = cell.address();
    assert!(cache.bind_dynamic_successor(
        site_cell,
        old_target.linear,
        old_target.linear,
        old_target.mode_key
    ));
    let old_portal = cache.block_portals[old_target_id.index()].address();
    assert!(cell.linked());

    assert_eq!(cache.invalidate_physical_range(old_target.physical, 1), 1);
    assert!(!cell.linked());
    assert_eq!(cell.portal.load(Ordering::Acquire), zero_portal().address());

    let replacement_id = install_trivial(&mut cache, replacement, 1);
    assert_eq!(replacement_id.index(), old_target_id.index());
    assert_eq!(
        cache.block_portals[replacement_id.index()].address(),
        old_portal
    );
    assert!(cache.is_link_visible(replacement_id));
    assert!(!cell.linked());
    assert_eq!(cell.portal.load(Ordering::Acquire), zero_portal().address());

    assert!(cache.bind_dynamic_successor(
        site_cell,
        replacement.linear,
        replacement.linear,
        replacement.mode_key
    ));
    assert!(cell.linked());
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn retained_link_cell_is_unlinked_when_its_cache_drops() {
    let cell = {
        let mut cache = BlockCache::default();
        let source = key(0x2300);
        let target = key(0x2400);
        let source_id = install_dynamic_trivial(&mut cache, source);
        install_trivial(&mut cache, target, 1);
        let cell = cache.link_cells[source_id.index()][0].clone();
        assert!(cache.bind_dynamic_successor(
            cell.address(),
            target.linear,
            target.linear,
            target.mode_key
        ));
        assert!(cell.linked());
        cell
    };

    assert_eq!(cell.portal.load(Ordering::Acquire), zero_portal().address());
    assert!(!cell.linked());
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn dynamic_ret_pic_requires_matching_x87_chain_top_and_kind() {
    let mut cache = BlockCache::default();
    let source = key(0x1000);
    let wrong_top = key(0x1100);
    let integer = key(0x1200);
    let matching_top = key(0x1300);

    assert!(matches!(cache.probe(source), BlockProbe::Interpret));
    assert!(matches!(cache.probe(source), BlockProbe::Compile));
    let mut source_compilation =
        trivial_compilation(BlockSpan::new(source, 1, 1).expect("source span"));
    source_compilation.has_x87 = true;
    source_compilation.x87_entry_top = 1;
    source_compilation.x87_exit_top = 3;
    source_compilation.dynamic_successor = true;
    let source_id = cache
        .install(&source_compilation)
        .expect("x87 source install");
    let site_cell = cache.link_cells[source_id.index()][0].address();

    assert!(matches!(cache.probe(wrong_top), BlockProbe::Interpret));
    assert!(matches!(cache.probe(wrong_top), BlockProbe::Compile));
    let mut wrong_top_compilation =
        trivial_compilation(BlockSpan::new(wrong_top, 1, 1).expect("wrong-top span"));
    wrong_top_compilation.has_x87 = true;
    wrong_top_compilation.x87_entry_top = 2;
    wrong_top_compilation.x87_exit_top = 2;
    cache
        .install(&wrong_top_compilation)
        .expect("wrong-top install");
    install_trivial(&mut cache, integer, 1);

    assert!(!cache.bind_dynamic_successor(
        site_cell,
        wrong_top.linear,
        wrong_top.linear,
        wrong_top.mode_key
    ));
    assert!(!cache.bind_dynamic_successor(
        site_cell,
        integer.linear,
        integer.linear,
        integer.mode_key
    ));

    assert!(matches!(cache.probe(matching_top), BlockProbe::Interpret));
    assert!(matches!(cache.probe(matching_top), BlockProbe::Compile));
    let mut matching_compilation =
        trivial_compilation(BlockSpan::new(matching_top, 1, 1).expect("matching span"));
    matching_compilation.has_x87 = true;
    matching_compilation.x87_entry_top = 3;
    matching_compilation.x87_exit_top = 3;
    cache
        .install(&matching_compilation)
        .expect("matching install");
    assert!(cache.bind_dynamic_successor(
        site_cell,
        matching_top.linear,
        matching_top.linear,
        matching_top.mode_key
    ));
    assert!(cache.link_cells[source_id.index()][0].linked());
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn dynamic_ret_pic_stays_unlinked_until_both_translation_epochs_are_current() {
    let mut cache = BlockCache::default();
    let source = key(0x1000);
    let target = key(0x1100);
    let source_id = install_dynamic_trivial(&mut cache, source);
    install_trivial(&mut cache, target, 1);
    let cell = cache.link_cells[source_id.index()][0].clone();
    let site_cell = cell.address();
    assert!(cache.bind_dynamic_successor(site_cell, target.linear, target.linear, target.mode_key));
    assert!(cell.linked());

    cache.invalidate_translation();
    assert!(!cell.linked());
    assert_eq!(cell.target_eip.load(Ordering::Acquire), target.linear);
    assert!(!cache.bind_dynamic_successor(
        site_cell,
        target.linear,
        target.linear,
        target.mode_key
    ));

    cache
        .refresh_decode_residency(source, 1)
        .expect("source revalidation");
    assert!(!cache.bind_dynamic_successor(
        site_cell,
        target.linear,
        target.linear,
        target.mode_key
    ));
    cache
        .refresh_decode_residency(target, 1)
        .expect("target revalidation");
    assert!(cache.bind_dynamic_successor(site_cell, target.linear, target.linear, target.mode_key));
    assert!(cell.linked());
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn translation_epoch_preserves_code_and_relinks_only_revalidated_blocks() {
    let mut cache = BlockCache::default();
    let source = key(0x1000);
    let target = key(0x1100);
    let rejected = key(0x1200);
    assert!(matches!(cache.probe(source), BlockProbe::Interpret));
    let mut source_compilation =
        trivial_compilation(BlockSpan::new(source, 1, 1).expect("source span"));
    source_compilation.successors[0] = Some(LinkTarget {
        linear: target.linear,
        mode_key: target.mode_key,
    });
    let source_id = cache.install(&source_compilation).expect("source install");
    install_trivial(&mut cache, target, 1);
    assert!(matches!(cache.probe(rejected), BlockProbe::Interpret));
    assert!(matches!(cache.probe(rejected), BlockProbe::Compile));
    reject(&mut cache, rejected, 1);

    let entry = cache.block(source_id).expect("source block").entry_ptr();
    let slots = cache.arena.as_ref().expect("arena").used_slots();
    let cells = cache.link_cells[source_id.index()].clone();
    assert!(cells[0].linked());

    cache.invalidate_translation();

    assert_eq!(cache.len(), 2);
    assert_eq!(cache.tracked_len(), 3);
    assert_eq!(cache.arena.as_ref().expect("arena").used_slots(), slots);
    assert_eq!(
        cache.block(source_id).expect("source block").entry_ptr(),
        entry
    );
    assert!(cache.range_hits_compiled_code(source.physical, 1));
    assert!(!cells[0].linked());
    assert!(cache.linear_blocks.is_empty());
    assert!(matches!(cache.probe(rejected), BlockProbe::Rejected));
    assert!(matches!(cache.probe(source), BlockProbe::Ready(id) if id == source_id));

    cache
        .refresh_decode_residency(source, 1)
        .expect("source revalidation");
    assert!(
        !cells[0].linked(),
        "an unvalidated target must stay unlinked"
    );
    let remapped_target = BlockKey::new(target.linear, target.physical + 0x1000, target.mode_key);
    assert!(matches!(
        cache.probe(remapped_target),
        BlockProbe::Interpret
    ));
    assert!(
        !cells[0].linked(),
        "a different physical key cannot satisfy the link"
    );

    assert!(matches!(cache.probe(target), BlockProbe::Ready(_)));
    cache
        .refresh_decode_residency(target, 1)
        .expect("same mapping revalidation");
    assert!(cells[0].linked());
    assert_eq!(cache.arena.as_ref().expect("arena").used_slots(), slots);
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn decode_slot_suspension_requires_revalidation_even_when_the_token_matches() {
    let mut cache = BlockCache::default();
    let block = key(0x4100);
    let id = install_trivial(&mut cache, block, 1);
    assert!(cache.is_link_visible(id));

    cache.suspend_decode_slot(block.linear as usize & cache.decode_slot_mask);
    assert!(!cache.is_link_visible(id));
    assert!(matches!(cache.probe(block), BlockProbe::Ready(hit) if hit == id));

    cache
        .refresh_decode_residency(block, 0)
        .expect("decode revalidation");
    assert!(cache.is_link_visible(id));
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn decode_slot_suspension_is_exact_and_keeps_logical_links_intact() {
    let mut cache = BlockCache::new(8);
    let outer = key(0x4000);
    let overlap = key(0x4002);
    let neighbor = key(0x4001);
    let outer_id = install_with_fetch_lens(&mut cache, outer, &[2, 2, 1]);
    let overlap_id = install_with_fetch_lens(&mut cache, overlap, &[2, 1]);
    let neighbor_id = install_with_fetch_lens(&mut cache, neighbor, &[1]);
    let outbound = cache.outbound.clone();
    let inbound = cache.inbound.clone();
    let waiting = cache.waiting.clone();
    let linear_blocks = cache.linear_blocks.clone();

    assert_eq!(cache.suspend_decode_slot(4), 2);
    assert!(!cache.is_link_visible(outer_id));
    assert!(!cache.is_link_visible(overlap_id));
    assert!(cache.is_link_visible(neighbor_id));
    assert_eq!(cache.outbound, outbound);
    assert_eq!(cache.inbound, inbound);
    assert_eq!(cache.waiting, waiting);
    assert_eq!(cache.linear_blocks, linear_blocks);

    assert_eq!(cache.suspend_decode_slot(3), 0);
    assert!(cache.is_link_visible(neighbor_id));
    assert_eq!(cache.suspend_decode_slot(4), 0);
    let stats = cache.take_stats();
    assert_eq!(stats.decode_dependencies_scanned, 4);
    assert_eq!(stats.portals_hidden, 2);
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn repeated_source_revalidation_does_not_rebuild_its_link_graph() {
    let mut cache = BlockCache::new(16);
    let source = key(0x4101);
    let hidden_target = key(0x4202);
    let unresolved_target = key(0x4303);
    let hidden_id = install_trivial(&mut cache, hidden_target, 1);
    assert_eq!(cache.suspend_decode_slot(2), 1);
    assert!(!cache.is_link_visible(hidden_id));

    assert!(matches!(cache.probe(source), BlockProbe::Interpret));
    assert!(matches!(cache.probe(source), BlockProbe::Compile));
    let mut source_compilation =
        trivial_compilation(BlockSpan::new(source, 1, 1).expect("source span"));
    source_compilation.successors = [
        Some(LinkTarget {
            linear: unresolved_target.linear,
            mode_key: unresolved_target.mode_key,
        }),
        Some(LinkTarget {
            linear: hidden_target.linear,
            mode_key: hidden_target.mode_key,
        }),
    ];
    let source_id = cache.install(&source_compilation).expect("source install");
    assert_eq!(cache.waiting.values().map(Vec::len).sum::<usize>(), 1);
    assert_eq!(cache.inbound.values().map(Vec::len).sum::<usize>(), 1);
    assert_eq!(cache.outbound[source_id.index()][0], None);
    assert_eq!(cache.outbound[source_id.index()][1], Some(hidden_id));
    assert!(!cache.link_cells[source_id.index()][1].linked());

    let outbound = cache.outbound.clone();
    let inbound = cache.inbound.clone();
    let waiting = cache.waiting.clone();
    let linear_blocks = cache.linear_blocks.clone();
    let graph_epochs = cache.block_link_epochs.clone();
    let cells = cache.link_cells[source_id.index()].clone();
    let portal_handles = cells
        .each_ref()
        .map(|cell| cell.portal.load(Ordering::Acquire));

    for residency_epoch in 1..=16 {
        assert_eq!(cache.suspend_decode_slot(1), 1);
        assert!(!cache.is_link_visible(source_id));
        assert_eq!(cache.block_link_epochs, graph_epochs);
        cache
            .refresh_decode_residency(source, residency_epoch)
            .expect("source revalidation");
        assert!(cache.is_link_visible(source_id));
        assert!(!cache.is_link_visible(hidden_id));
        assert_eq!(cache.outbound, outbound);
        assert_eq!(cache.inbound, inbound);
        assert_eq!(cache.waiting, waiting);
        assert_eq!(cache.linear_blocks, linear_blocks);
        assert_eq!(cache.block_link_epochs, graph_epochs);
        assert_eq!(
            cells
                .each_ref()
                .map(|cell| cell.portal.load(Ordering::Acquire)),
            portal_handles
        );
    }
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn retired_slot_dependencies_do_not_hide_a_reused_metadata_portal() {
    let mut cache = BlockCache::new(8);
    let old = key(0x5000);
    let old_id = install_with_fetch_lens(&mut cache, old, &[2, 2]);
    let portal_address = cache.block_portals[old_id.index()].address();
    let dependency_capacity = cache.decode_dependencies[0].capacity();
    let block_slot_capacity = cache.block_decode_slots[old_id.index()].capacity();

    assert_eq!(cache.invalidate_physical_range(old.physical, 4), 1);
    assert!(cache.decode_dependencies[0].is_empty());
    assert!(cache.block_decode_slots[old_id.index()].is_empty());
    assert!(cache.decode_dependencies[0].capacity() >= dependency_capacity);
    assert!(cache.block_decode_slots[old_id.index()].capacity() >= block_slot_capacity);

    let replacement = key(0x5001);
    let replacement_id = install_with_fetch_lens(&mut cache, replacement, &[1]);
    assert_eq!(replacement_id.index(), old_id.index());
    assert_eq!(
        cache.block_portals[replacement_id.index()].address(),
        portal_address
    );
    assert_eq!(cache.suspend_decode_slot(0), 0);
    assert!(cache.is_link_visible(replacement_id));
    assert_eq!(cache.suspend_decode_slot(1), 1);
    assert!(!cache.is_link_visible(replacement_id));

    let replacement_dependency_capacity = cache.decode_dependencies[1].capacity();
    cache.clear();
    assert!(cache.decode_dependencies[1].is_empty());
    assert!(cache.decode_dependencies[1].capacity() >= replacement_dependency_capacity);
    assert!(cache.block_decode_slots[replacement_id.index()].is_empty());
}

#[test]
fn full_arena_compacts_only_when_it_can_reclaim_a_slot() {
    assert!(!BlockCache::arena_compaction_can_reclaim(0, 8));
    assert!(BlockCache::arena_compaction_can_reclaim(7, 8));
    assert!(!BlockCache::arena_compaction_can_reclaim(8, 8));
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn invalidated_metadata_slot_reuse_rejects_its_stale_generation() {
    let mut cache = BlockCache::default();
    let source = key(0x1400);
    let missing = key(0x1500);
    assert!(matches!(cache.probe(source), BlockProbe::Interpret));
    assert!(matches!(cache.probe(source), BlockProbe::Compile));
    let mut compilation = trivial_compilation(BlockSpan::new(source, 1, 1).expect("source span"));
    compilation.successors[0] = Some(LinkTarget {
        linear: missing.linear,
        mode_key: missing.mode_key,
    });
    let stale_id = cache.install(&compilation).expect("source install");
    let stale_block = cache.block(stale_id).expect("source block");
    assert!(
        cache
            .waiting
            .values()
            .flatten()
            .any(|source| source.block == stale_id)
    );

    assert_eq!(cache.invalidate_physical_range(source.physical, 1), 1);
    assert!(cache.block(stale_id).is_none());
    assert_eq!(cache.blocks[stale_id.index()].entry, 0);
    assert_eq!(cache.blocks[stale_id.index()].body_entry, 0);
    assert!(
        !cache
            .waiting
            .values()
            .flatten()
            .any(|source| source.block == stale_id)
    );

    let replacement_id = install_trivial(&mut cache, key(0x1600), 1);
    assert_eq!(replacement_id.index(), stale_id.index());
    assert_ne!(replacement_id, stale_id);
    assert_eq!(cache.blocks.len(), 1);
    assert!(cache.block(stale_block.id()).is_none());
    assert_eq!(
        cache.block(replacement_id).expect("replacement").id(),
        replacement_id
    );
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn linked_blocks_relocate_without_replacing_link_cells() {
    let mut cache = BlockCache::default();
    let source = key(0x1700);
    let target = key(0x1800);
    let dead = key(0x1900);
    assert!(matches!(cache.probe(source), BlockProbe::Interpret));
    assert!(matches!(cache.probe(source), BlockProbe::Compile));
    let mut source_compilation =
        trivial_compilation(BlockSpan::new(source, 1, 1).expect("source span"));
    source_compilation.successors[0] = Some(LinkTarget {
        linear: target.linear,
        mode_key: target.mode_key,
    });
    let source_cell_address = source_compilation.link_cells[0].address();
    source_compilation.code = vec![0x48, 0xb8];
    source_compilation
        .code
        .extend_from_slice(&(source_cell_address as u64).to_le_bytes());
    source_compilation
        .code
        .extend_from_slice(&[0x48, 0x8b, 0x00, 0xff, 0x20]);
    let source_id = cache.install(&source_compilation).expect("source install");
    let target_id = install_trivial(&mut cache, target, 1);
    let dead_id = install_trivial(&mut cache, dead, 1);
    let source_cell = cache.link_cells[source_id.index()][0].clone();
    let old_source_entry = cache.block(source_id).expect("source").entry;
    let old_target_body = cache.block(target_id).expect("target").body_ptr();
    assert_eq!(cell_body(&source_cell), old_target_body);
    let old_entry: extern "C" fn() =
        unsafe { std::mem::transmute(cache.block(source_id).expect("source").entry_ptr()) };
    old_entry();
    assert_eq!(cache.invalidate_physical_range(dead.physical, 1), 1);
    let link_epochs = cache.block_link_epochs.clone();

    assert!(cache.compact_arena());

    let relocated_source = cache.block(source_id).expect("relocated source");
    let relocated_target = cache.block(target_id).expect("relocated target");
    assert_ne!(relocated_source.entry, old_source_entry);
    assert_ne!(relocated_target.body_ptr(), old_target_body);
    assert_eq!(source_cell.address(), source_cell_address);
    assert_eq!(cell_body(&source_cell), relocated_target.body_ptr());
    assert_eq!(cache.block_link_epochs, link_epochs);
    assert!(cache.range_hits_compiled_code(source.physical, 1));
    assert!(cache.range_hits_compiled_code(target.physical, 1));
    assert_eq!(cache.arena.as_ref().expect("arena").used_slots(), 2);
    let entry: extern "C" fn() = unsafe { std::mem::transmute(relocated_source.entry_ptr()) };
    entry();

    let reused_id = install_trivial(&mut cache, key(0x1a00), 1);
    assert_eq!(reused_id.index(), dead_id.index());
    assert_eq!(cache.arena.as_ref().expect("arena").used_slots(), 3);
    let stats = cache.take_stats();
    assert_eq!(stats.arena_compactions, 1);
    assert_eq!(stats.arena_compaction_live_blocks, 2);
    assert_eq!(stats.arena_compaction_bytes, 16);
    assert_eq!(stats.arena_compaction_failures, 0);
    assert_eq!(stats.cache_resets, 0);
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn unresolved_waiting_edge_survives_arena_compaction() {
    let mut cache = BlockCache::default();
    let source = key(0x1b00);
    let target = key(0x1c00);
    assert!(matches!(cache.probe(source), BlockProbe::Interpret));
    assert!(matches!(cache.probe(source), BlockProbe::Compile));
    let mut source_compilation =
        trivial_compilation(BlockSpan::new(source, 1, 1).expect("source span"));
    let target_key = LinkTarget {
        linear: target.linear,
        mode_key: target.mode_key,
    };
    source_compilation.successors[0] = Some(target_key);
    let source_id = cache.install(&source_compilation).expect("source install");
    let waiting = cache
        .waiting
        .get(&target_key)
        .cloned()
        .expect("waiting edge");

    assert!(cache.compact_arena());
    assert_eq!(cache.waiting.get(&target_key), Some(&waiting));
    let target_id = install_trivial(&mut cache, target, 1);
    assert_eq!(cache.outbound[source_id.index()][0], Some(target_id));
    assert_eq!(
        cell_body(&cache.link_cells[source_id.index()][0]),
        cache.block(target_id).expect("target").body_ptr()
    );
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn translation_invalid_blocks_stay_invisible_through_compaction() {
    let mut cache = BlockCache::default();
    let source = key(0x1d00);
    let target = key(0x1e00);
    assert!(matches!(cache.probe(source), BlockProbe::Interpret));
    assert!(matches!(cache.probe(source), BlockProbe::Compile));
    let mut source_compilation =
        trivial_compilation(BlockSpan::new(source, 1, 1).expect("source span"));
    source_compilation.successors[0] = Some(LinkTarget {
        linear: target.linear,
        mode_key: target.mode_key,
    });
    let source_id = cache.install(&source_compilation).expect("source install");
    let target_id = install_trivial(&mut cache, target, 1);
    let source_cell = cache.link_cells[source_id.index()][0].clone();
    assert!(source_cell.linked());

    cache.invalidate_translation();
    let link_epochs = cache.block_link_epochs.clone();
    assert!(cache.linear_blocks.is_empty());
    assert!(!source_cell.linked());
    assert!(cache.compact_arena());

    assert_eq!(cache.block_link_epochs, link_epochs);
    assert!(cache.linear_blocks.is_empty());
    assert!(cache.waiting.is_empty());
    assert!(!source_cell.linked());
    cache
        .refresh_decode_residency(source, 1)
        .expect("source revalidation");
    assert!(!source_cell.linked());
    cache
        .refresh_decode_residency(target, 1)
        .expect("target revalidation");
    assert!(source_cell.linked());
    assert_eq!(cache.outbound[source_id.index()][0], Some(target_id));
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn compaction_republishes_only_portals_that_were_visible() {
    let mut cache = BlockCache::new(8);
    let hidden = key(0x3000);
    let visible = key(0x3001);
    let hidden_id = install_trivial(&mut cache, hidden, 1);
    let visible_id = install_trivial(&mut cache, visible, 1);
    let old_visible_body = cache.block_portals[visible_id.index()]
        .body
        .load(Ordering::Acquire);

    assert_eq!(cache.suspend_decode_slot(0), 1);
    assert!(!cache.is_link_visible(hidden_id));
    assert!(cache.is_link_visible(visible_id));
    assert!(cache.compact_arena());

    assert_eq!(
        cache.block_portals[hidden_id.index()]
            .body
            .load(Ordering::Acquire),
        0
    );
    assert!(!cache.is_link_visible(hidden_id));
    assert!(cache.is_link_visible(visible_id));
    assert_ne!(
        cache.block_portals[visible_id.index()]
            .body
            .load(Ordering::Acquire),
        old_visible_body
    );
    cache
        .refresh_decode_residency(hidden, 1)
        .expect("hidden block revalidation");
    assert!(cache.is_link_visible(hidden_id));
    assert_eq!(
        cache.block_portals[hidden_id.index()]
            .body
            .load(Ordering::Acquire),
        cache
            .block(hidden_id)
            .expect("relocated hidden block")
            .body_ptr()
    );
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn translation_flush_preserves_blocks_but_coarse_map_flushes_drop_them() {
    let mut cpu = CpuGsw::default();
    let block_key = key(0x1000);
    install_trivial(&mut cpu.jit_direct, block_key, 1);
    let entry = cpu.jit_direct.blocks[0].entry_ptr();

    cpu.flush_tlb_and_code_caches();

    assert_eq!(cpu.jit_direct.len(), 1);
    assert_eq!(cpu.jit_direct.blocks[0].entry_ptr(), entry);
    assert!(matches!(
        cpu.jit_direct.probe(block_key),
        BlockProbe::Ready(_)
    ));

    cpu.note_a20_changed();

    assert_eq!(cpu.jit_direct.len(), 0);
    assert!(cpu.jit_direct.arena.is_none());
    assert!(matches!(
        cpu.jit_direct.probe(block_key),
        BlockProbe::Interpret
    ));

    assert!(matches!(
        cpu.jit_direct.probe(block_key),
        BlockProbe::Compile
    ));
    let span = BlockSpan::new(block_key, 1, 1).expect("replacement span");
    cpu.jit_direct
        .install(&trivial_compilation(span))
        .expect("replacement install");
    cpu.note_direct_map_changed();
    assert_eq!(cpu.jit_direct.len(), 0);
    assert!(cpu.jit_direct.arena.is_none());
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn physical_invalidation_removes_overlap_and_preserves_adjacent_blocks() {
    let mut cache = BlockCache::default();
    let overlap = BlockKey::new(0x1000, 0x20_020, 7);
    let adjacent = BlockKey::new(0x1100, 0x20_040, 7);
    install_trivial(&mut cache, overlap, 16);
    install_trivial(&mut cache, adjacent, 16);

    assert_eq!(cache.invalidate_physical_range(0x20_02f, 1), 1);
    assert_eq!(cache.len(), 1);
    assert_eq!(cache.blocks.len(), 2, "stable block IDs must not compact");
    assert_eq!(cache.block_active, [false, true]);
    assert!(cache.arena.is_some(), "sealed pages stay allocated");
    assert!(matches!(cache.probe(overlap), BlockProbe::Interpret));
    assert!(matches!(cache.probe(adjacent), BlockProbe::Ready(_)));

    cache.clear();
    assert_eq!(cache.len(), 0);
    assert!(cache.blocks.is_empty());
    assert!(cache.block_active.is_empty());
    assert!(cache.physical_keys.is_empty());
    assert!(cache.arena.is_none());
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn physical_invalidation_refcounts_shared_watch_chunks() {
    let mut cache = BlockCache::default();
    let first = BlockKey::new(0x1000, 0x20_020, 7);
    let second = BlockKey::new(0x2000, 0x20_028, 7);
    install_trivial(&mut cache, first, 8);
    install_trivial(&mut cache, second, 8);
    assert!(cache.range_hits_compiled_code(0x20_020, 16));

    assert_eq!(cache.invalidate_physical_range(first.physical, 1), 1);
    assert!(
        cache.range_hits_compiled_code(first.physical, 1),
        "the neighboring block still owns the shared 16-byte watch"
    );

    assert_eq!(cache.invalidate_physical_range(second.physical, 1), 1);
    assert!(!cache.range_hits_compiled_code(first.physical, 16));
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn invalidated_code_chunk_can_be_reused_and_recompiled() {
    let mut cache = BlockCache::default();
    let old = BlockKey::new(0x1000, 0x21_020, 7);
    install_trivial(&mut cache, old, 8);
    assert!(cache.range_hits_compiled_code(old.physical, 1));

    assert_eq!(cache.invalidate_physical_range(old.physical, 1), 1);
    assert!(!cache.range_hits_compiled_code(old.physical, 1));

    install_trivial(&mut cache, old, 8);
    assert!(cache.range_hits_compiled_code(old.physical, 1));
    assert_eq!(cache.invalidate_physical_range(old.physical, 1), 1);
    assert!(!cache.range_hits_compiled_code(old.physical, 1));
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn physical_invalidation_removes_every_linear_alias_without_stale_ready_hits() {
    let mut cache = BlockCache::default();
    let first = BlockKey::new(0x1000, 0x30_080, 7);
    let alias = BlockKey::new(0x5000, 0x30_080, 9);
    install_trivial(&mut cache, first, 16);
    install_trivial(&mut cache, alias, 16);

    assert_eq!(cache.invalidate_physical_range(0x30_084, 2), 2);
    assert_eq!(cache.len(), 0);
    assert!(matches!(cache.probe(first), BlockProbe::Interpret));
    assert!(matches!(cache.probe(alias), BlockProbe::Interpret));
}

#[test]
fn physical_invalidation_forgets_seen_and_rejected_entries_only_on_overlap() {
    let mut cache = BlockCache::default();
    let seen = BlockKey::new(0x1000, 0x40_010, 7);
    let rejected = BlockKey::new(0x2000, 0x40_010, 9);
    let adjacent = BlockKey::new(0x3000, 0x40_020, 7);
    assert!(matches!(cache.probe(seen), BlockProbe::Interpret));
    assert!(matches!(cache.probe(rejected), BlockProbe::Interpret));
    assert!(matches!(cache.probe(rejected), BlockProbe::Compile));
    reject(&mut cache, rejected, 1);
    assert!(matches!(cache.probe(adjacent), BlockProbe::Interpret));

    assert_eq!(cache.invalidate_physical_range(0x40_010, 1), 2);
    assert!(matches!(cache.probe(seen), BlockProbe::Interpret));
    assert!(matches!(cache.probe(rejected), BlockProbe::Interpret));
    assert!(matches!(cache.probe(adjacent), BlockProbe::Compile));
}

#[test]
fn physical_invalidation_checks_both_pages_of_a_cross_page_write() {
    let mut cache = BlockCache::default();
    let low = BlockKey::new(0x1000, 0x4fff, 7);
    let high = BlockKey::new(0x2000, 0x5000, 7);
    assert!(matches!(cache.probe(low), BlockProbe::Interpret));
    assert!(matches!(cache.probe(high), BlockProbe::Interpret));

    assert_eq!(cache.invalidate_physical_range(0x4fff, 2), 2);
    assert!(matches!(cache.probe(low), BlockProbe::Interpret));
    assert!(matches!(cache.probe(high), BlockProbe::Interpret));
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn cpu_code_write_uses_selective_direct_invalidation() {
    let mut cpu = CpuGsw::default();
    let overlap = BlockKey::new(0x1000, 0x60_010, 7);
    let adjacent = BlockKey::new(0x2000, 0x60_030, 7);
    install_trivial(&mut cpu.jit_direct, overlap, 16);
    install_trivial(&mut cpu.jit_direct, adjacent, 16);
    cpu.decode_cache.mark_code_range(overlap.physical, 1);
    cpu.jit_direct.mark_code_range(overlap.physical, 1);
    cpu.decode_cache.invalidate_and_clear_code_marks();
    assert!(!cpu.decode_cache.range_hits_code(overlap.physical, 1));

    cpu.note_code_write(overlap.physical, 1);

    assert_eq!(cpu.jit_direct.len(), 1);
    assert!(matches!(
        cpu.jit_direct.probe(overlap),
        BlockProbe::Interpret
    ));
    assert!(matches!(
        cpu.jit_direct.probe(adjacent),
        BlockProbe::Ready(_)
    ));
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn ranged_device_write_preserves_unrelated_blocks_and_unlinks_overlap() {
    let mut cpu = CpuGsw::default();
    let source = BlockKey::new(0x1000, 0x60_000, 7);
    let overlap = BlockKey::new(0x2000, 0x61_000, 7);
    let unrelated = BlockKey::new(0x3000, 0x62_000, 7);

    let mut source_compilation =
        trivial_compilation(BlockSpan::new(source, 1, 1).expect("source span"));
    source_compilation.successors[0] = Some(LinkTarget {
        linear: overlap.linear,
        mode_key: overlap.mode_key,
    });
    assert!(matches!(
        cpu.jit_direct.probe(source),
        BlockProbe::Interpret
    ));
    assert!(matches!(cpu.jit_direct.probe(source), BlockProbe::Compile));
    let source_id = cpu
        .jit_direct
        .install(&source_compilation)
        .expect("source installs");
    install_trivial(&mut cpu.jit_direct, overlap, 16);
    install_trivial(&mut cpu.jit_direct, unrelated, 16);
    let source_cell = cpu.jit_direct.link_cells[source_id.index()][0].clone();
    assert!(source_cell.linked());

    cpu.note_device_memory_write_range(0x70_000, 512);
    assert_eq!(cpu.jit_direct.len(), 3);
    assert!(source_cell.linked());

    cpu.note_device_memory_write_range(overlap.physical + 4, 1);
    assert_eq!(cpu.jit_direct.len(), 2);
    assert!(!source_cell.linked());
    assert!(matches!(
        cpu.jit_direct.probe(overlap),
        BlockProbe::Interpret
    ));
    assert!(matches!(cpu.jit_direct.probe(source), BlockProbe::Ready(_)));
    assert!(matches!(
        cpu.jit_direct.probe(unrelated),
        BlockProbe::Ready(_)
    ));

    let stats = cpu.jit_direct.take_stats();
    assert_eq!(stats.cache_resets, 0);
    assert_eq!(stats.unlinks, 1);
    assert_eq!(cpu.perf.device_write_ranges, 2);
    assert_eq!(cpu.perf.device_write_bytes, 513);
    assert_eq!(cpu.perf.device_write_code_hits, 1);
    assert_eq!(cpu.perf.device_write_coarse_resets, 0);
}
