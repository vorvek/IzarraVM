// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::atomic::Ordering;

use super::*;

use crate::jit::JitState;

/// Post-hoist constructor shim (Track C C1c-pre): the `NativeCodeWatch` now lives on
/// `JitState`, so this battery drives the cache through the SAME `JitState` wrapper surface
/// production uses (which carries the pre-hoist method signatures). Shadowing the
/// constructor names keeps every pre-existing call site below textually unchanged; field
/// and non-watch method access reaches the inner cache through `JitState`'s `Deref`.
struct BlockCache;

#[allow(clippy::new_ret_no_self)] // constructor-name shims, deliberately returning JitState
impl BlockCache {
    fn default() -> JitState {
        JitState::new(super::BlockCache::default())
    }

    fn new(decode_slot_count: usize) -> JitState {
        JitState::new(super::BlockCache::new(decode_slot_count))
    }

    fn with_entry_cap(entry_cap: usize) -> JitState {
        JitState::new(super::BlockCache::with_entry_cap(entry_cap))
    }

    fn arena_compaction_can_reclaim(live_blocks: usize, capacity: usize) -> bool {
        super::BlockCache::arena_compaction_can_reclaim(live_blocks, capacity)
    }
}

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
        fetch_lens,
        raw_clocks: 1,
        weighted_fp_clocks: 0,
        byte_reads: 0,
        word_reads: 0,
        dword_reads: 0,
        byte_stores: 0,
        word_stores: 0,
        dword_stores: 0,
        segment_layout: SegmentLayout::capture(&CpuGsw::default(), 0, 0, 0)
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
        imm_lanes: [NO_IMM_LANE; MAX_BLOCK_IMM_LANES],
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

fn reject(cache: &mut JitState, key: BlockKey, guest_len: usize) {
    cache.reject(RejectedSpan::new(key, guest_len).expect("rejected test span"));
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
fn install_trivial(cache: &mut JitState, key: BlockKey, guest_len: usize) -> BlockId {
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
fn install_dynamic_trivial(cache: &mut JitState, key: BlockKey) -> BlockId {
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
fn install_with_fetch_lens(cache: &mut JitState, key: BlockKey, fetch_lens: &[u8]) -> BlockId {
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
    let x87_addr = crate::AddrMode {
        segment: SegmentIndex::Ds,
        base: None,
        index: None,
        scale: 1,
        disp: 0,
        address_size: crate::AddressSize::Dword,
    };
    let x87_qword_read = slot(DirectKind::X87 {
        insn: NativeX87Insn::LoadF64 { addr: x87_addr },
        addr: Some(addr),
    });
    let x87_qword_write = slot(DirectKind::X87 {
        insn: NativeX87Insn::StoreF64 {
            addr: x87_addr,
            pop: false,
        },
        addr: Some(addr),
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
    // The x87 arm was entirely uncovered here before slice 39. A Qword access must land on the
    // DWORD lane, not the wildcard BYTE lane a genuine Word access would hit: this is the
    // regression pin for the explicit Qword arms added above the wildcards.
    assert_eq!(
        dynamic_counter_mask(&[x87_qword_read]),
        COUNTER_MODE13_DWORD_READ
    );
    assert_eq!(
        dynamic_counter_mask(&[x87_qword_write]),
        COUNTER_RAM_DWORD_WRITE | COUNTER_MODE13_DWORD_WRITE | COUNTER_MODE13_DIRTY
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
    assert_eq!(cache.retire_physical_range_for_test(key.physical, 1), 1);
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
    assert_eq!(cache.retire_physical_range_for_test(taken.physical, 1), 1);
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

    assert_eq!(cache.retire_physical_range_for_test(first.physical, 1), 1);
    assert!(cells[0].linked());
    assert!(cells[1].linked());
    assert_eq!(cache.retire_physical_range_for_test(second.physical, 1), 1);
    assert!(cells[0].linked());
    assert!(!cells[1].linked());
    assert_eq!(cache.retire_physical_range_for_test(third.physical, 1), 1);
    assert!(!cells[0].linked());

    assert_eq!(cache.retire_physical_range_for_test(source.physical, 1), 1);
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

    assert_eq!(
        cache.retire_physical_range_for_test(old_target.physical, 1),
        1
    );
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
fn dynamic_ret_pic_requires_a_matching_x87_top_and_spills_into_an_integer_target() {
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
    // A FLOAT source into an INTEGER target now binds on the dynamic path, and carries the same
    // spilling mark the static path uses: `emit_completed_dynamic_path` emits the boundary spill
    // that mark drives. This used to be refused as `LinkRefusal::DynamicFloatToInteger`.
    assert!(cache.bind_dynamic_successor(
        site_cell,
        integer.linear,
        integer.linear,
        integer.mode_key
    ));
    assert!(cache.link_cells[source_id.index()][0].is_spilling());

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

// Static-successor (Jmp/Jcc/Call/fallthrough) counterpart of the RET PIC test above: unlike the
// dynamic path, a static float source is allowed to link into an integer target, with the edge
// marked spilling so the emitted jump flushes x87 state before handing control over.
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn static_link_from_float_source_to_integer_target_is_permitted_and_marked_spilling() {
    let mut cache = BlockCache::default();
    let source = key(0x1000);
    let target = key(0x1100);

    let mut source_compilation =
        trivial_compilation(BlockSpan::new(source, 1, 1).expect("source span"));
    source_compilation.has_x87 = true;
    source_compilation.x87_entry_top = 2;
    source_compilation.x87_exit_top = 5;
    assert!(matches!(cache.probe(source), BlockProbe::Interpret));
    assert!(matches!(cache.probe(source), BlockProbe::Compile));
    let source_id = cache
        .install(&source_compilation)
        .expect("float source install");

    // Default trivial_compilation is has_x87 = false: an integer target. Its own
    // x87_entry_top/x87_exit_top stay at their default 0, a compile-time snapshot the
    // float-to-integer case never reads (see link_compatible's (true, false) arm).
    let target_id = install_trivial(&mut cache, target, 1);

    assert!(
        cache.try_link(source_id, 0, target_id),
        "a float source must be able to link to an integer target"
    );
    assert!(
        cache.link_cells[source_id.index()][0].is_spilling(),
        "a float-to-integer edge must be marked spilling"
    );
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn static_link_from_integer_source_to_float_target_goes_through_the_x87_pad() {
    let mut cache = BlockCache::default();
    let source = key(0x1000);
    let target = key(0x1100);

    let source_id = install_trivial(&mut cache, source, 1);

    let mut target_compilation =
        trivial_compilation(BlockSpan::new(target, 1, 1).expect("target span"));
    target_compilation.has_x87 = true;
    target_compilation.x87_entry_top = 3;
    target_compilation.x87_exit_top = 3;
    assert!(matches!(cache.probe(target), BlockProbe::Interpret));
    assert!(matches!(cache.probe(target), BlockProbe::Compile));
    let target_id = cache
        .install(&target_compilation)
        .expect("float target install");

    assert!(
        cache.try_link(source_id, 0, target_id),
        "an integer source now links to a float target through the shared x87 re-entry pad"
    );
    assert!(cache.link_cells[source_id.index()][0].linked());
    // The edge is integer-into-float, not float-into-integer, so nothing spills at the jump.
    assert!(!cache.link_cells[source_id.index()][0].is_spilling());
    // The pad guards this against the CPU's live TOP, so the cell must carry the target's baked
    // value and not the never-set sentinel.
    assert_eq!(
        cache.link_cells[source_id.index()][0].entry_top(),
        3,
        "the cell must carry the float target's baked entry TOP for the pad to guard against"
    );

    // The portal must send an INTEGER source to the pad and a FLOAT source to the body. Equal
    // fields here would mean the integer source jumps straight into a body whose x87 register
    // cache was never loaded.
    let index = target_id.index();
    let body = cache.block_portals[index].body.load(Ordering::Acquire);
    let integer_entry = cache.block_portals[index]
        .integer_entry
        .load(Ordering::Acquire);
    assert_ne!(body, 0);
    assert_ne!(
        integer_entry, body,
        "a float target must route an integer source through the pad, not into its body"
    );
    assert_eq!(
        Some(integer_entry),
        cache.x87_pad_address_if_built(),
        "integer_entry must be the shared pad"
    );
}

/// An INTEGER target publishes the same address in both fields, so the compile-time field
/// selection in `emit_completed_path` costs a pure integer chain nothing.
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn an_integer_target_publishes_the_same_address_in_both_portal_fields() {
    let mut cache = BlockCache::default();
    let target_id = install_trivial(&mut cache, key(0x2000), 1);
    let index = target_id.index();
    let body = cache.block_portals[index].body.load(Ordering::Acquire);
    assert_ne!(body, 0);
    assert_eq!(
        cache.block_portals[index]
            .integer_entry
            .load(Ordering::Acquire),
        body
    );
}

// A stale spilling flag would make a later float-to-float edge on the same slot spill and
// re-enter incorrectly, since the target-side float block never expects to be re-entered
// through its own prologue after a spill. This proves relinking clears it.
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn relinking_a_spilling_slot_to_a_matching_float_target_clears_the_stale_flag() {
    let mut cache = BlockCache::default();
    let source = key(0x1000);
    let integer_target = key(0x1100);
    let float_target = key(0x1200);

    let mut source_compilation =
        trivial_compilation(BlockSpan::new(source, 1, 1).expect("source span"));
    source_compilation.has_x87 = true;
    source_compilation.x87_entry_top = 3;
    source_compilation.x87_exit_top = 3;
    assert!(matches!(cache.probe(source), BlockProbe::Interpret));
    assert!(matches!(cache.probe(source), BlockProbe::Compile));
    let source_id = cache
        .install(&source_compilation)
        .expect("float source install");

    let integer_target_id = install_trivial(&mut cache, integer_target, 1);
    assert!(cache.try_link(source_id, 0, integer_target_id));
    assert!(cache.link_cells[source_id.index()][0].is_spilling());

    let mut float_target_compilation =
        trivial_compilation(BlockSpan::new(float_target, 1, 1).expect("float target span"));
    float_target_compilation.has_x87 = true;
    float_target_compilation.x87_entry_top = 3;
    float_target_compilation.x87_exit_top = 3;
    assert!(matches!(cache.probe(float_target), BlockProbe::Interpret));
    assert!(matches!(cache.probe(float_target), BlockProbe::Compile));
    let float_target_id = cache
        .install(&float_target_compilation)
        .expect("float target install");

    assert!(cache.try_link(source_id, 0, float_target_id));
    assert!(
        cache.link_cells[source_id.index()][0].linked(),
        "the relink to the matching float target must succeed"
    );
    assert!(
        !cache.link_cells[source_id.index()][0].is_spilling(),
        "relinking to a matching float target must not leave the old spilling flag set"
    );
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
        .revalidate_translation(source)
        .expect("source revalidation");
    assert!(!cache.bind_dynamic_successor(
        site_cell,
        target.linear,
        target.linear,
        target.mode_key
    ));
    cache
        .revalidate_translation(target)
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
        .revalidate_translation(source)
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
        .revalidate_translation(target)
        .expect("same mapping revalidation");
    assert!(cells[0].linked());
    assert_eq!(cache.arena.as_ref().expect("arena").used_slots(), slots);
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn decode_slot_suspension_requires_revalidation() {
    let mut cache = BlockCache::default();
    let block = key(0x4100);
    let id = install_trivial(&mut cache, block, 1);
    assert!(cache.is_link_visible(id));

    let slot = block.linear as usize & cache.decode_slot_mask;
    cache.suspend_decode_slot(slot);
    assert!(!cache.is_link_visible(id));
    assert!(matches!(cache.probe(block), BlockProbe::Ready(hit) if hit == id));

    cache
        .revalidate_translation(block)
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

    for _ in 0..16 {
        assert_eq!(cache.suspend_decode_slot(1), 1);
        assert!(!cache.is_link_visible(source_id));
        assert_eq!(cache.block_link_epochs, graph_epochs);
        cache
            .revalidate_translation(source)
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

    assert_eq!(cache.retire_physical_range_for_test(old.physical, 4), 1);
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

    assert_eq!(cache.retire_physical_range_for_test(source.physical, 1), 1);
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
    assert_eq!(cache.retire_physical_range_for_test(dead.physical, 1), 1);
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
        .revalidate_translation(source)
        .expect("source revalidation");
    assert!(!source_cell.linked());
    cache
        .revalidate_translation(target)
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
        .revalidate_translation(hidden)
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

    assert_eq!(cache.retire_physical_range_for_test(0x20_02f, 1), 1);
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

    assert_eq!(cache.retire_physical_range_for_test(first.physical, 1), 1);
    assert!(
        cache.range_hits_compiled_code(first.physical, 1),
        "the neighboring block still owns the shared 16-byte watch"
    );

    assert_eq!(cache.retire_physical_range_for_test(second.physical, 1), 1);
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

    assert_eq!(cache.retire_physical_range_for_test(old.physical, 1), 1);
    assert!(!cache.range_hits_compiled_code(old.physical, 1));

    install_trivial(&mut cache, old, 8);
    assert!(cache.range_hits_compiled_code(old.physical, 1));
    assert_eq!(cache.retire_physical_range_for_test(old.physical, 1), 1);
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

    assert_eq!(cache.retire_physical_range_for_test(0x30_084, 2), 2);
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

    assert_eq!(cache.retire_physical_range_for_test(0x40_010, 1), 2);
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

    assert_eq!(cache.retire_physical_range_for_test(0x4fff, 2), 2);
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

// ---- G1 SMC heat map ----

#[test]
fn smc_heat_crosses_the_threshold_within_one_epoch() {
    let mut heat = SmcHeatMap::default();
    let phys = 0x2_1234;
    for _ in 0..SMC_HEAT_THRESHOLD - 1 {
        assert_eq!(heat.bump(phys, 4, 0), 0);
    }
    assert!(!heat.chunk_hot(phys, 0));
    // The threshold-th bump crosses and reports exactly one newly-hot chunk.
    assert_eq!(heat.bump(phys, 4, 0), 1);
    assert!(heat.chunk_hot(phys, 0));
    // Saturated bumps neither overflow nor re-report the chunk.
    assert_eq!(heat.bump(phys, 4, 0), 0);
    assert!(heat.chunk_hot(phys, 0));
}

#[test]
fn smc_heat_ages_out_across_epochs_and_reenables_admission() {
    let mut heat = SmcHeatMap::default();
    let phys = 0x2_1234;
    for _ in 0..SMC_HEAT_THRESHOLD {
        heat.bump(phys, 4, 7);
    }
    assert!(heat.chunk_hot(phys, 7));
    // A later epoch reads the stale stamp as zero, so admission is re-enabled, and a fresh bump
    // starts that epoch's count from one rather than inheriting the saturated older count.
    assert!(!heat.chunk_hot(phys, 8));
    assert_eq!(heat.bump(phys, 4, 8), 0);
    assert!(!heat.chunk_hot(phys, 8));
}

#[test]
fn smc_heat_span_hot_only_flags_overlapping_chunks() {
    let mut heat = SmcHeatMap::default();
    let hot = 0x2_1234; // 16-byte chunk 0x2_123
    for _ in 0..SMC_HEAT_THRESHOLD {
        heat.bump(hot, 1, 0);
    }
    assert!(heat.span_hot(0x2_1230, 8, 0));
    assert!(!heat.span_hot(0x2_1220, 8, 0));
    assert!(!heat.span_hot(0x2_1240, 8, 0));
    // A wide span reaching across the boundary into the hot chunk still demotes.
    assert!(heat.span_hot(0x2_1228, 16, 0));
}

#[test]
fn smc_heat_threshold_setter_and_clear() {
    let mut heat = SmcHeatMap::default();
    heat.set_threshold(2);
    let phys = 0x2_1234;
    heat.bump(phys, 4, 0);
    assert!(!heat.chunk_hot(phys, 0));
    heat.bump(phys, 4, 0);
    assert!(heat.chunk_hot(phys, 0));
    heat.clear();
    assert!(!heat.chunk_hot(phys, 0));
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn reset_storage_drops_smc_heat_but_incremental_invalidation_keeps_it() {
    // The hoisted-map reset coupling: the cache signals its storage resets through
    // heat_resets, and the map owner (CpuGsw::sync_smc_heat) clears on observing one.
    let mut cpu = CpuGsw::default();
    let phys = 0x60_010;
    for _ in 0..SMC_HEAT_THRESHOLD {
        cpu.jit_direct.smc_heat.bump(phys, 4, 0);
    }
    // Installing then draining a block (an incremental invalidation) leaves heat intact:
    // no storage reset happened, so a sync is a no-op.
    install_trivial(&mut cpu.jit_direct, BlockKey::new(0x1000, phys, 7), 16);
    cpu.jit_direct.retire_physical_range_for_test(phys, 4);
    cpu.sync_smc_heat();
    assert!(
        cpu.jit_direct.smc_heat.chunk_hot(phys, 0),
        "heat survives invalidation"
    );
    // A full reset (arena pressure / clear) is the only wipe: the non-empty clear routes
    // through reset_storage, which bumps the coupling counter.
    install_trivial(&mut cpu.jit_direct, BlockKey::new(0x1000, phys, 7), 16);
    cpu.jit_direct.clear();
    cpu.sync_smc_heat();
    assert!(!cpu.jit_direct.smc_heat.chunk_hot(phys, 0));
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn hoisted_heat_survives_a_foreign_reset_and_drops_on_the_owned_one() {
    // Reset-coupling invariant (Track C C1a-pre): only the ACTIVE backend cache reset clears
    // the shared map. A dormant backend resetting its own EMPTY cache must not erase the live
    // backend demotion evidence: the map is keyed to the OWNED cache counter, so a foreign
    // cache reset moves no counter the owner observes.
    let mut cpu = CpuGsw::default();
    let phys = 0x60_010;
    for _ in 0..SMC_HEAT_THRESHOLD {
        cpu.jit_direct.smc_heat.bump(phys, 4, 0);
    }
    // An unrelated (inactive) cache resets its empty storage; the shared map is untouched.
    let mut foreign = BlockCache::default();
    foreign.clear();
    cpu.sync_smc_heat();
    assert!(
        cpu.jit_direct.smc_heat.chunk_hot(phys, 0),
        "an inactive backend reset must not clear the shared map"
    );
    // The empty-cache clear FAST PATH on the owned cache still drops heat, exactly as it did
    // when the map lived inside the cache.
    cpu.jit_direct.clear();
    cpu.sync_smc_heat();
    assert!(!cpu.jit_direct.smc_heat.chunk_hot(phys, 0));
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn smc_heat_accrues_only_on_actual_code_invalidation() {
    let mut cpu = CpuGsw::default();
    let key = BlockKey::new(0x1000, 0x60_010, 7);
    // A churn loop: install, invalidate, repeat. Each note_code_write kills the compiled block
    // (an actual invalidation), so the entry chunk heats and crosses the threshold in this epoch.
    for _ in 0..u32::from(SMC_HEAT_THRESHOLD) {
        install_trivial(&mut cpu.jit_direct, key, 16);
        cpu.note_code_write(key.physical, 4);
    }
    assert!(cpu.jit_direct.smc_heat.chunk_hot(key.physical, 0));
    assert_eq!(cpu.perf.smc_heat_chunks_hot, 1);

    // A data byte sharing the same 16-byte chunk as cold code (the block watches only its own
    // bytes) invalidates nothing, so it never heats the chunk and the block survives.
    let mut cold = CpuGsw::default();
    let data_key = BlockKey::new(0x2000, 0x61_000, 7);
    install_trivial(&mut cold.jit_direct, data_key, 4); // watches 0x61_000..0x61_003
    for _ in 0..8 {
        cold.note_code_write(0x61_004, 1); // same chunk 0x61_00, unwatched byte
    }
    assert!(!cold.jit_direct.smc_heat.chunk_hot(data_key.physical, 0));
    assert_eq!(cold.perf.smc_heat_chunks_hot, 0);
    assert!(matches!(
        cold.jit_direct.probe(data_key),
        BlockProbe::Ready(_)
    ));
}

/// C1c-pre pin (design section 2.5 / D-C1c.1): the hoist moves the watch's OWNER, never the
/// watch itself. The published `table_base()` Direct's emitted code bakes must stay one
/// stable address across every hoist-era cache operation (install, reject, per-range
/// invalidation, wholesale clear, storage reset), and the acquire/release/clear semantics
/// must behave exactly as they did when `BlockCache` owned the instance.
#[test]
fn hoisted_code_watch_keeps_the_table_base_and_the_watch_semantics() {
    let mut cache = BlockCache::default();
    let base = cache.native_code_watch_table();
    assert_ne!(base, 0);

    // Install acquires; the block's chunks read watched through the shared instance.
    let installed = key(0x400);
    install_trivial(&mut cache, installed, 4);
    assert_eq!(cache.native_code_watch_table(), base);
    assert!(cache.range_hits_compiled_code(installed.physical, 4));

    // Reject (after the Seen transition) acquires on the same shared instance.
    let rejected = key(0x800);
    assert!(matches!(cache.probe(rejected), BlockProbe::Interpret));
    assert!(matches!(cache.probe(rejected), BlockProbe::Compile));
    reject(&mut cache, rejected, 4);
    assert!(matches!(cache.probe(rejected), BlockProbe::Rejected));
    assert_eq!(cache.native_code_watch_table(), base);
    assert!(cache.range_hits_compiled_code(rejected.physical, 4));

    // Per-range invalidation releases exactly the dead owner's chunks.
    assert_eq!(
        cache.retire_physical_range_for_test(installed.physical, 1),
        1
    );
    assert!(!cache.range_hits_compiled_code(installed.physical, 4));
    assert!(cache.range_hits_compiled_code(rejected.physical, 4));
    assert_eq!(cache.native_code_watch_table(), base);

    // Wholesale clear drops the remaining watch bits; the table allocation survives.
    cache.clear();
    assert!(!cache.range_hits_compiled_code(rejected.physical, 4));
    assert_eq!(cache.native_code_watch_table(), base);

    // The whole JitState (the new owner) moving does not move the published table either:
    // the box allocations inside the watch are what the emitted code points at.
    let mut moved = cache;
    assert_eq!(moved.native_code_watch_table(), base);
    let reinstalled = key(0xc00);
    install_trivial(&mut moved, reinstalled, 4);
    assert!(moved.range_hits_compiled_code(reinstalled.physical, 4));
    assert_eq!(moved.native_code_watch_table(), base);
}

/// The backend's prefix gate is relative to the code segment's default size, because `decode`
/// computes `operand_size = default_32 XOR operand_size_override`.
///
/// The first half is a REGRESSION pin: under CS.D = 1 the mode-relative form must agree exactly
/// with the hard-coded form it replaced, or byte identity on the pinned corpus moves. The second
/// half is the part that was broken: under CS.D = 0 the old form rejected BOTH arms, so every
/// 16-bit instruction was refused as PrefixesUnsupported before the classifier was ever asked.
/// Nothing reaches it today because `key_for` refuses on `!d`; this pins it ahead of that work.
#[test]
fn prefix_gate_is_relative_to_the_code_segment_default_size() {
    let none = Prefixes::default();
    let oso = Prefixes {
        operand_size_override: true,
        ..Prefixes::default()
    };

    // CS.D = 1: unprefixed is Dword, 0x66 makes it Word. Unchanged from before.
    assert!(prefixes_supported_for(none, OperandSize::Dword, true));
    assert!(prefixes_supported_for(oso, OperandSize::Word, true));
    assert!(!prefixes_supported_for(oso, OperandSize::Dword, true));
    assert!(!prefixes_supported_for(none, OperandSize::Word, true));

    // CS.D = 0: the mapping inverts. Both of these were false before and are the whole point.
    assert!(prefixes_supported_for(none, OperandSize::Word, false));
    assert!(prefixes_supported_for(oso, OperandSize::Dword, false));
    assert!(!prefixes_supported_for(none, OperandSize::Dword, false));
    assert!(!prefixes_supported_for(oso, OperandSize::Word, false));

    // Any other prefix is still unsupported in both widths.
    let seg = Prefixes {
        segment_override: Some(SegmentIndex::Es),
        ..Prefixes::default()
    };
    for d in [false, true] {
        for size in [OperandSize::Word, OperandSize::Dword] {
            assert!(
                !prefixes_supported_for(seg, size, d),
                "segment override d={d}"
            );
        }
    }
}

/// A Word-size relative branch masks its target to 16 bits (`relative_jump` computes
/// `(eip + rel) & operand_size.mask()`), while the emitted form bakes an unmasked delta. The
/// compile loop expresses "the mask is a no-op" by clamping the limit it hands
/// `static_control_target_within_limit` to 0xFFFF at Word size.
///
/// This pins the predicate itself. The clamp is `cs.limit.min(0xFFFF)`, so the assertions below
/// are written against the already-clamped value the call site produces.
///
/// **The Dword rows are the load-bearing half.** Every Jcc fixture in this crate entries at a
/// tiny EIP (0x100, 0x101, 0x500), so a clamp wrongly applied at Dword would pass all of them
/// and reach the pinned corpus undetected. The `entry_eip` above 0xFFFF rows are the only thing
/// in the tree that would notice.
#[test]
fn word_size_control_targets_are_refused_above_the_sixteen_bit_wrap() {
    let jcc = |taken_delta| DirectKind::Jcc {
        condition: 0x5,
        taken_delta,
    };
    let flat = control_target_limit(OperandSize::Dword, u32::MAX);
    let word = control_target_limit(OperandSize::Word, u32::MAX);

    // Dword, flat limit: unchanged behaviour, including well above the wrap. If a clamp leaks
    // into the Dword path these are what fail.
    assert!(static_control_target_within_limit(
        jcc(0x40),
        0x1_0100,
        flat
    ));
    assert!(static_control_target_within_limit(
        jcc(0x10_0000),
        0x20_0000,
        flat
    ));

    // Word: refused once the target crosses the wrap, admitted below it.
    assert!(static_control_target_within_limit(jcc(0x40), 0x100, word));
    assert!(!static_control_target_within_limit(
        jcc(0x40),
        0x1_0100,
        word
    ));
    // Exactly at the boundary, both sides. `<=` is the correct comparison.
    assert!(static_control_target_within_limit(jcc(0), 0xFFFF, word));
    assert!(!static_control_target_within_limit(jcc(1), 0xFFFF, word));

    // A backward branch is stored as a wrapped u32. Where the architectural result would have
    // wrapped below zero the guard refuses it, which is conservative and never wrong; where it
    // lands at or below the wrap from a high entry it is admitted, and the mask is genuinely a
    // no-op there.
    assert!(!static_control_target_within_limit(
        jcc(0u32.wrapping_sub(0x200)),
        0x100,
        word
    ));
    assert!(static_control_target_within_limit(
        jcc(0u32.wrapping_sub(0x1_0000)),
        0x1_0100,
        word
    ));

    // Jmp and Call ride the same predicate, so the allowlist work inherits the guard.
    assert!(!static_control_target_within_limit(
        DirectKind::Jmp { target_delta: 0x40 },
        0x1_0100,
        word
    ));
    assert!(!static_control_target_within_limit(
        DirectKind::Call {
            return_delta: 0x5,
            target_delta: 0x40,
        },
        0x1_0100,
        word
    ));
    // The 16-bit call rides the same predicate. Without this row the assertion above keeps
    // passing while covering nothing about the kind that is actually admitted at Word size.
    assert!(!static_control_target_within_limit(
        DirectKind::Call16 {
            return_delta: 0x5,
            target_delta: 0x40,
        },
        0x1_0100,
        word
    ));
    assert!(static_control_target_within_limit(
        DirectKind::Call16 {
            return_delta: 0x5,
            target_delta: 0x40,
        },
        0x100,
        word
    ));

    // A kind with no control target is unaffected in either width.
    assert!(static_control_target_within_limit(
        DirectKind::Pop { dst: 0 },
        0x1_0100,
        word
    ));
}

/// In real mode the clamp is a no-op, because `cs.limit` is already 0xFFFF. Recorded so nobody
/// concludes the guard changes real-mode behaviour: the 16-bit mask was never observable there,
/// which is why this trap survived until the allowlist was about to open.
#[test]
fn the_word_control_clamp_is_a_no_op_at_a_real_mode_limit() {
    // Real mode: already 0xFFFF, so the clamp changes nothing in either width.
    assert_eq!(control_target_limit(OperandSize::Word, 0xFFFF), 0xFFFF);
    assert_eq!(control_target_limit(OperandSize::Dword, 0xFFFF), 0xFFFF);

    // Flat 32-bit: Word narrows to the wrap, Dword is untouched. Both directions are pinned
    // because a clamp leaking into Dword would pass every Jcc fixture in this crate, all of
    // which entry far below 0xFFFF.
    assert_eq!(control_target_limit(OperandSize::Word, u32::MAX), 0xFFFF);
    assert_eq!(control_target_limit(OperandSize::Dword, u32::MAX), u32::MAX);

    // A limit already below the wrap is never widened.
    assert_eq!(control_target_limit(OperandSize::Word, 0x0FFF), 0x0FFF);
    assert_eq!(control_target_limit(OperandSize::Dword, 0x0FFF), 0x0FFF);
}

/// The eight 16-bit addressing modes now classify, and they arrive in the shape `DirectAddr`
/// wants: base/index pairs at scale 1, with SS selected for the BP forms.
///
/// This is the classifier half of the slice. It cannot be an end-to-end compile fixture: the
/// only route to a 16-bit address size in a 32-bit code segment is a 0x67 prefix, which the
/// prefix gate refuses, so no admitted block can carry one until 16-bit code admission is
/// flipped.
#[test]
fn sixteen_bit_addressing_modes_classify_with_the_interpreter_shape() {
    let word_addr = |base, index, segment, disp: i32| crate::AddrMode {
        segment,
        base,
        index,
        scale: 1,
        disp,
        address_size: AddressSize::Word,
    };

    // The eight modes, as `parse_16bit_address` builds them: bx=3, bp=5, si=6, di=7.
    let cases: [(Option<u8>, Option<u8>, SegmentIndex); 8] = [
        (Some(3), Some(6), SegmentIndex::Ds), // bx+si
        (Some(3), Some(7), SegmentIndex::Ds), // bx+di
        (Some(5), Some(6), SegmentIndex::Ss), // bp+si
        (Some(5), Some(7), SegmentIndex::Ss), // bp+di
        (None, Some(6), SegmentIndex::Ds),    // si
        (None, Some(7), SegmentIndex::Ds),    // di
        (Some(5), None, SegmentIndex::Ss),    // bp
        (Some(3), None, SegmentIndex::Ds),    // bx
    ];
    for (base, index, segment) in cases {
        let lowered = classify::direct_addr(word_addr(base, index, segment, -2))
            .expect("every 16-bit mode must lower");
        assert_eq!(lowered.base, base);
        assert_eq!(lowered.index, index);
        assert_eq!(lowered.scale, 1);
        assert_eq!(
            lowered.segment, segment,
            "a BP form addresses SS, and getting this wrong is a wrong-memory-read"
        );
        // The displacement is sign-extended to 32 bits, which is what the interpreter carries
        // too. Both sides sum in 32 bits and mask, and addition is congruent mod 2^16.
        assert_eq!(lowered.disp, 0xffff_fffe);
    }

    // The disp16-only form, and a Dword address for contrast.
    let disp_only = classify::direct_addr(word_addr(None, None, SegmentIndex::Ds, 0x1234))
        .expect("disp16-only must lower");
    assert_eq!(disp_only.base, None);
    assert_eq!(disp_only.index, None);
    assert_eq!(disp_only.disp, 0x1234);

    // A scale other than 1, 2, 4 or 8 is still refused, at either address size.
    let mut bad_scale = word_addr(Some(3), Some(6), SegmentIndex::Ds, 0);
    bad_scale.scale = 3;
    assert!(classify::direct_addr(bad_scale).is_none());
}

/// The emitter masks a ModRM-derived effective address at 64K when the block's address size is
/// 16-bit, and does not when it is 32-bit.
///
/// This is the closest thing to an executed gate S3 can have. No admitted block can carry a
/// 16-bit address until admission flips, so the address former is exercised directly instead,
/// with the same operand under both block properties.
///
/// The mask lives in THIS function rather than in the segmented helper on purpose: LEA consumes
/// an effective address without ever reaching a segment, so a mask placed downstream would miss
/// it. Two callers, one mask, no per-caller obligation.
#[test]
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
fn a_sixteen_bit_effective_address_is_masked_and_a_thirty_two_bit_one_is_not() {
    let addr = DirectAddr {
        segment: SegmentIndex::Ds,
        base: Some(3),
        index: Some(6),
        scale: 1,
        disp: 4,
    };

    let mut unmasked = Encoder::new();
    emit::emit_effective_address(&mut unmasked, addr, emit::AddressWrap::None);
    let unmasked = unmasked.finish();

    let mut masked = Encoder::new();
    emit::emit_effective_address(&mut masked, addr, emit::AddressWrap::Word);
    let masked = masked.finish();

    let mut probe = Encoder::new();
    probe.and_r32_imm32(Reg::RAX, 0xFFFF);
    let mask = probe.finish();

    assert_eq!(
        masked.len(),
        unmasked.len() + mask.len(),
        "exactly one mask instruction"
    );
    assert_eq!(
        &masked[..unmasked.len()],
        &unmasked[..],
        "same address math"
    );
    assert_eq!(&masked[unmasked.len()..], &mask[..], "then the 64K mask");
}

/// `CompiledBlock` is copied out of `BlockCache::block()` several times per Direct entry
/// (probe, the `run_direct_block` argument, and the pre-entry re-resolve), so its size is
/// memcpy traffic multiplied by ~47M entries in a Quake/586 run. Fields that are not read
/// on a uniform-fetch entry belong in a parallel `BlockCache` lane, not in the copy.
#[test]
fn compiled_block_stays_small_enough_to_copy_per_entry() {
    assert_eq!(
        core::mem::size_of::<CompiledBlock>(),
        120,
        "CompiledBlock size changed; if a field was added, check it is actually read on the \
         uniform-fetch entry path before letting it ride every per-entry copy"
    );
}

/// Every state an entry can be in maps to exactly one `UnboundTarget`, and the variant list is
/// complete. The classes exist to close on `jit_direct_unresolved_static_unbound`, so a state
/// that fell through to a default (or two states sharing a class) would silently break the
/// attribution table the linking campaign is steered by.
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn unbound_target_classes_are_exhaustive() {
    let mut labels: Vec<&str> = UnboundTarget::ALL.iter().map(|k| k.label()).collect();
    let distinct = {
        labels.sort_unstable();
        labels.dedup();
        labels.len()
    };
    assert_eq!(
        distinct,
        UnboundTarget::COUNT,
        "class labels must be unique"
    );

    let mut cache = BlockCache::default();

    // Absent: never probed.
    let cold = key(0x1000);
    assert_eq!(cache.classify_unbound_target(cold), UnboundTarget::Absent);

    // Seen: probed, not yet compiled.
    assert!(matches!(cache.probe(cold), BlockProbe::Interpret));
    assert_eq!(cache.classify_unbound_target(cold), UnboundTarget::Seen);

    // Dormant, split by reason. `SpanHot` is the heat lane; the other three share the residual.
    let heat = key(0x1100);
    assert!(matches!(cache.probe(heat), BlockProbe::Interpret));
    cache.dormant(heat, DormantReason::SpanHot);
    assert_eq!(
        cache.classify_unbound_target(heat),
        UnboundTarget::DormantHeat
    );
    let other = key(0x1200);
    assert!(matches!(cache.probe(other), BlockProbe::Interpret));
    cache.dormant(other, DormantReason::CompileRetry);
    assert_eq!(
        cache.classify_unbound_target(other),
        UnboundTarget::DormantOther
    );

    // Rejected.
    let rejected = key(0x1300);
    assert!(matches!(cache.probe(rejected), BlockProbe::Interpret));
    cache.reject(RejectedSpan {
        key: rejected,
        guest_len: 1,
    });
    assert_eq!(
        cache.classify_unbound_target(rejected),
        UnboundTarget::Rejected
    );

    // Compiled and live, then retired out from under the same entry.
    let compiled = key(0x1400);
    install_trivial(&mut cache, compiled, 1);
    assert_eq!(
        cache.classify_unbound_target(compiled),
        UnboundTarget::Compiled
    );
    assert_eq!(
        cache.retire_physical_range_for_test(compiled.physical, 1),
        1
    );
    assert_eq!(
        cache.classify_unbound_target(compiled),
        UnboundTarget::Absent,
        "a retired range drops the entry entirely; CompiledRetired is the slot-reuse case"
    );
}

/// The three link-clear causes account for every cleared link: their sum equals the aggregate
/// `BlockCacheStats::unlinks` that feeds `jit_direct_links_cleared`. The aggregate is fed
/// independently rather than derived, so this is the cross-check that they have not drifted --
/// a new unlink site that forgets its cause would show up here as a shortfall.
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn link_clear_causes_close_on_the_aggregate() {
    let mut cache = BlockCache::default();
    let source = key(0x1000);
    let first = key(0x1100);
    let second = key(0x1200);
    let source_id = install_trivial(&mut cache, source, 1);
    let first_id = install_trivial(&mut cache, first, 1);
    let second_id = install_trivial(&mut cache, second, 1);

    // Replace: the same cell relinked to a different target.
    assert!(cache.try_link(source_id, 0, first_id));
    assert!(cache.try_link(source_id, 0, second_id));
    assert_eq!(
        cache.stalls.links_cleared[LinkClearCause::Replaced as usize],
        1
    );

    // Retire: the linked target goes away.
    assert_eq!(cache.retire_physical_range_for_test(second.physical, 1), 1);
    assert_eq!(
        cache.stalls.links_cleared[LinkClearCause::Retired as usize],
        1
    );

    // Flush: a translation invalidation drops the cell while the blocks stay compiled. This is
    // the site the fixtures actually spend their clears on, so it gets its own cause.
    let third = key(0x1300);
    let third_id = install_trivial(&mut cache, third, 1);
    assert!(cache.try_link(source_id, 0, third_id));
    cache.invalidate_translation();
    assert_eq!(
        cache.stalls.links_cleared[LinkClearCause::Flushed as usize],
        1
    );
    assert_eq!(
        cache.stalls.links_cleared[LinkClearCause::Reset as usize],
        0,
        "a translation flush must not be attributed to the cache-wide reset lane"
    );
    assert!(
        cache.len() > 0,
        "a flush leaves the blocks compiled; only the links go"
    );

    // Reset: the cache-wide drop, a separate site from the flush above. Needs a FRESH pair: the
    // flush bumped the link epoch, so every block installed before it now refuses to relink on
    // `LinkRefusal::StaleEpoch` until root dispatch republishes it.
    let fourth = key(0x1400);
    let fifth = key(0x1500);
    let fourth_id = install_trivial(&mut cache, fourth, 1);
    let fifth_id = install_trivial(&mut cache, fifth, 1);
    assert!(cache.try_link(fourth_id, 0, fifth_id));
    cache.clear();
    assert_eq!(
        cache.stalls.links_cleared[LinkClearCause::Reset as usize],
        1
    );

    // All FOUR sites, not three: deleting any single increment above must break the sum.
    let causes = cache.stalls.links_cleared;
    assert!(
        causes.iter().all(|&n| n > 0),
        "every cause site must be exercised by this fixture, got {causes:?}"
    );
    let stats = cache.take_stats();
    assert_eq!(
        causes.iter().sum::<u64>(),
        stats.unlinks,
        "the cause split must sum to the aggregate it attributes"
    );
}
